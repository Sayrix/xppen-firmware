//! Native tablet operations, workflow request dispatch and session logging.
//! Device access uses the platform transport; progress is emitted as typed events.
use crate::{
    Result,
    image::{self, Firmware},
    platform_hid::{self, HidReports, OperationLock},
    progress::{Progress, Stage},
    protocol::Bootloader,
    utility::Runner,
    workflow,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub struct NativeDevice<F: FnMut(&str), P: FnMut(Progress)> {
    _lock: OperationLock,
    log: fs::File,
    on_output: F,
    on_progress: P,
    boot: Option<Bootloader<HidReports>>,
    active_stage: Option<Stage>,
}

impl<F: FnMut(&str), P: FnMut(Progress)> NativeDevice<F, P> {
    pub fn new(log: &Path, on_output: F, on_progress: P) -> Result<Self> {
        Ok(Self {
            _lock: OperationLock::acquire()?,
            log: OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(log)
                .map_err(|e| e.to_string())?,
            on_output,
            on_progress,
            boot: None,
            active_stage: None,
        })
    }

    fn emit(&mut self, text: &str) -> Result<()> {
        self.log
            .write_all(text.as_bytes())
            .map_err(|e| format!("Cannot save operation log: {e}"))?;
        (self.on_output)(text);
        Ok(())
    }

    fn ensure_boot(&mut self) -> Result<()> {
        if self.boot.is_none() {
            let device = platform_hid::target()?;
            if device.pid != 0xfff1 {
                return Err("Tablet is not in the supported FFF1 bootloader".into());
            }
            self.boot = Some(Bootloader::new(HidReports::open(&device)?)?);
        }
        Ok(())
    }

    fn boot(&mut self) -> Result<&mut Bootloader<HidReports>> {
        self.ensure_boot()?;
        self.boot
            .as_mut()
            .ok_or_else(|| "Bootloader unavailable".into())
    }

    fn info(&mut self) -> Result<String> {
        let device = platform_hid::target()?;
        if device.pid == 0x0930 {
            self.boot = None;
            return Ok(format!(
                "Runtime device: {} [28BD:0930]\nIdentity: manufacturer={} serial={}\n",
                device.product, device.manufacturer, device.serial
            ));
        }
        let boot = self.boot()?;
        boot.ready()?;
        let state = boot.state()?;
        let vectors = boot.read_bytes(image::APP_OFFSET, 8, |_| {})?;
        let sp = u32::from_le_bytes(vectors[..4].try_into().map_err(|_| "Short SP")?);
        let reset = u32::from_le_bytes(vectors[4..].try_into().map_err(|_| "Short reset")?);
        let sp_valid = if sp == 0x20000ba8 { "valid" } else { "invalid" };
        let reset_valid = if reset == 0x08003969 {
            "valid"
        } else {
            "invalid"
        };
        Ok(format!(
            "Bootloader: {} [28BD:FFF1], IC 902, framed\nBoot state: ready (status 0x01, state 0x00)\nRuntime VID:PID: 28BD:0930\nFlash size: 64 KiB\nFlash range: 0x08000000 .. 0x0800FFFF\nPersistent boot state: {state}\nApplication SP: 0x{sp:08X} ({sp_valid})\nApplication reset: 0x{reset:08X} ({reset_valid})\n",
            device.product
        ))
    }

    fn dump(&mut self, directory: &Path) -> Result<String> {
        // Refuse overwriting before any device read; no resume or cached bytes.
        fs::create_dir(directory)
            .map_err(|e| format!("Cannot create fresh backup directory: {e}"))?;
        self.ensure_boot()?;
        self.emit("Reading full flash, pass 1 of 2.\n")?;
        let boot = self.boot.as_mut().ok_or("Bootloader unavailable")?;
        let on_progress = &mut self.on_progress;
        let first = boot.read_bytes(0, image::FLASH_SIZE, |completed| {
            on_progress(Progress::Advanced {
                stage: Stage::ReadBackup,
                completed,
                total: image::FLASH_SIZE * 2,
            });
        })?;
        // Save the real first pass even if a later read fails. It is explicitly
        // raw/unverified; only the workflow publishes verified-full-backup.bin.
        workflow::save_new(&directory.join("ic902-main.bin"), &first)?;
        self.emit("Reading full flash, pass 2 of 2.\n")?;
        let boot = self.boot.as_mut().ok_or("Bootloader unavailable")?;
        let on_progress = &mut self.on_progress;
        let second = boot.read_bytes(0, image::FLASH_SIZE, |completed| {
            on_progress(Progress::Advanced {
                stage: Stage::ReadBackup,
                completed: image::FLASH_SIZE + completed,
                total: image::FLASH_SIZE * 2,
            });
        })?;
        if first != second {
            return Err("Physical double-read mismatch; backup is unverified".into());
        }
        let local = image::crc16(&first);
        let device = self.boot()?.range_crc(image::BASE, image::FLASH_SIZE)?;
        if local != device {
            return Err("Full backup CRC mismatch".into());
        }
        Ok(format!(
            "Local CRC16/XMODEM: 0x{local:04X}\nDevice range CRC: 0x{device:04X}\nPhysical double-read: matched\n"
        ))
    }

    fn execute(&mut self, command: Command) -> Result<String> {
        match command {
            Command::Info => self.info(),
            Command::Dfu => {
                self.boot = None;
                let device = platform_hid::target()?;
                platform_hid::enter_dfu(&device)?;
                Ok("DFU descriptor request sent once; waiting for re-enumeration.\n".into())
            }
            Command::Leave => {
                self.boot()?.finalize()?;
                self.boot = None;
                Ok("FINALIZE submitted once; waiting for normal-mode reconnection.\n".into())
            }
            Command::Crc { address, length } => {
                let crc = self.boot()?.range_crc(address, length)?;
                Ok(format!(
                    "CRC16/XMODEM(0x{address:08X}, {length}) = 0x{crc:04X}\n"
                ))
            }
            Command::Dump(directory) => self.dump(&directory),
            Command::Update(path) => {
                let firmware = Firmware::load(&path)?;
                self.ensure_boot()?;
                self.emit(&format!(
                    "Native application write: {} bytes, SHA256 {}\n",
                    firmware.code.len(),
                    firmware.hash
                ))?;
                let boot = self.boot.as_mut().ok_or("Bootloader unavailable")?;
                let on_progress = &mut self.on_progress;
                boot.program(&firmware.code, |completed| {
                    on_progress(Progress::Advanced {
                        stage: Stage::WriteFirmware,
                        completed,
                        total: firmware.code.len(),
                    });
                })?;
                Ok("Application programmed; left in DFU for independent verification.\n".into())
            }
        }
    }
}

impl<F: FnMut(&str), P: FnMut(Progress)> Runner for NativeDevice<F, P> {
    fn progress(&mut self, event: Progress) {
        match event {
            Progress::Started(stage) => self.active_stage = Some(stage),
            Progress::Finished(stage) | Progress::Failed(stage)
                if self.active_stage == Some(stage) =>
            {
                self.active_stage = None
            }
            _ => {}
        }
        (self.on_progress)(event);
    }
    fn note(&mut self, text: &str) {
        let _ = self.emit(&format!("\n{text}\n"));
    }
    fn run(&mut self, args: &[String]) -> Result<String> {
        let command = Command::parse(args)?;
        self.emit(&format!("\nNATIVE {args:?}\n"))?;
        let result = self.execute(command);
        match &result {
            Ok(output) => self.emit(output)?,
            Err(error) => {
                // Drop a failed transport instead of replaying any command.
                // The workflow independently prevents exit after attempted writes.
                self.boot = None;
                self.emit(&format!("ERROR: {error}\n"))?;
            }
        }
        self.log.sync_all().map_err(|e| e.to_string())?;
        result
    }
}

enum Command {
    Info,
    Dfu,
    Leave,
    Crc { address: u32, length: usize },
    Dump(PathBuf),
    Update(PathBuf),
}
impl Command {
    fn parse(args: &[String]) -> Result<Self> {
        let command = args.first().ok_or("Missing native operation")?.as_str();
        let positional = match command {
            "info" | "dfu" | "exit-dfu" => 1,
            "dump" | "update" => 2,
            "crc" => 3,
            _ => return Err("Unsupported native operation".into()),
        };
        if args.len() < positional {
            return Err("Missing native operation argument".into());
        }
        let mut pid = false;
        let mut runtime = false;
        let mut double_read = false;
        let mut index = positional;
        while index < args.len() {
            match args[index].as_str() {
                "--pid" if !pid && args.get(index + 1).is_some_and(|value| value == "0xFFF1") => {
                    pid = true;
                    index += 2;
                }
                "--runtime-pid"
                    if !runtime
                        && command != "crc"
                        && args.get(index + 1).is_some_and(|value| value == "0x0930") =>
                {
                    runtime = true;
                    index += 2;
                }
                "--double-read" if command == "dump" && !double_read => {
                    double_read = true;
                    index += 1;
                }
                _ => return Err("Unsupported native selector/option".into()),
            }
        }
        if !pid || (command != "crc" && !runtime) || (command == "dump" && !double_read) {
            return Err("Explicit XS selectors and verified dump required".into());
        }
        Ok(match command {
            "info" => Self::Info,
            "dfu" => Self::Dfu,
            "exit-dfu" => Self::Leave,
            "dump" => Self::Dump(PathBuf::from(&args[1])),
            "update" => Self::Update(PathBuf::from(&args[1])),
            "crc" => {
                let address = u32::from_str_radix(
                    args[1].strip_prefix("0x").ok_or("Invalid CRC address")?,
                    16,
                )
                .map_err(|_| "Invalid CRC address")?;
                let length = args[2].parse::<usize>().map_err(|_| "Invalid CRC length")?;
                Self::Crc { address, length }
            }
            _ => return Err("Unsupported native operation".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utility::arguments;
    #[test]
    fn only_supported_requests_and_exact_target_are_accepted() {
        for name in ["info", "dfu", "exit-dfu"] {
            assert!(Command::parse(&arguments(name, &[])).is_ok());
        }
        assert!(Command::parse(&arguments("update", &["requested.hex".into()])).is_ok());
        assert!(
            Command::parse(&arguments(
                "dump",
                &["backup".into(), "--double-read".into()]
            ))
            .is_ok()
        );
        assert!(Command::parse(&arguments("dump", &["backup".into()])).is_err());
        assert!(Command::parse(&arguments("erase", &[])).is_err());
        let mut wrong = arguments("info", &[]);
        wrong[2] = "0xFFF3".into();
        assert!(Command::parse(&wrong).is_err());
    }
}

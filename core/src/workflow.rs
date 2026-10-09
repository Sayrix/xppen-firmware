use crate::{
    Result,
    image::{self, APP_OFFSET, BASE, FLASH_SIZE, Firmware, STATE_OFFSET},
    progress::{Progress, Stage, step},
    utility::{Mode, Runner, arguments, identify},
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Serialize operations sharing a backup folder; the transport also takes an OS lock.
pub struct DeviceLock {
    path: PathBuf,
    _file: fs::File,
}
impl DeviceLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let path = root.join("tablet.lock");
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|_| format!("Another tablet operation holds {}. Wait for it to finish; do not remove an active lock.", path.display()))?;
        let result = writeln!(
            file,
            "{}",
            json!({"pid": std::process::id(), "operation": "desktop-flasher"})
        );
        if let Err(e) = result {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(e.to_string());
        }
        Ok(Self { path, _file: file })
    }
}
impl Drop for DeviceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn new_session(root: &Path) -> Result<PathBuf> {
    // Windows clock resolution can give simultaneous operations the same time.
    // A process-local sequence keeps session creation exclusive without reuse.
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = root.join("desktop").join(format!(
        "{timestamp}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(path.parent().ok_or("Missing session parent")?)
        .map_err(|e| e.to_string())?;
    fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn save_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

fn wait(runner: &mut impl Runner, expected: Option<Mode>) -> Result<Mode> {
    let start = Instant::now();
    loop {
        let result = runner.run(&arguments("info", &[]));
        let last = match result {
            Ok(output) => {
                let mode = identify(&output)?;
                if expected.is_none_or(|value| value == mode) {
                    return Ok(mode);
                }
                format!("Device still in {mode:?}")
            }
            Err(error)
                if error.contains("No XP-Pen runtime device or supported DFU interface found")
                    || error.contains("(Win32 error 1167)") =>
            {
                error
            }
            Err(error) => return Err(error),
        };
        if start.elapsed() >= Duration::from_secs(30) {
            return Err(format!("USB reconnection timed out: {last}"));
        }
        thread::sleep(Duration::from_millis(1000));
    }
}

pub fn connect(runner: &mut impl Runner) -> Result<Mode> {
    step(runner, Stage::Identify, |runner| wait(runner, None))
}

fn enter(runner: &mut impl Runner) -> Result<()> {
    if wait(runner, None)? == Mode::Runtime {
        runner.run(&arguments("dfu", &[]))?;
    }
    wait(runner, Some(Mode::Dfu))?;
    Ok(())
}

/// Validate live vectors before asking the resident bootloader to execute them.
pub fn leave(runner: &mut impl Runner) -> Result<()> {
    step(runner, Stage::ReturnRuntime, leave_inner)
}

fn leave_inner(runner: &mut impl Runner) -> Result<()> {
    let output = runner.run(&arguments("info", &[]))?;
    if identify(&output)? == Mode::Dfu {
        if !output.contains("Application SP: 0x20000BA8 (valid)")
            || !output.contains("Application reset: 0x08003969 (valid)")
        {
            return Err("Live application vectors are invalid; left in DFU for recovery".into());
        }
        runner.run(&arguments("exit-dfu", &[]))?;
    }
    wait(runner, Some(Mode::Runtime))?;
    Ok(())
}

/// Parse the requested range as well as its CRC, not an unrelated printed number.
pub fn parse_range_crc(output: &str, address: u32, size: usize) -> Result<u16> {
    let lines = output
        .lines()
        .filter(|line| line.contains("CRC16/XMODEM("))
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err("Missing or ambiguous device CRC response".into());
    }
    let suffix = lines[0]
        .split_once("CRC16/XMODEM(")
        .ok_or("Invalid CRC response")?
        .1;
    let (range, crc) = suffix.split_once(")").ok_or("Invalid CRC range")?;
    let numbers = range
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            if let Some(hex) = word.strip_prefix("0x") {
                u32::from_str_radix(hex, 16)
            } else {
                word.parse()
            }
            .map_err(|_| "Invalid CRC address/length".to_string())
        })
        .collect::<Result<Vec<u32>>>()?;
    if numbers != [address, size as u32] {
        return Err("CRC response range differs from the requested range".into());
    }
    let value = crc
        .trim()
        .strip_prefix('=')
        .ok_or("Missing CRC value")?
        .trim()
        .strip_prefix("0x")
        .ok_or("Invalid CRC value")?;
    if value.len() != 4 {
        return Err("Invalid CRC value length".into());
    }
    u16::from_str_radix(value, 16).map_err(|_| "Invalid CRC value".into())
}

fn range_crc(runner: &mut impl Runner, offset: usize, size: usize) -> Result<u16> {
    let address = BASE + offset as u32;
    // CRC is bootloader-only: --runtime-pid is deliberately absent.
    let output = runner.run(&[
        "crc".into(),
        format!("0x{address:08x}"),
        size.to_string(),
        "--pid".into(),
        "0xFFF1".into(),
    ])?;
    parse_range_crc(&output, address, size)
}

pub fn verify_blocks(runner: &mut impl Runner, expected: &[u8]) -> Result<()> {
    verify_blocks_inner(runner, expected, None)
}

fn verify_blocks_inner(
    runner: &mut impl Runner,
    expected: &[u8],
    stage: Option<Stage>,
) -> Result<()> {
    if expected.len() != FLASH_SIZE {
        return Err("Expected a complete flash image".into());
    }
    for (index, bytes) in expected.chunks(4096).enumerate() {
        let offset = index * 4096;
        if range_crc(runner, offset, bytes.len())? != image::crc16(bytes) {
            return Err(format!(
                "Live flash differs at 0x{:08x}; no install approved",
                BASE + offset as u32
            ));
        }
        runner.note(&format!("Verified flash block {}/16", index + 1));
        if let Some(stage) = stage {
            runner.progress(Progress::Advanced {
                stage,
                completed: index + 1,
                total: 16,
            });
        }
    }
    Ok(())
}

fn verify_resident(runner: &mut impl Runner, backup: &[u8]) -> Result<()> {
    let mut boot = image::normalized_backup(backup)?[..APP_OFFSET].to_vec();
    let state_crc = range_crc(runner, STATE_OFFSET, 4)?;
    let state = (0..=2u32)
        .find(|state| image::crc16(&state.to_le_bytes()) == state_crc)
        .ok_or("Unrecognized live boot state")?;
    boot[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&state.to_le_bytes());
    let total = 1 + boot.len().div_ceil(1024);
    runner.progress(Progress::Advanced {
        stage: Stage::CheckResident,
        completed: 1,
        total,
    });
    for (index, bytes) in boot.chunks(1024).enumerate() {
        if range_crc(runner, index * 1024, bytes.len())? != image::crc16(bytes) {
            return Err("Resident bootloader changed; recovery refused".into());
        }
        runner.progress(Progress::Advanced {
            stage: Stage::CheckResident,
            completed: index + 2,
            total,
        });
    }
    Ok(())
}

/// Count completed checks only; a failed CRC never advances its progress bar.
fn check_blocks(runner: &mut impl Runner, expected: &[u8], stage: Stage) -> Result<()> {
    step(runner, stage, |runner| {
        verify_blocks_inner(runner, expected, Some(stage))
    })
}

fn dump_crc(output: &str, label: &str) -> Result<u16> {
    let values = output
        .lines()
        .filter_map(|line| line.split_once(label).map(|(_, value)| value.trim()))
        .collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(format!("Missing/ambiguous backup evidence: {label}"));
    }
    u16::from_str_radix(
        values[0].strip_prefix("0x").ok_or("Missing backup CRC")?,
        16,
    )
    .map_err(|_| "Invalid backup CRC".into())
}

pub fn backup(runner: &mut impl Runner, session: &Path) -> Result<Vec<u8>> {
    let result = (|| {
        step(runner, Stage::EnterDfu, enter)?;
        runner.note("Reading all 64 KiB twice. This can take several minutes.");
        let directory = session.join("full-backup");
        let output = step(runner, Stage::ReadBackup, |runner| {
            runner.run(&arguments(
                "dump",
                &[directory.to_string_lossy().into(), "--double-read".into()],
            ))
        })?;
        step(runner, Stage::CheckBackup, |runner| {
        let raw = fs::read(directory.join("ic902-main.bin")).map_err(|e| e.to_string())?;
        let main = image::normalized_backup(&raw)?;
        let local = image::crc16(&raw);
        if dump_crc(&output, "Local CRC16/XMODEM:")? != local
            || dump_crc(&output, "Device range CRC:")? != local
        {
            return Err("Independent backup CRC mismatch".into());
        }
        verify_blocks_inner(runner, &main, Some(Stage::CheckBackup))?;
        Ok(main)
        }).and_then(|main| step(runner, Stage::SaveBackup, |_| {
        save_new(&session.join("verified-full-backup.bin"), &main)?;
        save_new(&session.join("backup-verification.json"), json!({"verificationMode":"physical-double-read", "mainSha256":image::sha256(&main), "bytes":main.len()}).to_string().as_bytes())?;
        Ok(main)
        }))
    })();
    // Backup cannot damage the application. Clean up even after a read failure.
    let exit = leave(runner);
    match (result, exit) {
        (Ok(main), Ok(())) => Ok(main),
        (Err(e), Ok(())) | (Ok(_), Err(e)) => Err(e),
        (Err(a), Err(b)) => Err(format!("{a}\nNormal-mode return also failed: {b}")),
    }
}

/// Failed writes leave the device in DFU and preserve a rollback image for
/// explicit restoration. Writes and recovery are not retried automatically.
pub fn install(
    runner: &mut impl Runner,
    firmware: &Firmware,
    before: &[u8],
    session: &Path,
    recovery: bool,
) -> Result<Vec<u8>> {
    let (before, expected, input) = step(runner, Stage::SaveRollback, |_| {
        let before = image::normalized_backup(before)?;
        image::validate_application(&firmware.code)?;
        let expected = image::expected_flash(&before, firmware)?;
        let restore = image::backup_firmware(&before)?;
        save_new(&session.join("rollback-full.bin"), &before)?;
        save_new(
            &session.join("restore.hex"),
            image::application_hex(&restore).as_bytes(),
        )?;
        let input = session.join("requested.hex");
        save_new(&input, image::application_hex(firmware).as_bytes())?;
        Ok((before, expected, input))
    })?;
    let mut may_have_written = false;
    let result = (|| {
        step(runner, Stage::EnterDfu, enter)?;
        if recovery {
            step(runner, Stage::CheckResident, |runner| {
                verify_resident(runner, &before)
            })?;
        } else {
            check_blocks(runner, &before, Stage::CheckCurrent)?;
        }
        step(runner, Stage::WriteFirmware, |runner| {
            save_new(&session.join("write-started.json"), json!({"applicationSha256":firmware.hash,"rollbackSha256":image::sha256(&before),"recovery":recovery}).to_string().as_bytes())?;
            // Mark before transmission: a transport error may follow an erase.
            may_have_written = true;
            runner.run(&arguments("update", &[input.to_string_lossy().into()]))
        })?;
        // The native programmer never finalizes. ERASE sets the resident boot
        // request word to 2; verify that exact live image before requesting boot.
        step(runner, Stage::ReconnectDfu, |runner| {
            wait(runner, Some(Mode::Dfu)).map(|_| ())
        })?;
        let mut written = expected.clone();
        written[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&2u32.to_le_bytes());
        check_blocks(runner, &written, Stage::CheckWritten)?;
        step(runner, Stage::SaveVerification, |_| {
            save_new(&session.join("expected-full.bin"), &expected)?;
            save_new(&session.join("verification.json"), json!({"verificationMode":"device-block-crc","physicalFullReadback":false,"mainSha256":image::sha256(&expected),"verifiedDfuImageSha256":image::sha256(&written),"verifiedDfuBootState":2,"referenceBootState":1,"applicationSha256":firmware.hash}).to_string().as_bytes())?;
            Ok(())
        })?;
        leave(runner)?;
        Ok(expected)
    })();
    if let Err(error) = &result
        && !may_have_written
        && !recovery
        && let Err(exit) = leave(runner)
    {
        return Err(format!("{error}\nNormal-mode return also failed: {exit}"));
    }
    result.map_err(|error| if may_have_written { format!("{error}\nThe install is unverified. No automatic write retry or DFU exit was requested. Retain {} and use Restore saved backup.", session.join("rollback-full.bin").display()) } else { error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{OPTIMIZED_HASH, STOCK_HASH};
    const STOCK: &[u8] = include_bytes!("../../firmware/stock-deco-fun-xs/firmware.bin");
    const OPT: &[u8] = include_bytes!(
        "../../firmware/deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31/firmware.bin"
    );
    const BOOT: &[u8] = include_bytes!("../../tests/fixtures/ic902-bootloader.bin");
    fn main_image() -> Vec<u8> {
        let mut bytes = vec![255; FLASH_SIZE];
        bytes[..APP_OFFSET].copy_from_slice(BOOT);
        bytes[APP_OFFSET..APP_OFFSET + STOCK.len()].copy_from_slice(STOCK);
        bytes
    }
    struct Simulated {
        main: Vec<u8>,
        mode: Mode,
        calls: Vec<String>,
        fail_write: bool,
        corrupt_after: bool,
        fail_dump: bool,
        progress: Vec<Progress>,
    }
    impl Runner for Simulated {
        fn note(&mut self, _: &str) {}
        fn progress(&mut self, event: Progress) {
            self.progress.push(event);
        }
        fn run(&mut self, args: &[String]) -> Result<String> {
            self.calls.push(args[0].clone());
            match args[0].as_str() {
                "info" => Ok(if self.mode == Mode::Runtime { "Runtime device: 4 inch PenTablet [28BD:0930]\nIdentity: manufacturer=UGTABLET serial=000000" } else { "Bootloader: TABLET_DFU200718 [28BD:FFF1], IC 902, framed\nRuntime VID:PID: 28BD:0930\nFlash size: 64 KiB\nFlash range: 0x08000000 .. 0x0800FFFF\nApplication SP: 0x20000BA8 (valid)\nApplication reset: 0x08003969 (valid)" }.into()),
                "dfu" => { self.mode = Mode::Dfu; self.main[STATE_OFFSET..STATE_OFFSET+4].copy_from_slice(&1u32.to_le_bytes()); Ok(String::new()) },
                "exit-dfu" => { self.mode = Mode::Runtime; self.main[STATE_OFFSET..STATE_OFFSET+4].copy_from_slice(&0u32.to_le_bytes()); Ok(String::new()) },
                "dump" => { assert!(args.contains(&"--double-read".into())); if self.fail_dump { return Err("Simulated dump failure".into()); } let directory = Path::new(&args[1]); fs::create_dir(directory).unwrap(); save_new(&directory.join("ic902-main.bin"), &self.main).unwrap(); let crc = image::crc16(&self.main); Ok(format!("Local CRC16/XMODEM: 0x{crc:04X}\nDevice range CRC: 0x{crc:04X}")) },
                "crc" => { assert!(!args.contains(&"--runtime-pid".into())); let address = u32::from_str_radix(args[1].trim_start_matches("0x"),16).unwrap(); let size: usize = args[2].parse().unwrap(); let offset = (address - BASE) as usize; Ok(format!("CRC16/XMODEM(0x{address:08X}, {size}) = 0x{:04X}", image::crc16(&self.main[offset..offset+size]))) },
                "update" => { if self.fail_write { self.main[APP_OFFSET..].fill(255); return Err("Simulated disconnected write".into()); } let code = image::parse_hex(&fs::read_to_string(&args[1]).unwrap()).unwrap(); self.main[APP_OFFSET..].fill(255); self.main[APP_OFFSET..APP_OFFSET+code.len()].copy_from_slice(&code); if self.corrupt_after { self.main[0xa000] ^= 1; } self.mode = Mode::Dfu; self.main[STATE_OFFSET..STATE_OFFSET+4].copy_from_slice(&2u32.to_le_bytes()); Ok(String::new()) },
                _ => Err("Unexpected simulated command".into()),
            }
        }
    }
    fn device() -> Simulated {
        Simulated {
            main: main_image(),
            mode: Mode::Runtime,
            calls: vec![],
            fail_write: false,
            corrupt_after: false,
            fail_dump: false,
            progress: vec![],
        }
    }
    fn directory() -> PathBuf {
        let root = std::env::temp_dir().join("deco-flasher-tests");
        new_session(&root).unwrap()
    }
    #[test]
    fn correct_install_checks_before_and_after() {
        let mut device = device();
        let session = directory();
        let before = main_image();
        let firmware = Firmware::pinned("optimized", OPT, OPTIMIZED_HASH).unwrap();
        let expected = install(&mut device, &firmware, &before, &session, false).unwrap();
        assert_eq!(image::normalized_backup(&device.main).unwrap(), expected);
        assert_eq!(device.mode, Mode::Runtime);
        let finalize = device
            .calls
            .iter()
            .position(|call| call == "exit-dfu")
            .unwrap();
        assert_eq!(
            device.calls[..finalize]
                .iter()
                .filter(|call| *call == "crc")
                .count(),
            32
        );
        assert_eq!(device.calls.iter().filter(|call| *call == "dfu").count(), 1);
        assert_eq!(
            device.calls.iter().filter(|call| *call == "crc").count(),
            32
        );
        assert_eq!(
            device.calls.iter().filter(|call| *call == "update").count(),
            1
        );
        assert!(session.join("rollback-full.bin").exists());
        for stage in [Stage::CheckCurrent, Stage::CheckWritten] {
            let counts: Vec<_> = device
                .progress
                .iter()
                .filter_map(|event| match event {
                    Progress::Advanced {
                        stage: reported,
                        completed,
                        total,
                    } if *reported == stage => Some((*completed, *total)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                counts,
                (1..=16).map(|count| (count, 16)).collect::<Vec<_>>()
            );
            assert!(device.progress.contains(&Progress::Finished(stage)));
        }
        assert_eq!(
            device.progress.last(),
            Some(&Progress::Finished(Stage::ReturnRuntime))
        );
        fs::remove_dir_all(session).unwrap();
    }
    #[test]
    fn mismatch_blocks_erase_and_returns_runtime() {
        let mut device = device();
        device.main[0x9000] ^= 1;
        let session = directory();
        let firmware = Firmware::pinned("stock", STOCK, STOCK_HASH).unwrap();
        assert!(install(&mut device, &firmware, &main_image(), &session, false).is_err());
        assert!(!device.calls.contains(&"update".into()));
        assert_eq!(device.mode, Mode::Runtime);
        fs::remove_dir_all(session).unwrap();
    }
    #[test]
    fn failed_or_corrupt_write_is_not_retried_or_booted() {
        for corrupt in [false, true] {
            let mut device = device();
            device.fail_write = !corrupt;
            device.corrupt_after = corrupt;
            let session = directory();
            let firmware = Firmware::pinned("optimized", OPT, OPTIMIZED_HASH).unwrap();
            assert!(install(&mut device, &firmware, &main_image(), &session, false).is_err());
            assert_eq!(
                device.calls.iter().filter(|call| *call == "update").count(),
                1
            );
            assert!(!device.calls.contains(&"exit-dfu".into()));
            assert_eq!(device.mode, Mode::Dfu);
            let failed_stage = if corrupt {
                Stage::CheckWritten
            } else {
                Stage::WriteFirmware
            };
            assert!(device.progress.contains(&Progress::Failed(failed_stage)));
            assert!(!device.progress.contains(&Progress::Finished(failed_stage)));
            assert!(
                !device
                    .progress
                    .contains(&Progress::Started(Stage::ReturnRuntime))
            );
            fs::remove_dir_all(session).unwrap();
        }
    }
    #[test]
    fn explicit_recovery_handles_partial_app_but_refuses_changed_boot() {
        for changed_boot in [false, true] {
            let mut device = device();
            device.mode = Mode::Dfu;
            device.main[APP_OFFSET..].fill(255);
            device.main[STATE_OFFSET] = 2;
            if changed_boot {
                device.main[15] ^= 1;
            }
            let session = directory();
            let backup = main_image();
            let firmware = image::backup_firmware(&backup).unwrap();
            let result = install(&mut device, &firmware, &backup, &session, true);
            assert_eq!(result.is_ok(), !changed_boot);
            assert_eq!(device.calls.contains(&"update".into()), !changed_boot);
            if changed_boot {
                assert_eq!(device.mode, Mode::Dfu);
                assert!(!device.calls.contains(&"exit-dfu".into()));
            }
            fs::remove_dir_all(session).unwrap();
        }
    }
    #[test]
    fn range_parser_and_exclusive_lock() {
        assert_eq!(
            parse_range_crc("CRC16/XMODEM(0x08000000, 4096) = 0x1234", BASE, 4096).unwrap(),
            0x1234
        );
        assert!(parse_range_crc("CRC16/XMODEM(0x08000000, 4096) = 0x1234", BASE, 4).is_err());
        let dir = directory();
        let lock = DeviceLock::acquire(&dir).unwrap();
        assert!(DeviceLock::acquire(&dir).is_err());
        drop(lock);
        assert!(DeviceLock::acquire(&dir).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_backup_double_reads_saves_and_returns_runtime() {
        let mut device = device();
        let session = directory();
        let bytes = backup(&mut device, &session).unwrap();
        assert_eq!(bytes, main_image());
        assert_eq!(device.mode, Mode::Runtime);
        assert_eq!(
            fs::read(session.join("verified-full-backup.bin")).unwrap(),
            bytes
        );
        assert_eq!(
            device.calls.iter().filter(|call| *call == "crc").count(),
            16
        );
        assert!(!device.calls.contains(&"update".into()));
        assert!(
            device
                .progress
                .contains(&Progress::Finished(Stage::ReadBackup))
        );
        assert!(
            device
                .progress
                .contains(&Progress::Finished(Stage::ReturnRuntime))
        );
        assert!(
            device
                .progress
                .contains(&Progress::Finished(Stage::SaveBackup))
        );
        fs::remove_dir_all(session).unwrap();
    }

    #[test]
    fn failed_backup_returns_runtime_without_writing() {
        let mut device = device();
        device.fail_dump = true;
        let session = directory();
        assert!(backup(&mut device, &session).is_err());
        assert_eq!(device.mode, Mode::Runtime);
        assert!(!session.join("verified-full-backup.bin").exists());
        assert!(!device.calls.contains(&"update".into()));
        assert!(
            device
                .progress
                .contains(&Progress::Failed(Stage::ReadBackup))
        );
        assert!(
            device
                .progress
                .contains(&Progress::Finished(Stage::ReturnRuntime))
        );
        assert!(
            !device
                .progress
                .contains(&Progress::Finished(Stage::SaveBackup))
        );
        fs::remove_dir_all(session).unwrap();
    }
}

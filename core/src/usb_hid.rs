//! Linux/macOS USB transport. The protocol remains identical to Windows HID;
//! libusb exposes the 64-byte interrupt payload without Windows' report-ID pad.
// Windows tests also compile the USB API and descriptor parser.
// Windows production builds use the native HID transport.
#![cfg_attr(windows, allow(dead_code))]

use crate::{
    Result,
    protocol::{REPORT_BYTES, Reports},
};
use rusb::{Context, DeviceHandle, Direction, Recipient, RequestType, TransferType, UsbContext};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(2);
const MISSING: &str = "No XP-Pen runtime device or supported DFU interface found";

pub struct OperationLock {
    _file: File,
}
impl OperationLock {
    /// Use an OS lock, not an existence test: a crashed process releases it and
    /// copies in different app folders still coordinate through one user path.
    pub fn acquire() -> Result<Self> {
        let home =
            std::env::var_os("HOME").ok_or("HOME is missing; cannot establish the tablet lock")?;
        let root = if cfg!(target_os = "macos") {
            PathBuf::from(home).join("Library/Caches/xppen-firmware")
        } else {
            std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(home).join(".cache"))
                .join("xppen-firmware")
        };
        Self::at(&root)
    }

    fn at(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .map_err(|e| format!("Cannot create operation lock folder: {e}"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("tablet-operation.lock"))
            .map_err(|e| format!("Cannot open tablet lock: {e}"))?;
        file.try_lock()
            .map_err(|e| format!("Another native flasher operation may be running: {e}"))?;
        // Keep the inode: removing it while another process has it open would
        // let a third process lock a different file under the same name.
        Ok(Self { _file: file })
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    usb: rusb::Device<Context>,
    interface: u8,
    input: u8,
    output: u8,
}

fn error(action: &str, error: rusb::Error) -> String {
    match error {
        rusb::Error::NoDevice | rusb::Error::NotFound => format!("{MISSING} ({action}: {error})"),
        rusb::Error::Access => format!(
            "{action}: USB access denied. Close tablet software and check device permissions. Linux needs access to both 28BD:0930 and 28BD:FFF1. macOS may require USB capture privileges; this app never elevates itself."
        ),
        _ => format!("{action}: {error}"),
    }
}

fn identity(pid: u16, manufacturer: &str, product: &str, serial: &str) -> Result<()> {
    match pid {
        0x0930
            if manufacturer == "UGTABLET"
                && product == "4 inch PenTablet"
                && serial == "000000" =>
        {
            Ok(())
        }
        0xfff1 if product == "TABLET_DFU200718" => Ok(()),
        _ => Err("USB identity is not the supported Deco Fun XS / FFF1 bootloader".into()),
    }
}

fn report_length(extra: &[u8]) -> Result<usize> {
    let mut offset = 0;
    while offset < extra.len() {
        let header = extra
            .get(offset..offset + 2)
            .ok_or("Truncated USB descriptor")?;
        let length = usize::from(header[0]);
        if length < 2 {
            return Err("Invalid USB descriptor length".into());
        }
        let data = extra
            .get(offset..offset + length)
            .ok_or("Truncated USB descriptor body")?;
        if data[1] == 0x21 {
            let count = usize::from(*data.get(5).ok_or("Short HID descriptor")?);
            if data.len() != 6 + count * 3 {
                return Err("Malformed HID descriptor table".into());
            }
            for item in data[6..].chunks_exact(3) {
                if item[0] == 0x22 {
                    let size = usize::from(u16::from_le_bytes([item[1], item[2]]));
                    if (1..=4096).contains(&size) {
                        return Ok(size);
                    }
                    return Err("Unsupported HID report descriptor size".into());
                }
            }
        }
        offset += length;
    }
    Err("Missing HID report descriptor".into())
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u32,
    size: u32,
    count: u32,
}
#[derive(Default, Debug, PartialEq, Eq)]
struct Layout {
    matched: bool,
    numbered: bool,
    input_bits: u32,
    output_bits: u32,
    collections: usize,
}

/// Validate actual top-level usage and report sizes instead of assuming every
/// HID interface on a matching USB PID belongs to the bootloader protocol.
fn layout(bytes: &[u8], wanted_page: u16) -> Result<Layout> {
    let mut result = Layout::default();
    let mut global = Globals::default();
    let mut stack = Vec::new();
    let mut usage = 0;
    let mut depth = 0usize;
    let mut selected = false;
    let mut offset = 0;
    while offset < bytes.len() {
        let prefix = bytes[offset];
        offset += 1;
        if prefix == 0xfe {
            return Err("Unsupported HID long item".into());
        }
        let length = match prefix & 3 {
            3 => 4,
            n => usize::from(n),
        };
        let data = bytes
            .get(offset..offset + length)
            .ok_or("Truncated HID item")?;
        offset += length;
        let value = data.iter().enumerate().fold(0u32, |value, (i, byte)| {
            value | (u32::from(*byte) << (i * 8))
        });
        let kind = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        match (kind, tag) {
            (1, 0) => global.page = value,
            (1, 7) => global.size = value,
            (1, 8) => {
                if value == 0 || value > 255 {
                    return Err("Invalid HID report ID".into());
                }
                result.numbered = true;
            }
            (1, 9) => global.count = value,
            (1, 10) => {
                if stack.len() >= 16 {
                    return Err("HID global stack too deep".into());
                }
                stack.push(global);
            }
            (1, 11) => global = stack.pop().ok_or("Unbalanced HID global stack")?,
            (2, 0) => usage = value,
            (0, 10) => {
                if depth == 0 {
                    result.collections += 1;
                    let page = if usage > 0xffff {
                        usage >> 16
                    } else {
                        global.page
                    };
                    selected = value == 1 && page == u32::from(wanted_page) && usage & 0xffff == 1;
                    result.matched |= selected;
                }
                depth += 1;
                if depth > 16 {
                    return Err("HID collections too deep".into());
                }
            }
            (0, 12) => {
                depth = depth.checked_sub(1).ok_or("Unbalanced HID collection")?;
                if depth == 0 {
                    selected = false;
                }
            }
            (0, 8 | 9) if selected => {
                let bits = global
                    .size
                    .checked_mul(global.count)
                    .ok_or("HID report size overflow")?;
                let target = if tag == 8 {
                    &mut result.input_bits
                } else {
                    &mut result.output_bits
                };
                *target = target.checked_add(bits).ok_or("HID report size overflow")?;
            }
            _ => {}
        }
        if kind == 0 {
            usage = 0;
        }
    }
    if depth != 0 || !stack.is_empty() || result.collections == 0 {
        return Err("Unterminated or empty HID descriptor".into());
    }
    Ok(result)
}

fn boot_layout(layout: &Layout) -> Result<()> {
    if !layout.matched
        || layout.numbered
        || layout.collections != 1
        || layout.input_bits != (REPORT_BYTES * 8) as u32
        || layout.output_bits != (REPORT_BYTES * 8) as u32
    {
        return Err("FFF1 requires a single FF00:0001 collection with unnumbered 64-byte input/output reports".into());
    }
    Ok(())
}

/// Identity/descriptor reads only. Kernel interfaces are never claimed during
/// discovery, and the reserved C8 string is requested only by explicit DFU entry.
pub fn target() -> Result<Device> {
    let context = Context::new().map_err(|e| error("Initialize USB", e))?;
    let list = context.devices().map_err(|e| error("Enumerate USB", e))?;
    let mut matches = Vec::new();
    for usb in list.iter() {
        let descriptor = usb
            .device_descriptor()
            .map_err(|e| error("Read USB identity", e))?;
        if descriptor.vendor_id() != 0x28bd || !matches!(descriptor.product_id(), 0x0930 | 0xfff1) {
            continue;
        }
        let pid = descriptor.product_id();
        let handle = usb
            .open()
            .map_err(|e| error("Open tablet for metadata", e))?;
        let product = handle
            .read_product_string_ascii(&descriptor)
            .map_err(|e| error("Read product", e))?;
        let manufacturer = if pid == 0x0930 {
            handle
                .read_manufacturer_string_ascii(&descriptor)
                .map_err(|e| error("Read manufacturer", e))?
        } else {
            String::new()
        };
        let serial = if pid == 0x0930 {
            handle
                .read_serial_number_string_ascii(&descriptor)
                .map_err(|e| error("Read serial", e))?
        } else {
            String::new()
        };
        identity(pid, &manufacturer, &product, &serial)?;
        let config = usb
            .active_config_descriptor()
            .map_err(|e| error("Read active configuration", e))?;
        for interface in config.interfaces().flat_map(|i| i.descriptors()) {
            if interface.class_code() != 3 || interface.setting_number() != 0 {
                continue;
            }
            if pid == 0xfff1 && interface.interface_number() != 1 {
                continue;
            }
            let size = report_length(interface.extra())?;
            let mut report = vec![0; size];
            let count = handle
                .read_control(
                    rusb::request_type(Direction::In, RequestType::Standard, Recipient::Interface),
                    6,
                    0x2200,
                    u16::from(interface.interface_number()),
                    &mut report,
                    TIMEOUT,
                )
                .map_err(|e| error("Read HID report descriptor", e))?;
            if count != size {
                return Err("Short HID report descriptor".into());
            }
            let layout = layout(&report, if pid == 0xfff1 { 0xff00 } else { 0xff0a })?;
            if !layout.matched {
                continue;
            }
            let mut input = 0;
            let mut output = 0;
            if pid == 0xfff1 {
                boot_layout(&layout)?;
                for endpoint in interface.endpoint_descriptors() {
                    if endpoint.transfer_type() != TransferType::Interrupt
                        || endpoint.max_packet_size() != REPORT_BYTES as u16
                    {
                        return Err("Unsupported FFF1 USB endpoint".into());
                    }
                    let slot = if endpoint.direction() == Direction::In {
                        &mut input
                    } else {
                        &mut output
                    };
                    if *slot != 0 {
                        return Err("Ambiguous FFF1 USB endpoints".into());
                    }
                    *slot = endpoint.address();
                }
                if input == 0 || output == 0 {
                    return Err("FFF1 requires 64-byte interrupt IN and OUT endpoints".into());
                }
            }
            matches.push(Device {
                pid,
                product: product.clone(),
                manufacturer: manufacturer.clone(),
                serial: serial.clone(),
                usb: usb.clone(),
                interface: interface.interface_number(),
                input,
                output,
            });
        }
    }
    if matches.len() > 1 {
        return Err("Multiple supported tablets/interfaces found; attach only one".into());
    }
    matches.pop().ok_or_else(|| MISSING.into())
}

pub fn enter_dfu(device: &Device) -> Result<()> {
    if device.pid != 0x0930 {
        return Err("DFU entry requires verified runtime identity".into());
    }
    let handle = device
        .usb
        .open()
        .map_err(|e| error("Open runtime tablet", e))?;
    let mut response = [0; 12];
    // Same USB GET_DESCRIPTOR(string C8, length 12) used by Windows. Request
    // once; expected reset/disconnect is confirmed by workflow re-enumeration.
    match handle.read_control(
        rusb::request_type(Direction::In, RequestType::Standard, Recipient::Device),
        6,
        0x03c8,
        0x0409,
        &mut response,
        TIMEOUT,
    ) {
        Ok(_) | Err(rusb::Error::NoDevice | rusb::Error::Io | rusb::Error::Timeout) => Ok(()),
        Err(e) => Err(error("DFU string request failed", e)),
    }
}

pub struct HidReports {
    handle: DeviceHandle<Context>,
    input: u8,
    output: u8,
}
impl HidReports {
    pub fn open(device: &Device) -> Result<Self> {
        if device.pid != 0xfff1 {
            return Err("Report transport requires FFF1".into());
        }
        let handle = device.usb.open().map_err(|e| error("Open bootloader", e))?;
        // Only the supported boot interface is claimed. libusb releases it and
        // reattaches a detached kernel driver when its handle is dropped.
        if rusb::supports_detach_kernel_driver() {
            handle
                .set_auto_detach_kernel_driver(true)
                .map_err(|e| error("Enable temporary interface ownership", e))?;
        }
        handle
            .claim_interface(device.interface)
            .map_err(|e| error("Claim FFF1 interface", e))?;
        let mut stale = [0; REPORT_BYTES];
        for _ in 0..16 {
            match handle.read_interrupt(device.input, &mut stale, Duration::from_millis(1)) {
                Err(rusb::Error::Timeout) => {
                    return Ok(Self {
                        handle,
                        input: device.input,
                        output: device.output,
                    });
                }
                Ok(_) => {}
                Err(e) => return Err(error("Drain stale bootloader input", e)),
            }
        }
        Err("Bootloader input did not become idle; refusing ambiguous responses".into())
    }
}
impl Reports for HidReports {
    fn write(&mut self, report: &[u8; REPORT_BYTES], timeout: Duration) -> Result<()> {
        let count = self
            .handle
            .write_interrupt(self.output, report, timeout)
            .map_err(|e| error("Write bootloader report", e))?;
        if count != REPORT_BYTES {
            return Err("Short USB write; request will not be replayed".into());
        }
        Ok(())
    }
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut buffer = [0; REPORT_BYTES];
        let count = self
            .handle
            .read_interrupt(self.input, &mut buffer, timeout)
            .map_err(|e| error("Read bootloader report", e))?;
        if count != REPORT_BYTES {
            return Err("Unexpected bootloader input length".into());
        }
        Ok(buffer.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BOOT: &[u8] = &[
        0x06, 0x00, 0xff, 0x09, 1, 0xa1, 1, 0x75, 8, 0x95, 64, 0x09, 1, 0x81, 2, 0x09, 1, 0x91, 2,
        0xc0,
    ];
    #[test]
    fn rejects_wrong_identity_usage_lengths_and_numbered_reports() {
        assert!(identity(0x0930, "UGTABLET", "4 inch PenTablet", "000000").is_ok());
        assert!(identity(0xfff1, "", "TABLET_DFU200718", "").is_ok());
        for (pid, product) in [(0xfff3, "TABLET_DFU200718"), (0xfff1, "other")] {
            assert!(identity(pid, "", product, "").is_err());
        }
        assert!(identity(0x0930, "UGTABLET", "4 inch PenTablet", "different").is_err());
        assert!(boot_layout(&layout(BOOT, 0xff00).unwrap()).is_ok());
        assert!(boot_layout(&layout(BOOT, 0xff0a).unwrap()).is_err());
        let mut short = BOOT.to_vec();
        short[10] = 63;
        assert!(boot_layout(&layout(&short, 0xff00).unwrap()).is_err());
        let mut numbered = vec![0x85, 1];
        numbered.extend_from_slice(BOOT);
        assert!(boot_layout(&layout(&numbered, 0xff00).unwrap()).is_err());
    }
    #[test]
    fn malformed_descriptors_cannot_select_an_interface() {
        assert_eq!(
            report_length(&[9, 0x21, 0x11, 1, 0, 1, 0x22, 20, 0]).unwrap(),
            20
        );
        assert!(report_length(&[9, 0x21, 0x11, 1, 0, 2, 0x22, 20, 0]).is_err());
        for end in 1..BOOT.len() {
            assert!(layout(&BOOT[..end], 0xff00).is_err());
        }
        assert!(layout(&[0xc0], 0xff00).is_err());
        assert!(layout(&[0xb4], 0xff00).is_err());
        assert!(layout(&[0xfe, 0, 0], 0xff00).is_err());
    }
    #[test]
    fn actual_public_bootloader_descriptors_match_the_transport() {
        // Check descriptors against the public bootloader fixture.
        // Offsets refer to fixture bytes, not device write addresses.
        let boot = include_bytes!("../../tests/fixtures/ic902-bootloader.bin");
        boot_layout(&layout(&boot[0x252c..0x2549], 0xff00).unwrap()).unwrap();
        assert_eq!(report_length(&boot[0x2515..0x251e]).unwrap(), 29);
        assert_eq!(&boot[0x251e..0x2525], &[7, 5, 0x82, 3, 64, 0, 2]);
        assert_eq!(&boot[0x2525..0x252c], &[7, 5, 0x02, 3, 64, 0, 2]);
    }
    #[test]
    fn operation_lock_blocks_other_folders_and_releases_after_drop() {
        let path = std::env::temp_dir().join(format!("xppen-usb-lock-{}", std::process::id()));
        let first = OperationLock::at(&path).unwrap();
        assert!(OperationLock::at(&path).is_err());
        drop(first);
        drop(OperationLock::at(&path).unwrap());
        fs::remove_dir_all(&path).unwrap();
    }
}

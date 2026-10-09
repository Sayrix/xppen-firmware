use crate::Result;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const FLASH_SIZE: usize = 65536;
pub const APP_OFFSET: usize = 0x3800;
pub const BASE: u32 = 0x08000000;
pub const STATE_OFFSET: usize = 0x3400;
pub const BOOT_HASH: &str = "fa7bf148e5ee63272e1ed11d41dee58091a7e4a2845383ab8aa5b52b41be2640";
pub const STOCK_HASH: &str = "1501685b6ac0b3b2f38dbc32c397fe8f8ccf0f31d827c79601ed8d02e5112127";
pub const OPTIMIZED_HASH: &str = "041c94e06c89d0ba44555b1205171633e50fde5d1b421f7e2a7ae2c16eb99acb";

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
    }
    crc
}

/// A validated snapshot: changes to the selected file cannot affect a later flash.
#[derive(Clone, Debug)]
pub struct Firmware {
    pub name: String,
    pub code: Vec<u8>,
    pub hash: String,
}

impl Firmware {
    pub fn new(name: impl Into<String>, code: Vec<u8>) -> Result<Self> {
        validate_application(&code)?;
        Ok(Self {
            name: name.into(),
            hash: sha256(&code),
            code,
        })
    }

    pub fn pinned(name: &str, code: &[u8], expected: &str) -> Result<Self> {
        let firmware = Self::new(name, code.to_vec())?;
        if firmware.hash != expected {
            return Err("Built-in firmware hash mismatch".into());
        }
        Ok(firmware)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
        if metadata.len() > 1024 * 1024 {
            return Err("Firmware file exceeds 1 MiB".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let code = match extension.as_str() {
            "bin" => bytes,
            "hex" => parse_hex(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)?,
            _ => return Err("Choose an application .hex or .bin file".into()),
        };
        Self::new(path.file_name().unwrap_or_default().to_string_lossy(), code)
    }
}

pub fn validate_application(code: &[u8]) -> Result<()> {
    if code.len() < 0x168 || code.len() > FLASH_SIZE - APP_OFFSET || !code.len().is_multiple_of(4) {
        return Err(
            "Invalid application length; full-flash and truncated binaries are refused".into(),
        );
    }
    validate_vectors(code)
}

pub fn validate_vectors(code: &[u8]) -> Result<()> {
    if code.get(..8) != Some(&[0xa8, 0x0b, 0x00, 0x20, 0x69, 0x39, 0x00, 0x08]) {
        return Err("Application vectors do not match the supported Deco Fun XS family".into());
    }
    Ok(())
}

/// Permit only the resident bootloader's documented mutable request word.
pub fn normalized_backup(main: &[u8]) -> Result<Vec<u8>> {
    if main.len() != FLASH_SIZE {
        return Err("Backup must contain exactly 64 KiB".into());
    }
    let mut image = main.to_vec();
    let state = u32::from_le_bytes(
        image[STATE_OFFSET..STATE_OFFSET + 4]
            .try_into()
            .map_err(|_| "Missing boot state")?,
    );
    if state > 2 {
        return Err("Unknown persistent boot state".into());
    }
    image[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
    if sha256(&image[..APP_OFFSET]) != BOOT_HASH {
        return Err("Resident bootloader differs from the tested IC902 / FFF1 target".into());
    }
    validate_vectors(&image[APP_OFFSET..])?;
    Ok(image)
}

pub fn backup_firmware(main: &[u8]) -> Result<Firmware> {
    let image = normalized_backup(main)?;
    let mut end = image.len();
    while end > APP_OFFSET + 0x168 && image[end - 1] == 255 {
        end -= 1;
    }
    end = end.div_ceil(4) * 4;
    Firmware::new("Saved tablet application", image[APP_OFFSET..end].to_vec())
}

pub fn expected_flash(before: &[u8], firmware: &Firmware) -> Result<Vec<u8>> {
    let mut expected = normalized_backup(before)?;
    expected[APP_OFFSET..].fill(255);
    expected[APP_OFFSET..APP_OFFSET + firmware.code.len()].copy_from_slice(&firmware.code);
    Ok(expected)
}

fn record(address: u16, kind: u8, data: &[u8]) -> String {
    let mut bytes = vec![data.len() as u8, (address >> 8) as u8, address as u8, kind];
    bytes.extend_from_slice(data);
    bytes.push(0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))));
    let mut line = String::from(":");
    for byte in bytes {
        use std::fmt::Write;
        write!(line, "{byte:02X}").expect("Writing to a String cannot fail");
    }
    line.push('\n');
    line
}

pub fn application_hex(firmware: &Firmware) -> String {
    let mut text = record(0, 4, &[8, 0]);
    for (index, data) in firmware.code.chunks(16).enumerate() {
        text.push_str(&record((APP_OFFSET + index * 16) as u16, 0, data));
    }
    text.push_str(&record(0, 1, &[]));
    text
}

/// Reject conflicting records and all boot/config addresses before any USB access.
pub fn parse_hex(text: &str) -> Result<Vec<u8>> {
    let mut image = vec![255; FLASH_SIZE - APP_OFFSET];
    let mut written = vec![false; image.len()];
    let mut upper = 0u32;
    let mut eof = false;
    let mut first = None;
    let mut end = 0;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if eof {
            return Err("Data after HEX end-of-file".into());
        }
        let raw = line.strip_prefix(':').ok_or("Missing HEX record prefix")?;
        if raw.len() % 2 != 0 || !raw.is_ascii() {
            return Err("Malformed HEX record".into());
        }
        let bytes = (0..raw.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&raw[i..i + 2], 16).map_err(|_| "Invalid HEX byte".to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        if bytes.len() < 5
            || bytes.len() != usize::from(bytes[0]) + 5
            || bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)) != 0
        {
            return Err("HEX record length/checksum mismatch".into());
        }
        let address = u32::from(u16::from_be_bytes([bytes[1], bytes[2]]));
        let data = &bytes[4..bytes.len() - 1];
        match bytes[3] {
            0 => {
                if address + data.len() as u32 > 65536 {
                    return Err("HEX record crosses a 64 KiB boundary".into());
                }
                for (index, byte) in data.iter().enumerate() {
                    let absolute = upper
                        .checked_add(address + index as u32)
                        .ok_or("HEX address overflow")?;
                    if !(BASE + APP_OFFSET as u32..BASE + FLASH_SIZE as u32).contains(&absolute) {
                        return Err("HEX contains an address outside the application".into());
                    }
                    let offset = (absolute - BASE - APP_OFFSET as u32) as usize;
                    if written[offset] {
                        return Err("Overlapping HEX records are refused".into());
                    }
                    image[offset] = *byte;
                    written[offset] = true;
                    first = Some(first.map_or(offset, |f: usize| f.min(offset)));
                    end = end.max(offset + 1);
                }
            }
            1 if data.is_empty() && address == 0 => eof = true,
            4 if data.len() == 2 && address == 0 => {
                upper = u32::from(u16::from_be_bytes([data[0], data[1]])) << 16
            }
            2 if data.len() == 2 && address == 0 => {
                upper = u32::from(u16::from_be_bytes([data[0], data[1]])) << 4
            }
            _ => return Err("Unsupported or malformed HEX record type".into()),
        }
    }
    if !eof || first != Some(0) {
        return Err("HEX must start at 0x08003800 and end with EOF".into());
    }
    image.truncate(end);
    validate_application(&image)?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    const STOCK: &[u8] = include_bytes!("../../firmware/stock-deco-fun-xs/firmware.bin");
    const OPTIMIZED: &[u8] = include_bytes!(
        "../../firmware/deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31/firmware.bin"
    );
    const BOOT: &[u8] = include_bytes!("../../tests/fixtures/ic902-bootloader.bin");
    pub fn backup() -> Vec<u8> {
        let mut bytes = vec![255; FLASH_SIZE];
        bytes[..APP_OFFSET].copy_from_slice(BOOT);
        bytes[APP_OFFSET..APP_OFFSET + STOCK.len()].copy_from_slice(STOCK);
        bytes
    }

    #[test]
    fn builtins_and_hex_round_trip() {
        for (name, bytes, hash) in [
            ("stock", STOCK, STOCK_HASH),
            ("optimized", OPTIMIZED, OPTIMIZED_HASH),
        ] {
            let firmware = Firmware::pinned(name, bytes, hash).unwrap();
            assert_eq!(parse_hex(&application_hex(&firmware)).unwrap(), bytes);
        }
        assert_eq!(crc16(b"123456789"), 0x31c3);
    }

    #[test]
    fn backup_preserves_boot_and_erases_old_tail() {
        let before = backup();
        for state in 0..=2u32 {
            let mut bytes = before.clone();
            bytes[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&state.to_le_bytes());
            assert_eq!(normalized_backup(&bytes).unwrap(), before);
        }
        let firmware = Firmware::new("short", STOCK[..0x168].to_vec()).unwrap();
        let after = expected_flash(&before, &firmware).unwrap();
        assert_eq!(&after[..APP_OFFSET], &before[..APP_OFFSET]);
        assert!(after[APP_OFFSET + 0x168..].iter().all(|b| *b == 255));
        let mut corrupted = before.clone();
        corrupted[15] ^= 1;
        assert!(normalized_backup(&corrupted).is_err());
        corrupted = before.clone();
        corrupted[STATE_OFFSET] = 3;
        assert!(normalized_backup(&corrupted).is_err());
        corrupted = before.clone();
        corrupted[APP_OFFSET] ^= 1;
        assert!(normalized_backup(&corrupted).is_err());
    }

    #[test]
    fn unsafe_and_malformed_hex_is_refused() {
        let firmware = Firmware::new("stock", STOCK.to_vec()).unwrap();
        let valid = application_hex(&firmware);
        assert!(parse_hex(&valid.replace(":00000001FF", "")).is_err());
        assert!(parse_hex(&(valid.clone() + ":00000001FF\n")).is_err());
        assert!(parse_hex(&valid.replace("A80B", "A90B")).is_err());
        assert!(
            parse_hex(
                &(record(0, 4, &[8, 0]) + &record(0x3400, 0, &[1, 0, 0, 0]) + ":00000001FF\n")
            )
            .is_err()
        );
        let duplicated = valid.replacen(
            ":00000001FF",
            &format!("{}:00000001FF", record(0x3800, 0, &STOCK[..16])),
            1,
        );
        assert!(parse_hex(&duplicated).is_err());
        assert!(Firmware::new("full flash", backup()).is_err());
    }
}

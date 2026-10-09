//! IC902/FFF1 framing, flash access and CRC verification.
//! Protocol logic is independent of the operating system transport.
use crate::{Result, image};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

pub const REPORT_BYTES: usize = 64;
const MAX_BODY: usize = 1000;

pub trait Reports {
    /// Windows report-ID padding is handled by the transport, not the protocol.
    fn write(&mut self, report: &[u8; REPORT_BYTES], timeout: Duration) -> Result<()>;
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>>;
    fn pace(&mut self) {
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub fn encode(command: u8, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() + 3 > MAX_BODY {
        return Err("Bootloader frame exceeds its buffer".into());
    }
    let mut message = vec![command];
    message.extend_from_slice(payload);
    message.extend_from_slice(&image::crc16(&message).to_le_bytes());
    let mut frame = vec![1];
    for byte in message {
        if matches!(byte, 1 | 4 | 16) {
            frame.push(16);
        }
        frame.push(byte);
    }
    frame.push(4);
    Ok(frame)
}

#[derive(Default)]
pub struct Decoder {
    body: Vec<u8>,
    active: bool,
    escaped: bool,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if !self.active {
                if byte == 1 {
                    self.active = true;
                    self.escaped = false;
                    self.body.clear();
                }
                continue;
            }
            if self.escaped {
                self.escaped = false;
                self.body.push(byte);
            } else {
                match byte {
                    1 => {
                        self.body.clear();
                        self.escaped = false;
                    }
                    16 => {
                        self.escaped = true;
                        continue;
                    }
                    4 => {
                        self.active = false;
                        if self.body.len() < 3 {
                            return Err("Short bootloader response".into());
                        }
                        let tail = self.body.len() - 2;
                        let crc = u16::from_le_bytes([self.body[tail], self.body[tail + 1]]);
                        self.body.truncate(tail);
                        if image::crc16(&self.body) != crc {
                            return Err("Bootloader response CRC mismatch".into());
                        }
                        frames.push(std::mem::take(&mut self.body));
                        continue;
                    }
                    _ => self.body.push(byte),
                }
            }
            if self.body.len() > MAX_BODY {
                return Err("Oversized bootloader response".into());
            }
        }
        Ok(frames)
    }
}

/// A command timeout or malformed reply poisons this connection. Never replay a
/// possibly completed erase/program request, or finalize through a stale link.
pub struct Bootloader<R: Reports> {
    reports: R,
    decoder: Decoder,
    poisoned: bool,
}
impl<R: Reports> Bootloader<R> {
    pub fn new(reports: R) -> Result<Self> {
        let mut this = Self {
            reports,
            decoder: Decoder::default(),
            poisoned: false,
        };
        this.ready()?;
        Ok(this)
    }

    fn send(&mut self, command: u8, payload: &[u8], deadline: Instant) -> Result<()> {
        let frame = encode(command, payload)?;
        let chunks = frame.chunks(REPORT_BYTES);
        let count = chunks.len();
        for (index, bytes) in chunks.enumerate() {
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Bootloader command timed out")?;
            let mut report = [0; REPORT_BYTES];
            report[..bytes.len()].copy_from_slice(bytes);
            self.reports.write(&report, timeout)?;
            if index + 1 < count {
                self.reports.pace();
            }
        }
        Ok(())
    }

    pub fn command(&mut self, command: u8, payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err("Bootloader connection is stale; reconnect before continuing".into());
        }
        let result = (|| {
            let deadline = Instant::now() + timeout;
            self.send(command, payload, deadline)?;
            loop {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or("Bootloader command timed out")?;
                let frames = self.decoder.push(&self.reports.read(remaining)?)?;
                if frames.len() > 1 {
                    return Err("Multiple replies to one bootloader command".into());
                }
                if let Some(mut frame) = frames.into_iter().next() {
                    if frame[0] != command {
                        return Err("Unexpected bootloader response command".into());
                    }
                    frame.remove(0);
                    return Ok(frame);
                }
            }
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub fn ready(&mut self) -> Result<()> {
        let reply = self.command(1, &[], Duration::from_secs(5))?;
        if reply != [1, 0] {
            self.poisoned = true;
            return Err("Bootloader not in IC902 ready state".into());
        }
        Ok(())
    }

    pub fn range_crc(&mut self, address: u32, length: usize) -> Result<u16> {
        if length == 0
            || address < image::BASE
            || u64::from(address)
                .checked_add(length as u64)
                .is_none_or(|end| end > u64::from(image::BASE) + image::FLASH_SIZE as u64)
        {
            return Err("Read outside tablet flash refused".into());
        }
        let mut payload = address.to_le_bytes().to_vec();
        payload.extend_from_slice(&(length as u32).to_le_bytes());
        let reply = self.command(4, &payload, Duration::from_secs(5))?;
        let bytes: [u8; 2] = reply.try_into().map_err(|_| {
            self.poisoned = true;
            "Invalid range CRC response"
        })?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// CRC16/XMODEM is bijective for two bytes, allowing device flash reads
    /// through the bootloader's range-CRC command.
    pub fn read_bytes(
        &mut self,
        offset: usize,
        length: usize,
        mut progress: impl FnMut(usize),
    ) -> Result<Vec<u8>> {
        if !length.is_multiple_of(2)
            || offset
                .checked_add(length)
                .is_none_or(|end| end > image::FLASH_SIZE)
        {
            return Err("Invalid word-read range".into());
        }
        let inverse = inverse_crc();
        let mut bytes = Vec::with_capacity(length);
        for index in (0..length).step_by(2) {
            let crc = self.range_crc(image::BASE + (offset + index) as u32, 2)?;
            bytes.extend_from_slice(&inverse[usize::from(crc)]);
            if (index + 2).is_multiple_of(128) || index + 2 == length {
                progress(index + 2);
            }
        }
        Ok(bytes)
    }

    pub fn state(&mut self) -> Result<u32> {
        let bytes = self.read_bytes(image::STATE_OFFSET, 4, |_| {})?;
        let state = u32::from_le_bytes(bytes.try_into().map_err(|_| "Short boot state")?);
        if state > 2 {
            return Err("Unknown persistent boot state".into());
        }
        Ok(state)
    }

    /// Validate every application byte before ERASE. PROGRAM gets raw, bounded
    /// Intel HEX records; only application addresses are ever generated.
    /// Finalization deliberately belongs to the verified workflow, not here.
    pub fn program(&mut self, code: &[u8], mut progress: impl FnMut(usize)) -> Result<()> {
        let records = program_records(code)?;
        let erased = self.command(2, &[], Duration::from_secs(15))?;
        if !erased.is_empty() {
            self.poisoned = true;
            return Err("Unexpected erase reply".into());
        }
        let mut completed = 0;
        for record in records {
            let reply = self.command(3, &record, Duration::from_secs(5))?;
            if !reply.is_empty() {
                self.poisoned = true;
                return Err("Unexpected program reply".into());
            }
            if record[3] == 0 {
                completed += usize::from(record[0]);
            }
            progress(completed);
        }
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        if self.poisoned {
            return Err("Cannot finalize through a stale bootloader connection".into());
        }
        image::validate_vectors(&self.read_bytes(image::APP_OFFSET, 8, |_| {})?)?;
        // FINALIZE may disconnect before replying. Submit exactly once; the
        // workflow independently observes runtime re-enumeration afterwards.
        let result = self.send(5, &[], Instant::now() + Duration::from_secs(5));
        self.poisoned = true;
        result
    }
}

fn raw_record(address: u16, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut record = vec![data.len() as u8, (address >> 8) as u8, address as u8, kind];
    record.extend_from_slice(data);
    record.push(0u8.wrapping_sub(record.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))));
    record
}

pub fn program_records(code: &[u8]) -> Result<Vec<Vec<u8>>> {
    image::validate_application(code)?;
    let mut records = vec![raw_record(0, 4, &[8, 0])];
    for (index, bytes) in code.chunks(128).enumerate() {
        records.push(raw_record(
            (image::APP_OFFSET + index * 128) as u16,
            0,
            bytes,
        ));
    }
    records.push(raw_record(0, 1, &[]));
    Ok(records)
}

pub fn inverse_crc() -> &'static [[u8; 2]; 65536] {
    static INVERSE: OnceLock<Box<[[u8; 2]; 65536]>> = OnceLock::new();
    INVERSE.get_or_init(|| {
        let mut inverse = Box::new([[0; 2]; 65536]);
        for word in 0..=u16::MAX {
            let bytes = word.to_be_bytes();
            inverse[usize::from(image::crc16(&bytes))] = bytes;
        }
        inverse
    })
}

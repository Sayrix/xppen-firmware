use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use xppen_firmware_core::{
    Result,
    image::{self, APP_OFFSET, FLASH_SIZE, STATE_OFFSET},
    protocol::{self, Bootloader, Decoder, Reports},
};

const STOCK: &[u8] = include_bytes!("../../firmware/stock-deco-fun-xs/firmware.bin");
const BOOT: &[u8] = include_bytes!("../../tests/fixtures/ic902-bootloader.bin");

struct Tablet {
    memory: Vec<u8>,
    decoder: Decoder,
    replies: VecDeque<Vec<u8>>,
    commands: Arc<Mutex<Vec<u8>>>,
    upper: u32,
    fail_program: bool,
    wrong_command: bool,
    corrupt_reply: bool,
    fragment: usize,
    drop_reply: bool,
    duplicate_reply: bool,
}
impl Tablet {
    fn new() -> Self {
        let mut memory = vec![255; FLASH_SIZE];
        memory[..BOOT.len()].copy_from_slice(BOOT);
        memory[APP_OFFSET..APP_OFFSET + STOCK.len()].copy_from_slice(STOCK);
        Self {
            memory,
            decoder: Decoder::default(),
            replies: VecDeque::new(),
            commands: Arc::new(Mutex::new(vec![])),
            upper: 0,
            fail_program: false,
            wrong_command: false,
            corrupt_reply: false,
            fragment: 1,
            drop_reply: false,
            duplicate_reply: false,
        }
    }
    fn handle(&mut self, body: &[u8]) -> Result<()> {
        let (&command, payload) = body.split_first().ok_or("Empty request")?;
        self.commands.lock().unwrap().push(command);
        let reply = match command {
            1 => vec![1, 0],
            2 => {
                assert!(payload.is_empty());
                self.memory[APP_OFFSET..].fill(255);
                self.memory[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&2u32.to_le_bytes());
                vec![]
            }
            3 => {
                if self.fail_program {
                    return Err("Injected program disconnection".into());
                }
                assert_eq!(
                    payload
                        .iter()
                        .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
                    0
                );
                assert_eq!(payload.len(), usize::from(payload[0]) + 5);
                match payload[3] {
                    4 => {
                        assert_eq!(payload[0], 2);
                        self.upper = u32::from(u16::from_be_bytes([payload[4], payload[5]])) << 16;
                    }
                    0 => {
                        let address =
                            self.upper + u32::from(u16::from_be_bytes([payload[1], payload[2]]));
                        let offset = (address - image::BASE) as usize;
                        assert!(
                            offset >= APP_OFFSET && offset + usize::from(payload[0]) <= FLASH_SIZE
                        );
                        self.memory[offset..offset + usize::from(payload[0])]
                            .copy_from_slice(&payload[4..payload.len() - 1]);
                    }
                    1 => assert_eq!(payload[0], 0),
                    _ => panic!("Unsupported program record"),
                }
                vec![]
            }
            4 => {
                assert_eq!(payload.len(), 8);
                let offset =
                    (u32::from_le_bytes(payload[..4].try_into().unwrap()) - image::BASE) as usize;
                let length = u32::from_le_bytes(payload[4..].try_into().unwrap()) as usize;
                image::crc16(&self.memory[offset..offset + length])
                    .to_le_bytes()
                    .to_vec()
            }
            5 => {
                assert!(payload.is_empty());
                return Ok(());
            }
            _ => panic!("Unsupported wire command"),
        };
        let mut frame = protocol::encode(if self.wrong_command { 99 } else { command }, &reply)?;
        if self.corrupt_reply {
            frame[1] ^= 0x80;
        }
        if self.drop_reply {
            return Ok(());
        }
        if self.duplicate_reply {
            frame.extend(frame.clone());
        }
        for bytes in frame.chunks(self.fragment) {
            self.replies.push_back(bytes.to_vec());
        }
        Ok(())
    }
}
impl Reports for Tablet {
    fn write(&mut self, report: &[u8; 64], _: Duration) -> Result<()> {
        for frame in self.decoder.push(report)? {
            self.handle(&frame)?;
        }
        Ok(())
    }
    fn read(&mut self, _: Duration) -> Result<Vec<u8>> {
        self.replies
            .pop_front()
            .ok_or_else(|| "Injected HID timeout".into())
    }
    fn pace(&mut self) {}
}

#[test]
fn exact_framing_escape_boundaries_and_corruption() {
    assert_eq!(protocol::encode(1, &[]).unwrap(), [1, 16, 1, 33, 16, 16, 4]);
    assert_eq!(image::crc16(b"123456789"), 0x31c3);
    for size in [0, 1, 63, 64, 127, 997] {
        let payload: Vec<_> = (0..size)
            .map(|index| [0, 1, 4, 16, 255, 128][index % 6])
            .collect();
        let encoded = protocol::encode(3, &payload).unwrap();
        for chunk in [1, 2, 63, 64, 65] {
            let mut decoder = Decoder::default();
            let mut frames = vec![];
            for bytes in encoded.chunks(chunk) {
                frames.extend(decoder.push(bytes).unwrap());
            }
            assert_eq!(
                frames,
                [std::iter::once(3)
                    .chain(payload.iter().copied())
                    .collect::<Vec<_>>()]
            );
        }
    }
    assert!(protocol::encode(3, &[0; 998]).is_err());
    assert!(Decoder::default().push(&[1, 3, 0, 0, 4]).is_err());
    assert!(Decoder::default().push(&[1, 4]).is_err());
    let mut huge = vec![1];
    huge.extend([2; 1001]);
    assert!(Decoder::default().push(&huge).is_err());
}

#[test]
fn every_word_roundtrips_through_crc_oracle() {
    let inverse = protocol::inverse_crc();
    let mut seen = vec![false; 65536];
    for word in 0..=u16::MAX {
        let bytes = word.to_be_bytes();
        let crc = usize::from(image::crc16(&bytes));
        assert!(!seen[crc]);
        seen[crc] = true;
        assert_eq!(inverse[crc], bytes);
    }
}

#[test]
fn physical_flash_read_and_crc_match_real_fixture() {
    let tablet = Tablet::new();
    let expected = tablet.memory.clone();
    let mut boot = Bootloader::new(tablet).unwrap();
    let mut counts = vec![];
    let first = boot
        .read_bytes(0, FLASH_SIZE, |count| counts.push(count))
        .unwrap();
    let second = boot.read_bytes(0, FLASH_SIZE, |_| {}).unwrap();
    assert_eq!(first, expected);
    assert_eq!(first, second);
    assert_eq!(counts.last(), Some(&FLASH_SIZE));
    assert_eq!(
        boot.range_crc(image::BASE, FLASH_SIZE).unwrap(),
        image::crc16(&first)
    );
    assert!(boot.range_crc(image::BASE - 1, 2).is_err());
    assert!(
        boot.range_crc(image::BASE + FLASH_SIZE as u32 - 1, 2)
            .is_err()
    );
    assert!(boot.range_crc(image::BASE, 0).is_err());
    assert!(boot.range_crc(image::BASE, usize::MAX).is_err());
    assert!(boot.read_bytes(FLASH_SIZE - 1, 2, |_| {}).is_err());
    assert!(boot.read_bytes(0, 3, |_| {}).is_err());
}

#[test]
fn program_records_cover_exact_application_without_boot_addresses() {
    let records = protocol::program_records(STOCK).unwrap();
    assert_eq!(records[0], [2, 0, 0, 4, 8, 0, 0xf2]);
    assert_eq!(records.last().unwrap(), &[0, 0, 0, 1, 255]);
    let mut reconstructed = vec![];
    for (index, record) in records[1..records.len() - 1].iter().enumerate() {
        assert_eq!(
            u16::from_be_bytes([record[1], record[2]]) as usize,
            APP_OFFSET + index * 128
        );
        assert_eq!(record[3], 0);
        assert_eq!(
            record.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
        assert!(record[0] <= 128);
        reconstructed.extend_from_slice(&record[4..record.len() - 1]);
    }
    assert_eq!(reconstructed, STOCK);
    assert!(protocol::program_records(&vec![0; FLASH_SIZE]).is_err());
}

#[test]
fn writing_never_finalizes_and_program_failure_never_retries() {
    let mut tablet = Tablet::new();
    tablet.fail_program = true;
    let trace = tablet.commands.clone();
    let mut boot = Bootloader::new(tablet).unwrap();
    assert!(boot.program(STOCK, |_| {}).is_err());
    assert_eq!(*trace.lock().unwrap(), [1, 2, 3]);
    assert!(boot.finalize().is_err());
    assert!(boot.program(STOCK, |_| {}).is_err());
    // Failed PROGRAM cannot be replayed through the poisoned link.
    // Construct a fresh successful transport to inspect exact resulting bytes.
    let mut tablet = Tablet::new();
    tablet.fragment = 64;
    // The old application extends beyond the new one: all old tail bytes must
    // be erased, and all resident bytes except the boot request must survive.
    tablet.memory[APP_OFFSET + STOCK.len()..].fill(0x55);
    let before = tablet.memory.clone();
    let trace = tablet.commands.clone();
    let mut boot = Bootloader::new(tablet).unwrap();
    let mut counts = vec![];
    boot.program(STOCK, |count| counts.push(count)).unwrap();
    assert!(!trace.lock().unwrap().contains(&5));
    assert_eq!(counts.last(), Some(&STOCK.len()));
    assert_eq!(boot.state().unwrap(), 2);
    assert_eq!(
        boot.read_bytes(APP_OFFSET, STOCK.len(), |_| {}).unwrap(),
        STOCK
    );
    let mut expected = before;
    expected[APP_OFFSET..].fill(255);
    expected[APP_OFFSET..APP_OFFSET + STOCK.len()].copy_from_slice(STOCK);
    expected[STATE_OFFSET..STATE_OFFSET + 4].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(boot.read_bytes(0, FLASH_SIZE, |_| {}).unwrap(), expected);
    boot.finalize().unwrap();
    assert_eq!(
        trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&command| command == 5)
            .count(),
        1
    );
    assert!(boot.range_crc(image::BASE, 2).is_err());
}

#[test]
fn corrupt_or_unexpected_ready_reply_is_rejected() {
    for corrupt in [false, true] {
        let mut tablet = Tablet::new();
        tablet.corrupt_reply = corrupt;
        tablet.wrong_command = !corrupt;
        assert!(Bootloader::new(tablet).is_err());
    }
}

#[test]
fn invalid_application_cannot_erase_or_finalize() {
    let tablet = Tablet::new();
    let trace = tablet.commands.clone();
    let mut boot = Bootloader::new(tablet).unwrap();
    assert!(boot.program(&[0; 512], |_| {}).is_err());
    assert!(!trace.lock().unwrap().contains(&2));
    assert_eq!(boot.state().unwrap(), 1);
    let mut tablet = Tablet::new();
    tablet.memory[APP_OFFSET..APP_OFFSET + 8].fill(255);
    let trace = tablet.commands.clone();
    let mut boot = Bootloader::new(tablet).unwrap();
    assert!(boot.finalize().is_err());
    assert!(!trace.lock().unwrap().contains(&5));
}

#[test]
fn missing_and_ambiguous_replies_do_not_trigger_retries() {
    for duplicate in [false, true] {
        let mut tablet = Tablet::new();
        tablet.drop_reply = !duplicate;
        tablet.duplicate_reply = duplicate;
        tablet.fragment = 64;
        let trace = tablet.commands.clone();
        assert!(Bootloader::new(tablet).is_err());
        assert_eq!(*trace.lock().unwrap(), [1]);
    }
}

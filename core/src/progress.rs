use crate::{Result, utility::Runner};

/// Workflow milestones are separate from human-readable utility logs. A successful
/// transfer counter does not mark a step complete until its checks also succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Prepare,
    Identify,
    SaveRollback,
    EnterDfu,
    ReadBackup,
    CheckBackup,
    SaveBackup,
    CheckCurrent,
    CheckResident,
    WriteFirmware,
    ReconnectDfu,
    CheckWritten,
    SaveVerification,
    ReturnRuntime,
    SaveLog,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Prepare => "Prepare operation",
            Self::Identify => "Check tablet connection",
            Self::SaveRollback => "Save recovery files",
            Self::EnterDfu => "Enter DFU mode",
            Self::ReadBackup => "Read firmware twice",
            Self::CheckBackup => "Verify backup",
            Self::SaveBackup => "Save verified backup",
            Self::CheckCurrent => "Match backup against tablet",
            Self::CheckResident => "Check resident bootloader",
            Self::WriteFirmware => "Write firmware",
            Self::ReconnectDfu => "Check bootloader after write",
            Self::CheckWritten => "Verify flashed firmware",
            Self::SaveVerification => "Save verification results",
            Self::ReturnRuntime => "Return tablet to normal mode",
            Self::SaveLog => "Save operation log",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Started(Stage),
    Advanced {
        stage: Stage,
        completed: usize,
        total: usize,
    },
    Finished(Stage),
    Failed(Stage),
}

/// Emits stage events around an operation without altering its execution.
pub fn step<T, R: Runner>(
    runner: &mut R,
    stage: Stage,
    action: impl FnOnce(&mut R) -> Result<T>,
) -> Result<T> {
    runner.progress(Progress::Started(stage));
    let result = action(runner);
    runner.progress(if result.is_ok() {
        Progress::Finished(stage)
    } else {
        Progress::Failed(stage)
    });
    result
}

/// Parses carriage-return-updated transfer counters from separate log streams.
/// Partial lines are buffered until a complete counter is available.
pub struct TransferDecoder {
    stage: Stage,
    lines: [String; 2],
    discarded: [bool; 2],
}

impl TransferDecoder {
    pub fn new(stage: Stage) -> Self {
        Self {
            stage,
            lines: [String::new(), String::new()],
            discarded: [false; 2],
        }
    }

    pub fn push(&mut self, stream: usize, fragment: &str) -> Vec<Progress> {
        let mut events = Vec::new();
        for ch in fragment.chars() {
            if matches!(ch, '\r' | '\n') {
                if !self.discarded[stream]
                    && let Some(event) = parse_transfer(self.stage, &self.lines[stream])
                {
                    events.push(event);
                }
                self.lines[stream].clear();
                self.discarded[stream] = false;
            } else if !self.discarded[stream] {
                if self.lines[stream].len() < 1024 {
                    self.lines[stream].push(ch);
                } else {
                    self.lines[stream].clear();
                    self.discarded[stream] = true;
                }
            }
        }
        events
    }
}

fn parse_transfer(stage: Stage, line: &str) -> Option<Progress> {
    let line = line.trim();
    let (percent, counter) = match stage {
        Stage::ReadBackup => {
            let (percent, rest) = line.strip_prefix('[')?.split_once("%]")?;
            (percent, rest.split_whitespace().next()?)
        }
        Stage::WriteFirmware => {
            let mut words = line.strip_prefix("[PROGRAM]")?.split_whitespace();
            (words.next()?.strip_suffix('%')?, words.next()?)
        }
        _ => return None,
    };
    let percent: f64 = percent.parse().ok()?;
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return None;
    }
    let (completed, total) = counter.split_once('/')?;
    let completed: usize = completed.parse().ok()?;
    let total: usize = total.parse().ok()?;
    if total == 0 || completed > total || total > crate::image::FLASH_SIZE {
        return None;
    }
    if stage == Stage::ReadBackup && total != crate::image::FLASH_SIZE {
        return None;
    }
    // Progress uses byte and chunk counts to avoid rounded display percentages.
    Some(Progress::Advanced {
        stage,
        completed,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_survive_every_stream_fragment_boundary_and_crlf() {
        for (stage, line, completed, total) in [
            (
                Stage::ReadBackup,
                "[50.0%] 32768/65536  @ 0x08007FFF  250 B/s  ETA 2m10s\r\n",
                32768,
                65536,
            ),
            (
                Stage::WriteFirmware,
                "[PROGRAM] 50.0%  946/1893     \r",
                946,
                1893,
            ),
        ] {
            for split in 0..=line.len() {
                let mut decoder = TransferDecoder::new(stage);
                let mut events = decoder.push(0, &line[..split]);
                assert!(decoder.push(1, "unrelated stderr\n").is_empty());
                events.extend(decoder.push(0, &line[split..]));
                assert_eq!(
                    events,
                    [Progress::Advanced {
                        stage,
                        completed,
                        total
                    }]
                );
            }
        }
    }

    #[test]
    fn ignores_invalid_cross_stage_and_incomplete_progress() {
        for line in [
            "[PROGRAM] NaN% 1/2",
            "[PROGRAM] 20% 3/2",
            "[PROGRAM] 0% 0/0",
            "[PROGRAM] 101% 1/2",
            "[INFO] 50% 1/2",
        ] {
            assert!(parse_transfer(Stage::WriteFirmware, line).is_none());
        }
        assert!(parse_transfer(Stage::ReadBackup, "[50%] 1/2").is_none());
        assert!(parse_transfer(Stage::CheckWritten, "[PROGRAM] 50% 1/2").is_none());
        let mut decoder = TransferDecoder::new(Stage::ReadBackup);
        assert!(decoder.push(0, "[50%] 32768/65536").is_empty());
        assert_eq!(decoder.push(0, "\r").len(), 1);
        assert!(
            decoder
                .push(0, &format!("{}[50%] 32768/65536\r", "x".repeat(1500)))
                .is_empty()
        );
        assert_eq!(decoder.push(0, "[100%] 65536/65536\r").len(), 1);
    }
}

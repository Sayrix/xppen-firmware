use crate::{Result, progress::Progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Runtime,
    Dfu,
}

/// Request interface shared by workflow dispatch, tests and session logging.
/// NativeDevice executes requests in-process.
pub trait Runner {
    fn run(&mut self, args: &[String]) -> Result<String>;
    fn note(&mut self, text: &str);
    fn progress(&mut self, _: Progress) {}
}

pub const SELECTORS: [&str; 4] = ["--pid", "0xFFF1", "--runtime-pid", "0x0930"];
pub fn arguments(command: &str, extra: &[String]) -> Vec<String> {
    std::iter::once(command.to_string())
        .chain(extra.iter().cloned())
        .chain(SELECTORS.map(String::from))
        .collect()
}

/// Fail closed on unknown models and bootloaders, including unsupported FFF3.
pub fn identify(output: &str) -> Result<Mode> {
    if output.contains("Bootloader: TABLET_DFU200718 [28BD:FFF1], IC 902, framed")
        && output.contains("Runtime VID:PID: 28BD:0930")
        && output.contains("Flash size: 64 KiB")
        && output.contains("Flash range: 0x08000000 .. 0x0800FFFF")
    {
        return Ok(Mode::Dfu);
    }
    if output.contains("Runtime device: 4 inch PenTablet [28BD:0930]")
        && output.contains("manufacturer=UGTABLET serial=000000")
    {
        return Ok(Mode::Runtime);
    }
    Err("Device identity is not the verified Deco Fun XS / IC902 / FFF1 target".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrong_device_is_refused() {
        assert_eq!(identify("Runtime device: 4 inch PenTablet [28BD:0930]\nIdentity: manufacturer=UGTABLET serial=000000").unwrap(), Mode::Runtime);
        assert!(identify("Runtime device: 4 inch PenTablet [28BD:0931]\nIdentity: manufacturer=UGTABLET serial=000000").is_err());
        assert!(identify("Bootloader: TABLET_DFU200718 [28BD:FFF3], IC 902, framed").is_err());
    }
}

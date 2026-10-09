pub mod image;
pub mod native;
pub mod progress;
pub mod protocol;
pub mod utility;
#[cfg(windows)]
mod windows_hid;
pub mod workflow;

pub type Result<T> = std::result::Result<T, String>;

#[cfg(windows)]
use windows_hid as platform_hid;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod usb_hid;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use usb_hid as platform_hid;

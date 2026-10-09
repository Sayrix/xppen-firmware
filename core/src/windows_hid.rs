//! Minimal Windows HID transport. All unsafe code stays at the OS boundary;
//! every overlapped transfer is completed/cancelled before its buffer is freed.
use crate::{
    Result,
    protocol::{REPORT_BYTES, Reports},
};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, Threading::*},
    },
    core::GUID,
};

pub struct Handle(HANDLE);
impl Handle {
    fn new(raw: HANDLE) -> Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(last_error("Open Windows handle"))
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub struct OperationLock {
    handle: Handle,
}
impl OperationLock {
    /// An ephemeral named mutex also serializes copies in different folders.
    pub fn acquire() -> Result<Self> {
        let name: Vec<u16> = "Local\\DecoFunXSFlasher-28BD-0930-FFF1\0"
            .encode_utf16()
            .collect();
        let handle = Handle::new(unsafe { CreateMutexW(null(), 0, name.as_ptr()) })?;
        match unsafe { WaitForSingleObject(handle.0, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { handle }),
            WAIT_TIMEOUT => Err("Another native flasher operation is running".into()),
            _ => Err(last_error("Acquire tablet operation lock")),
        }
    }
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.handle.0);
        }
    }
}

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Preparsed(PHIDP_PREPARSED_DATA);
impl Drop for Preparsed {
    fn drop(&mut self) {
        unsafe {
            HidD_FreePreparsedData(self.0);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    path: Vec<u16>,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
}

fn last_error(action: &str) -> String {
    let error = std::io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(2 | 433 | 1167)) {
        // A path can disappear between enumeration and metadata opening while
        // USB re-enumerates. Only discovery waits treat this as transient.
        format!("No XP-Pen runtime device or supported DFU interface found ({action}: {error})")
    } else {
        format!("{action}: {error}")
    }
}

fn open(path: &[u16], access: u32, flags: u32) -> Result<Handle> {
    // Shared access permits Windows' normal HID driver to remain installed.
    Handle::new(unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    })
}

fn string(
    handle: &Handle,
    get: unsafe extern "system" fn(HANDLE, *mut std::ffi::c_void, u32) -> bool,
) -> Result<String> {
    let mut text = [0u16; 128];
    if !unsafe {
        get(
            handle.0,
            text.as_mut_ptr().cast(),
            size_of_val(&text) as u32,
        )
    } {
        return Err(last_error("Read HID identity string"));
    }
    let end = text
        .iter()
        .position(|&c| c == 0)
        .ok_or("Unterminated HID identity string")?;
    String::from_utf16(&text[..end]).map_err(|e| e.to_string())
}

/// Enumerate only the two supported PIDs. Metadata handles never issue input,
/// output or indexed-string requests; C8 is reserved for explicit DFU entry.
pub fn enumerate() -> Result<Vec<Device>> {
    let mut guid: GUID = unsafe { zeroed() };
    unsafe {
        HidD_GetHidGuid(&mut guid);
    }
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw == -1 {
        return Err(last_error("Enumerate HID interfaces"));
    }
    let set = DeviceSet(raw);
    let mut devices = Vec::new();
    for index in 0..4096 {
        let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
        interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, null(), &guid, index, &mut interface) } == 0
        {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok(devices);
            }
            return Err(last_error("Enumerate HID interface"));
        }
        let mut required = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
        }
        if !(8..=32768).contains(&required) {
            return Err("Invalid HID interface path size".into());
        }
        // Vec<u64> gives the detail structure aligned storage; DevicePath has
        // offset 4 even though cbSize is 8 for the Windows x64 ABI.
        let mut storage = vec![0u64; (required as usize).div_ceil(8)];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                detail,
                required,
                null_mut(),
                null_mut(),
            )
        } == 0
        {
            return Err(last_error("Read HID interface path"));
        }
        let path = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!((*detail).DevicePath).cast::<u16>(),
                (required as usize - 4) / 2,
            )
        };
        let end = path
            .iter()
            .position(|&c| c == 0)
            .ok_or("Unterminated HID path")?;
        let path = path[..=end].to_vec();
        let label = String::from_utf16_lossy(&path[..end]).to_ascii_lowercase();
        if !label.contains("vid_28bd")
            || !(label.contains("pid_0930") || label.contains("pid_fff1"))
        {
            continue;
        }
        let handle = open(&path, 0, 0)?;
        let mut attributes: HIDD_ATTRIBUTES = unsafe { zeroed() };
        attributes.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        if !unsafe { HidD_GetAttributes(handle.0, &mut attributes) } {
            return Err(last_error("Read HID attributes"));
        }
        if attributes.VendorID != 0x28bd || !matches!(attributes.ProductID, 0x0930 | 0xfff1) {
            continue;
        }
        let mut data = 0;
        if !unsafe { HidD_GetPreparsedData(handle.0, &mut data) } {
            return Err(last_error("Read HID report description"));
        }
        let data = Preparsed(data);
        let mut caps = HIDP_CAPS::default();
        if unsafe { HidP_GetCaps(data.0, &mut caps) } != HIDP_STATUS_SUCCESS {
            return Err("Invalid HID capabilities".into());
        }
        let wanted = if attributes.ProductID == 0xfff1 {
            caps.UsagePage == 0xff00 && caps.Usage == 1
        } else {
            caps.UsagePage == 0xff0a && caps.Usage == 1
        };
        if !wanted {
            continue;
        }
        if attributes.ProductID == 0xfff1
            && (caps.InputReportByteLength != 65 || caps.OutputReportByteLength != 65)
        {
            return Err(
                "FFF1 must expose 64-byte input/output payloads plus Windows report ID padding"
                    .into(),
            );
        }
        let device = Device {
            path,
            pid: attributes.ProductID,
            manufacturer: if attributes.ProductID == 0x0930 {
                string(&handle, HidD_GetManufacturerString)?
            } else {
                String::new()
            },
            product: string(&handle, HidD_GetProductString)?,
            // Some resident boot descriptors have no serial string. Runtime
            // identity requires it; DFU is gated by product/caps and boot hash.
            serial: if attributes.ProductID == 0x0930 {
                string(&handle, HidD_GetSerialNumberString)?
            } else {
                String::new()
            },
        };
        if device.pid == 0x0930
            && (device.manufacturer != "UGTABLET"
                || device.product != "4 inch PenTablet"
                || device.serial != "000000")
        {
            return Err("Runtime identity is not the verified Deco Fun XS".into());
        }
        if device.pid == 0xfff1 && device.product != "TABLET_DFU200718" {
            return Err("Unrecognized FFF1 bootloader product".into());
        }
        devices.push(device);
    }
    Err("HID enumeration exceeded its interface limit".into())
}

pub fn target() -> Result<Device> {
    let mut devices = enumerate()?;
    if devices.len() > 1 {
        return Err("Multiple supported tablets found; attach only one".into());
    }
    devices
        .pop()
        .ok_or_else(|| "No XP-Pen runtime device or supported DFU interface found".into())
}

pub fn enter_dfu(device: &Device) -> Result<()> {
    if device.pid != 0x0930 {
        return Err("DFU entry requires verified runtime identity".into());
    }
    let handle = open(&device.path, 0, 0)?;
    let mut buffer = [0u16; 6];
    // Helper 0x1400330B7/0x1400330BF: index C8, exactly 12 bytes.
    // Stock descriptor handler persists the request and resets. The expected
    // disconnect can fail this API; re-enumeration, not its return value,
    // establishes that DFU entry worked. Never retry the request automatically.
    let ok = unsafe { HidD_GetIndexedString(handle.0, 0xc8, buffer.as_mut_ptr().cast(), 12) };
    if !ok {
        let error = unsafe { GetLastError() };
        if !matches!(error, 0 | 2 | 6 | 31 | 995 | 1167) {
            return Err(format!(
                "DFU descriptor request failed (Win32 error {error})"
            ));
        }
    }
    Ok(())
}

pub struct HidReports {
    handle: Handle,
}
impl HidReports {
    pub fn open(device: &Device) -> Result<Self> {
        if device.pid != 0xfff1 {
            return Err("Report transport requires FFF1".into());
        }
        let handle = open(
            &device.path,
            GENERIC_READ | GENERIC_WRITE,
            FILE_FLAG_OVERLAPPED,
        )?;
        if !unsafe { HidD_FlushQueue(handle.0) } {
            return Err(last_error("Flush old HID input"));
        }
        Ok(Self { handle })
    }

    fn transfer(&mut self, buffer: &mut [u8], write: bool, timeout: Duration) -> Result<usize> {
        let event = Handle::new(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let mut transferred = 0;
        // The buffer, event and OVERLAPPED remain alive until completion.
        // Failed waits cancel and drain the request before leaving this scope.
        let ok = unsafe {
            if write {
                WriteFile(
                    self.handle.0,
                    buffer.as_ptr(),
                    buffer.len() as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            } else {
                ReadFile(
                    self.handle.0,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            }
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(format!(
                    "{} failed (Win32 error {error})",
                    if write { "WriteFile" } else { "ReadFile" }
                ));
            }
            let millis = timeout.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32;
            let waited = unsafe { WaitForSingleObject(event.0, millis) };
            if waited != WAIT_OBJECT_0 {
                let error = if waited == WAIT_TIMEOUT {
                    "HID transfer timed out".to_string()
                } else {
                    last_error("Wait for HID transfer")
                };
                unsafe {
                    CancelIoEx(self.handle.0, &overlapped);
                    GetOverlappedResult(self.handle.0, &overlapped, &mut transferred, 1);
                }
                return Err(error);
            }
            if unsafe { GetOverlappedResult(self.handle.0, &overlapped, &mut transferred, 0) } == 0
            {
                return Err(last_error("Complete HID transfer"));
            }
        }
        Ok(transferred as usize)
    }
}

impl Reports for HidReports {
    fn write(&mut self, report: &[u8; REPORT_BYTES], timeout: Duration) -> Result<()> {
        let mut buffer = [0u8; REPORT_BYTES + 1];
        buffer[1..].copy_from_slice(report);
        if self.transfer(&mut buffer, true, timeout)? != buffer.len() {
            return Err("Short HID write; request will not be replayed".into());
        }
        Ok(())
    }
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut buffer = [0u8; REPORT_BYTES + 1];
        let count = self.transfer(&mut buffer, false, timeout)?;
        if count != buffer.len() || buffer[0] != 0 {
            return Err("Unexpected HID input report length or ID".into());
        }
        Ok(buffer[1..].to_vec())
    }
}

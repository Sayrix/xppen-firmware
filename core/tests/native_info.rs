use std::path::Path;
use xppen_firmware_core::{
    native::NativeDevice,
    utility::{Runner, arguments, identify},
    workflow::{DeviceLock, new_session},
};

/// An optional transport check: INFO reads identity without entering DFU.
#[test]
#[ignore = "Requires a connected Deco Fun XS; sends identity reads only"]
fn native_read_only_identity() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let _lock = DeviceLock::acquire(&root.join("backups")).unwrap();
    let session = new_session(&root.join("backups")).unwrap();
    let mut utility = NativeDevice::new(&session.join("commands.log"), |_| {}, |_| {}).unwrap();
    let output = utility.run(&arguments("info", &[])).unwrap();
    let mode = identify(&output).unwrap();
    println!(
        "Read-only native identity: {mode:?}; log: {}",
        session.display()
    );
}

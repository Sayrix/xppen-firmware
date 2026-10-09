# XPPen Firmware

A desktop firmware flasher for the **XPPen Deco Fun XS (CT430)**, built in Rust
with GPUI. Supports firmware backup, installation and restoration on Windows,
Linux and macOS.

The application includes four firmware options:

| Firmware | Position rate | Pressure acquisition |
| --- | --- | --- |
| Stock | ~240 Hz | Every position cycle |
| No smoothing | ~240 Hz | Every position cycle |
| Optimized, full pressure rate | ~280 Hz | Every position cycle |
| Optimized, reduced pressure rate | ~330 Hz | Every third position cycle |

Rates vary with pen angle and contact. See the [firmware reference](firmware/README.md)
for acquisition settings and measurement details.

> **Experimental software. Firmware installation can permanently brick the
> tablet and may void its warranty. Recovery is not guaranteed, even with a
> backup. Use is at the user's own risk.**

## Installation

Download and extract the release package matching the operating system and CPU.

| Platform | Application | Requirements |
| --- | --- | --- |
| Windows x64 | `xppen-firmware.exe` | Keep the bundled `vcruntime140.dll` beside the executable. |
| Linux x64 / ARM64 | `./xppen-firmware` | glibc 2.39+; compatible Wayland/X11, fontconfig, xkbcommon and Vulkan libraries. |
| macOS Intel / Apple Silicon | `XPPen Firmware.app` | macOS 13+; packages use ad hoc signing and are not notarized. |

### Device access

**Linux:** the bundled `69-xppen-firmware.rules` grants USB access to the active
desktop user. Install the rule in `/etc/udev/rules.d/`, reload the udev rules,
and reconnect the tablet if device access is denied.

**macOS:** Gatekeeper restrictions apply to downloaded bundles. Access to
kernel-owned HID interfaces can require elevated USB capture privileges.
The application provides no automatic elevation or privileged helper.

## Usage

1. Close OpenTabletDriver, XPPen software and other tablet applications.
2. Launch the flasher and select **Create a backup**.
3. Select a firmware option. Hover over its information icon for details.
4. Select **Flash selected firmware** and accept the risk warning to proceed.

Keep the USB cable connected throughout each operation. Backups read the full
64 KiB flash twice and verify the results. Firmware installation writes the
application region and verifies device block CRCs before returning to normal
mode. The bootloader is preserved.

**Load backup** imports a full 64 KiB backup. **Restore saved backup** restores
its application region. Failed writes require an explicit recovery operation.

### Backup locations

Backups and operation logs are stored locally:

| Platform | Location |
| --- | --- |
| Windows | `backups/` beside the executable |
| Linux | `$XDG_DATA_HOME/xppen-firmware/backups`, or `~/.local/share/xppen-firmware/backups` |
| macOS | `~/Library/Application Support/xppen-firmware/backups` |

## Compatibility

Supported hardware is restricted to the Deco Fun XS runtime `28BD:0930` and
IC902 bootloader `28BD:FFF1`. Device identity, flash bounds, application vectors
and the resident bootloader hash are checked before installation.

Transport coverage includes unit tests, simulated protocol tests, a Windows
device identity check and GUI startup checks. End-to-end DFU, backup,
installation and recovery remain pending hardware validation. The Linux/macOS
USB backend also remains pending device testing.

## Development

The project uses Rust 1.97.0 and GPUI 0.2.2. Linux build dependencies are listed
in [the release workflow](.github/workflows/release.yml). macOS builds use the
system Metal framework for shader compilation.

```sh
cargo test --workspace --release --locked
cargo build --release --locked
cargo xtask package 0.1.0 --target x86_64-pc-windows-msvc
```

For Linux or macOS, replace the target triple and build with
`cargo build --release --locked --target TARGET` before packaging. Available targets:

- `x86_64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

The Rust `xtask` tool handles versioning, packaging, dependency notices and
checksums. It uses Windows tar, Linux tar or macOS ditto. Packaging reads cached
Cargo sources; Cargo commands support `--offline` when dependencies are cached.

`cargo test -p xppen-firmware-core --locked` runs core tests without the GUI.
Live-device tests are ignored by default. `--smoke-test` runs a brief GUI check
with device operations disabled. CI uses simulated tests without tablet access.

## License

Application source is licensed under MIT. Tablet firmware is distributed as
BIN/HEX files; firmware source and generation tools are not included.
Firmware, binary test fixtures and third-party assets have separate licensing
terms. See [LICENSE-SCOPE.md](LICENSE-SCOPE.md) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

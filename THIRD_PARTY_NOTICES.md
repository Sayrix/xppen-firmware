# Third-party components

| Component | License / distribution terms |
| --- | --- |
| GPUI 0.2.2 | Apache-2.0 |
| rusb 0.9.4 | MIT; Linux/macOS USB backend |
| libusb 1.0.27 | LGPL-2.1-or-later; bundled on Linux/macOS |
| Lato | SIL OFL 1.1; copyright 2010–2011 tyPoland Lukasz Dziedzic, Reserved Font Name "Lato" |
| `vcruntime140.dll` | Microsoft Visual C++ runtime redistribution terms; Windows packages |
| Tablet illustration | Separate third-party imagery and trademark rights |

Release packages contain a dependency inventory in `third-party/DEPENDENCIES.json`
and available license, copyright and notice files from the resolved crate sources.
Lato's complete license is included as `Lato-OFL.txt`.

Unix packages include libusb's source and build bindings under
`third-party/libusb1-sys-0.7.0/source`, including the full `COPYING` notice.
The application source and build tools support rebuilding and relinking with
modified libusb. Windows packages include the Visual Studio x64 redistributable
`vcruntime140.dll` beside the application executable.

Firmware and the bootloader test fixture are governed by the exclusions in
[LICENSE-SCOPE.md](LICENSE-SCOPE.md). The dependency inventory does not grant
additional rights to upstream components. Product names and trademarks remain
the property of their respective owners.

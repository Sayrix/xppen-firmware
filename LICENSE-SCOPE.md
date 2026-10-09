# License scope

The MIT license in [LICENSE](LICENSE) covers the application source in `src/`
and `core/`, the Rust release tool in `xtask/`, the wrappers in `scripts/`,
the GitHub workflow and original project documentation.

The following components are excluded:

| Component | Terms |
| --- | --- |
| `firmware/` binaries | No separate open-source license is granted. Rights in the original firmware remain with their respective holders. Firmware source and disassembly are not distributed. |
| `tests/fixtures/ic902-bootloader.bin` | Publicly sourced test fixture with provenance recorded in the [fixture reference](tests/fixtures/README.md). Excluded from application packages and flashing payloads. |
| `assets/Lato-*.ttf` | SIL Open Font License 1.1; see `assets/Lato-OFL.txt`. |
| `assets/tablet.png` | Third-party illustration. The application license grants no rights to its imagery or trademarks. |
| Third-party libraries and Microsoft runtime | Respective upstream licenses and distribution terms apply. |

These exclusions also apply to firmware and assets embedded in the executable.

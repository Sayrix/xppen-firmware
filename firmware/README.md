# Firmware reference

Firmware images target the Deco Fun XS CT430 / IC902. Each directory contains
an application image in binary (`firmware.bin`) and Intel HEX (`firmware.hex`)
formats, starting at `0x08003800`. The resident bootloader is preserved.

| Directory | Position rate | Pressure acquisition | Excitation |
| --- | --- | --- | --- |
| `stock-deco-fun-xs` | ~240 Hz | Every position cycle | 37 periods |
| `deco-fun-xs-no-smoothing` | ~240 Hz | Every position cycle | 37 periods |
| `deco-fun-xs-no-smoothing-full-pressure-gpio-reset-excitation-31` | ~280 Hz | Every position cycle | 31 periods |
| `deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31` | ~330 Hz; 337 Hz measured | Every third cycle; ~110–112 Hz at these position rates | 31 periods |
| `deco-fun-xs-light-smoothing-8ms-4ms-pressure-every-3-cycles-gpio-reset-excitation-31` | ~330 Hz target; filtered variant not independently measured | Every third cycle | 31 periods |

## Acquisition settings

- **Stock:** original coordinate smoothing, acquisition timing and pressure processing.
- **240 Hz no smoothing:** bypasses main coordinate smoothing while preserving
  stock acquisition, pressure processing, timing and downstream movement gates.
- **280 Hz full pressure:** bypasses smoothing and optimizes calculations, copy
  loops, GPIO resets and acquisition bookkeeping, with reduced diagnostic overhead.
  Position and pressure are acquired every cycle.
- **330 Hz reduced pressure:** includes the same optimizations, with pressure
  acquisition scheduled every third position cycle.

- **330 Hz light smoothing:** uses the reduced-pressure optimized build with an
  adaptive coordinate filter shared by hover and drag. It targets approximately
  8 ms of tracking lag for small movements or near rest, easing toward 4 ms for
  faster movement and larger deviations from the filtered position. Time between
  scans determines the weight of new coordinates. The earlier stock averaging
  remains for other internal consumers, and downstream movement gates remain.
  The 4–8 ms figures describe approximate added filter lag, not measured total
  input latency. Added processing may affect the achieved scan rate.

All variants retain pen button support. The optimized variants use 31 excitation
periods rather than the stock 37. This changes acquisition characteristics and
can affect chatter and signal quality.

## Measurements and integrity

Rates were measured on one tablet and vary with pen angle and contact. They
describe scan cadence, not guaranteed USB delivery rates. The reduced pressure
rate is calculated from the acquisition schedule rather than independently measured.

The application embeds checksum-pinned BIN images. Tests verify that each HEX
matches its paired BIN. `SHA256SUMS` records checksums for both formats.

Firmware images are excluded from the application's MIT license. Firmware source
and generation tools are not distributed. See [license scope](../LICENSE-SCOPE.md).

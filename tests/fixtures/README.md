# IC902 bootloader test fixture

`ic902-bootloader.bin` is a 14,336-byte Deco Fun XS IC902 bootloader sample from
[NiceAesth/xppen-dumps](https://github.com/NiceAesth/xppen-dumps/tree/96297689eab73789618f31edf6738e4acab8832a/stock/Deco%20Fun%20XS).

| Field | Value |
| --- | --- |
| Upstream revision | `96297689eab73789618f31edf6738e4acab8832a` |
| Upstream path | `stock/Deco Fun XS/ic902-bootloader.bin` |
| SHA-256 | `fa7bf148e5ee63272e1ed11d41dee58091a7e4a2845383ab8aa5b52b41be2640` |

Tests use the fixture to verify the bootloader hash check, descriptor parsing
and protocol behavior. It is excluded from application packages and flashing
payloads, and is not covered by the application MIT license.

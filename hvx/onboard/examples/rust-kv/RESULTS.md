# rust-kv under hydo (2026-10-24)

rkv (`src/main.rs`) is a small Rust key/value server that uses the Rust assertion SDK
(`sdk/rust`, crate `hydo-assert`). Its own records -- 6 `declare!` entries and 7 call-site
macros, three of them BUGGIFY points -- appear in the consoles next to the manifest's invariant
`rkv.oracle`, and `hydo_assert_report` and `explore.py` read them unchanged.

## Mac backend (M2 Pro, the Mac release `hydo-mac-0.1.0` installed in a scratch prefix, with
## this branch's `hydo-build`, `hydo-init-buggify.sh` and `manifest_space.py` copied in)

`./build-rkv.sh aarch64` (Rust 1.90.0, `aarch64-unknown-linux-musl`, rust-lld): rkv
`e5d97b55…`, two clean builds identical. `hydo build hydo.yaml` twice: initramfs
`500ecb2f93288a1d…`, data disk `e19be506…`, identical.

`hydo run`, each schedule twice (fresh boot each time), every pair identical:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK, acked=20 | `64de839903b96e45` |
| `disk=cut \| run C=12` | OK, acked=12 (fdatasync before the reply) | `6a7e5947973e5770` |
| `disk=cut \| run C=12 S=never` | VIOLATION, acked=12 lost=12 (no fdatasync) | `6f29e36a1cd516be` |
| `disk=none \| run K=rkv@8+2` | OK, acked=18 | `375ca29fb8f1ab23` |
| `disk=none \| run B=5abc,1000,500` | OK, acked=2 (every point enabled, half the hits fire) | `de31ab2024fb243f` |
| `disk=none \| run B=1234` | OK, acked=20 (FoundationDB rates: only `rkv.fsync.before` enabled) | `244342f8f8aaa1e9` |
| `disk=none \| run K=rkv@8+2 B=1234` | OK, acked=18 | `0be47ac30f845cb6` |
| `disk=none \| run P=rkv@5+3` | OK, acked=17 | `d45afcdc04b39994` |

`hydo_assert_report` over these 20 consoles: `rkv.acked_durable` FAIL (the `S=never` cut),
`rkv.recovered`, `rkv.log_line_parses`, `rkv.client.no_reply`, the three
`buggify.rkv.*` points and `rkv.check.retried_get` exercised; `rkv.torn_tail` NOT EXERCISED
(declared by a macro site that never ran, listed through the catalog).

`hydo explore --seeds 1-16 --strategy swarm --buggify --workers 2`, run twice: all 16
schedules carry `B=`, the 16 fingerprints of the two explorations are identical, and so are
the two property tables. 8 failures, all reproduced, all expected classes: lying disk 4
(minimal `disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20`, `9a3c7a8656c131ff` twice),
relaxed knob 3 (minimal `disk=cut dseed=dd8d1ecadadb959b | run C=9 S=never`,
`654c8423f241e386` twice), I/O errors 1. The fault points fired in the exploration:
`lost_reply` 6, `fsync.before` 4, `slow_fsync` 1.

Found on the way (fixed in `src/main.rs`): with `B=5abc,1000,500` the first version of
`rkv check` reported a lost key that was not lost -- its GET timed out while the server was
still busy with PUTs whose clients had given up (a slow fdatasync), and the oracle counted the
timeout as a loss. It now asks again for up to 30 s and exits 2 ("could not check") if the
server never answers.

## Bare-metal backend (x86_64, `~/hydo/build/explore-ob-rust-kv-v2-disk.img` `118c9f2c…`)

Built on the build machine with the pinned `hvx_seedrng` (`c2a440c5…`) from commit
`ff4a4dd` (an earlier rkv, `721b61af…`, without the oracle retry, and the manifest with a
`params:` path); two builds identical. `hvx_explore one`, twice each: `disk=none dseed=1 | run`
OK `ecfd264f2e19865f`; `disk=cut dseed=1 | run C=12` OK `0d8d109d8a1eca66`. The SDK's records
are in both consoles (`hydo_assert_report`: 0 violations). The other schedules were moved to
the Mac because the box was crowded.

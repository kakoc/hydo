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
the Mac because the bare-metal machine was crowded.

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**x86_64 (hvx), rust-kv**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `389bc482e51e2194` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `d4604820a762223b` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `1591a9e914967b33` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `d4d56cb09417de82` | yes |

**Mac (hvm), rust-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `5777c136b33b2224` | `41f2ebf5d53abd98` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `74c66bb13973fd34` | `1dbfd8f3d1e8d604` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `72da87f9b318654b` | `2e45ad554dc8876c` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `d94833c3a18f8546` | `0dd8982209f7390b` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `28e10d55aef58de9` | `c6f3b015807efab0` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `548606cca90e0e00` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `050f2a8526860056` | `b2a49ac46a82c64a` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `a853e49fb584b0fc` | `5dfdccf8b8dd97c5` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**x86_64 (hvx), rust-kv**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `cc837d9a09a2e708` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `bc268af1845c6e27` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `816d17daf4541f55` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `cb256779528160cd` | yes |

**Mac (hvm), rust-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `61439e850cbf3f0b` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `340ee6be11b15054` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `5c01cc7f873600a8` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `a3ccc7e44aba8a85` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `45f4beefdcc451e5` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `30a2e13e3513dcd6` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `9becaca62b9a064d` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `c79b1f2bf4cf5659` | `53e69be30dff5ad1` | yes |

## Milestone 155: the image record (superseded by "Milestone 163" below)

Since M155 every image carries `/etc/hydo/image.json` (the guest kernel it was built for, by
sha256, and the fingerprint epoch; docs/HISTORY.md "Milestone 155"), so every image has new bytes
and every fingerprint of the M140 section above is superseded; the agent, the engines and the
controller are M140's, so the verdicts are M140's (docs/REBASELINE.md section 13 lists the lines
that differ). These are fingerprint epoch 1. The current values, each schedule run twice with the
same fingerprint (docs/REBASELINE.md section 13, which also says how each image was made):

**x86_64 (hvx), rust-kv**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `2db6fdcc217982c3` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `9391d8a94083dd30` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `4ce661597e641723` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `faa3c2c7c56207c8` | yes |

**Mac (hvm), rust-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `547f7b419affd943` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `6d32a2e7ac9aecce` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `c221cba5d3298571` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `3d188ff780a5d993` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `439f45f7fe19dff7` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `dbc272cd3cc600ec` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `0178e70f40b959a5` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `6e6269b2af51beab` | `53e69be30dff5ad1` | yes |

## Milestone 163: the SDK cleanup (the current values)

Milestone 163 cleaned up the assertion SDKs' APIs before their first publication (the named
comparison helpers and `emit_line` removed, internals hidden; docs/HISTORY.md "Milestone 163").
The records the remaining calls write are unchanged, but `rkv` compiles the Rust SDK and now calls `hydo_always_cmp!(s.size, "<=", MAX_LOG, ..)` instead of the removed `hydo_always_less_than_or_equal_to!`, so the image has new bytes and new
fingerprints. Every verdict line is the earlier one word for word; each schedule ran twice with
the same fingerprint (docs/REBASELINE.md section 16, which also says how the images were made).
Fingerprint epoch 1.

**x86_64 (hvx), rust-kv** (`explore-ob-rust-kv-v2`, disk `ef1b4c5d…`)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `d184b981aeb40784` | `f8463ecaf1560f12` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `a9cdd32d8953d7f2` | `9003b2c60348a405` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `9e590e418c25edae` | `50d7001af4588bc1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `eb37594cfd033c85` | `20741d9f13ac3585` | yes |

**Mac (hvm), rust-kv** (initramfs `c450aacb…`)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `5e1d844bdf412e73` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `ac2bfa6ddc729e9f` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `3a4be66e14c4c051` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `bf97e6e506a17a14` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `839345b237fb6bef` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `1f691ae7e24d1f34` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `aa0b8c437f8b33f4` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `a55085a1fe310ad9` | `53e69be30dff5ad1` | yes |

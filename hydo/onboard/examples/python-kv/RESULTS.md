# python-kv — results (2026-10-24, Mac backend, M83)

A Python service under hydo, with its properties written by the program itself through the
Python SDK (`sdk/python`, `hydo_assert`). Run on this Mac (M2 Pro) with the Mac release
`hydo-mac-0.1.0` rebuilt on 2026-10-24 (the build that ships `buggify: true`), installed into a
scratch prefix; at most two VMs at a time.

- Programs: `pykv.py` (an asyncio TCP server with an append-only log on `/data`, and its client
  and oracle) and the SDK copied to `/opt/pykv/hydo_assert`.
- Python: the pinned aarch64-linux CPython 3.11.9 of the build machine's nixpkgs
  (`/nix/store/ic0l2ykss28lr7r4by14inb3pqw87yza-python3-3.11.9`), staged by
  `fetch-python-arm64.sh` (interpreter, standard library without test/idlelib/tkinter/… and
  without the `-O`/`-OO` `.pyc` files, extension modules, and the shared libraries of 15 other store paths that
  the loader opens; 1317 files, 52 MB, checked against `files.sha256`).
- Image: `hydo build hydo-mac.yaml` gives `explore-ob-python-kv-arm64.cpio.gz` `d96a0feb…` and
  the data disk `93c5d2ca…`; two builds are byte-identical.

## Hand-written schedules (`hydo run`, each twice, both runs identical)

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK, acked=20 all present | `bfe5a4e04ca43a71` |
| `disk=none \| run K=pykv@8+2` | OK, acked=18 (the restarted server replays its log: `kv.recovered`, `kv.log_parses` pass) | `3c05e0fa3796cea2` |
| `disk=cut \| run C=12` | OK, acked=12 (fsync before the reply) | `4f40677ee1ce1be2` |
| `disk=cut \| run C=12 S=never` | VIOLATION `key k:0 lost (got 'NONE')` (no fsync: the power cut loses acknowledged PUTs; `kv.acked_durable` FAIL) | `74debdd69ee7af08` |
| `disk=none \| run B=5abc,1000,500` | OK, acked=4 (both fault points fire: `HYDO-BUGGIFY {"seed":"5abc","enable_pm":1000,"fire_pm":500,"enabled":["kv.put.lost_reply","kv.put.slow"]}`) | `092c90dc4c56d181` |
| `disk=none \| run K=pykv@5+2 B=77,1000,300` | OK, acked=10 | `0b1d382236c21141` |

`hydo_assert_report` (compiled from `hydo/assert/hydo_assert_report.c`) over each run's consoles gives
the same property table for both runs of every schedule; it exits 1 only for the `S=never`
cut (`kv.acked_durable` and `pykv.oracle` FAIL) and 0 for the others.

## Explorer (`hydo explore --seeds 1-20 --strategy swarm --workers 2 --buggify`, twice)

Both explorations gave the same 20 schedules (each with a `B=` seed), the same fingerprints and
verdicts, and the same report apart from timings. 10 failures, all re-run with the same
fingerprint, all in expected classes:

| class | signature | seeds |
|---|---|---|
| lying-disk | `VIOLATION [pykv.oracle] key k:# lost` | 1 4 8 14 18 19 |
| unsafe-knob (`S=never` with a cut) | same | 2 5 9 |
| io-errors | `ERROR pykv restart failed` | 12 |

Minimal reproducers: `disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20` (`1d1caf745e0ee594`) and
`disk=cut dseed=dd8d1ecadadb959b | run C=9 S=never` (`f963db2572fe465d`). The three schedules
with fsync on and an honest disk that include a power cut (seeds 10, 15, 16) passed. The
report's property table shows the program's own properties: `kv.acked_durable` 13 ok / 9 fail,
`kv.recovered` and `kv.log_parses` 12, `kv.put_no_reply` 65, `kv.back` (eventually) 19 in time,
`buggify.kv.put.lost_reply` 1 and `buggify.kv.put.slow` 2; `kv.torn_tail` was never exercised
(the server writes each log line with one `write()`, so no run cut one in half).

## Notes

- The oracle (`pykv.py check`) writes the catalog, because the server starts before the
  explorer's snapshot point and its own catalog lines are not in the per-run consoles.
- No x86_64 (bare-metal) manifest: the bare-metal machine was crowded, and the coordinator asked
  for the Mac path. A Linux manifest would list the x86_64 python3 store path under `root.nix`.

## The Python history recorder (Milestone 96, 2026-10-30)

`hydo-mac-hist.yaml`: the workload's client is `pykv_hist.py`, which records every PUT in
history `kv` with `hydo_assert.history` (invoke before the request, `ok` after `OK`, `info` when
no answer came), and the `final:` read-back GETs every key into the same history; the host checks
`kv` for linearizability (one register per key) instead of the hand-written oracle. Files staged
with `fetch-python-arm64.sh` (it now also copies `hydo_assert/history.py`); image
`explore-ob-python-kv-hist-arm64.cpio.gz` `78b57526…`, disk `03d395df…`, two builds identical.
With the Mac `hydo run` (which now runs the host checkers), each twice, identical:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=cut \| run C=12` | OK, `kv.linearizable: 32 operations on 20 keys linearizable` | `c4391968a41ff960` |
| `disk=cut \| run C=12 S=never` | VIOLATION `[kv.linearizable] key k:0: an acknowledged write is lost or a read is stale` (12 keys) | `1f36ed5eb33c8652` |

The original `hydo-mac.yaml` image, rebuilt from the M83 sources with the M96 tools, is still
`d96a0feb…` / `93c5d2ca…` and gives `bfe5a4e04ca43a71` and `74debdd69ee7af08` as above. A fetch
made after M96 copies `history.py` into `files/opt/pykv/hydo_assert/` too, so that image then
differs by that one file.

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**Mac (hvm), python-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `26d1a7ff9f69fc6e` | `b7a4875e60aad8cf` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `2bf54a9ce250ed03` | `ac6ca0286e71a319` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `9860f80fb0e1625b` | `8c230ac87c46cb19` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `d3709c242b3f967a` | `6e977dae9aa72cba` | yes |

**Mac (hvm), python-kv-hist**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `a7a64977623a9b0e` | `59f9219faa984ef8` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `88d948c63401bb25` | `f9cb1876c15427a0` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**Mac (hvm), python-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `9d00f01d83b06ac4` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `128d1d3b48e64d1b` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `849c3e7fdff7ff7d` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `8b0fb5dad694116f` | `91e0931a5e07100c` | yes |

**Mac (hvm), python-kv-hist**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `591de92b60287448` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `9f14f5e97552d859` | `4a799f3170dcb41b` | yes |

## Milestone 155: the image record (the current values)

Since M155 every image carries `/etc/hydo/image.json` (the guest kernel it was built for, by
sha256, and the fingerprint epoch; docs/HISTORY.md "Milestone 155"), so every image has new bytes
and every fingerprint of the M140 section above is superseded; the agent, the engines and the
controller are M140's, so the verdicts are M140's (docs/REBASELINE.md section 13 lists the lines
that differ). These are fingerprint epoch 1. The current values, each schedule run twice with the
same fingerprint (docs/REBASELINE.md section 13, which also says how each image was made):

**Mac (hvm), python-kv** (superseded by "Milestone 157" below)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `a7f4b2de5ba82687` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `5b58d933c00349b6` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `fc139f93a791ec46` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `ed2d03a6970eb0bd` | `ece160c3964f7f03` | yes |

**Mac (hvm), python-kv-hist** (superseded by "Milestone 157" below)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `ee666af6229fc2e5` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `458abf5f089819cb` | `4a799f3170dcb41b` | yes |

## Milestone 157: the SDK comment reworded (superseded by "Milestone 163" below)

Milestone 157 reworded a comment in `sdk/python/src/hydo_assert/_core.py`, which both images
contain (`/opt/pykv/hydo_assert/_core.py`), so they have new bytes (initramfs python-kv
`3d7c2130b76cdd8e…`, python-kv-hist `1c03cd40f532ac95…`) and new fingerprints; the verdict lines
and the ctl values are Milestone 155's. Each schedule ran twice with the same fingerprint
(docs/REBASELINE.md section 14). Fingerprint epoch 1.

**Mac (hvm), python-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `f37f7b2513c61d0c` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `0dc1f942d30494c2` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `bd9cb77e5587ec31` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `5bf4fc7e7881d4d9` | `ece160c3964f7f03` | yes |

**Mac (hvm), python-kv-hist**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `618b9d4430b2089e` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `78c99aebf1097a15` | `4a799f3170dcb41b` | yes |

## Milestone 163: the SDK cleanup (the current values)

Milestone 163 cleaned up the assertion SDKs' APIs before their first publication (the named
comparison helpers and `emit_line` removed, internals hidden; docs/HISTORY.md "Milestone 163").
The records the remaining calls write are unchanged, but both images contain the SDK (`/opt/pykv/hydo_assert/`) and `pykv.py`, which now calls `LOG_SIZE.cmp(self.entries, "<=", MAX_ENTRIES)` instead of the removed `less_than_or_equal_to`, so each image has new bytes and new
fingerprints. Every verdict line is the earlier one word for word; each schedule ran twice with
the same fingerprint (docs/REBASELINE.md section 16, which also says how the images were made).
Fingerprint epoch 1.

**Mac (hvm), python-kv** (initramfs `0796b216…`)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `77662a4dc66660b4` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `d218c9200955852c` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `67c1be61543262ec` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `9de124f239acb2d2` | `ece160c3964f7f03` | yes |

**Mac (hvm), python-kv-hist** (initramfs `08d77aa7…`)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `c784a2d6d224eb47` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `4e0ea385f449db81` | `4a799f3170dcb41b` | yes |

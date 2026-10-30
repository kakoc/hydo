# python-kv — results (2026-10-24, Mac backend, M83)

A Python service under hydo, with its properties written by the program itself through the
Python SDK (`sdk/python`, `hydo_assert`). Run on this Mac (M2 Pro) with the Mac release
`hydo-mac-0.1.0` rebuilt on 2026-10-24 (the build that ships `buggify: true`), installed into a
scratch prefix; at most two VMs at a time.

- Programs: `pykv.py` (an asyncio TCP server with an append-only log on `/data`, and its client
  and oracle) and the SDK copied to `/opt/pykv/hydo_assert`.
- Python: the pinned aarch64-linux CPython 3.11.9 of the build box's nixpkgs
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
- No x86_64 (bare-metal) manifest: the bare-metal test machine was crowded, and the coordinator asked
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

# Known bug #1: SQLite WAL-reset corruption (3.51.2 buggy, 3.51.3 fixed)

Can hydo find a real concurrency bug in unmodified software without being told the trigger?
Background and sources: `../CANDIDATES.md` section 1 (sqlite.org/wal.html section 11, the
Antithesis write-up). The bug: two connections in WAL mode; a checkpoint completes, a second
checkpoint starts, and while it starts another connection commits a transaction that resets
the WAL; the second checkpoint misses the reset and records part of the WAL as checkpointed;
a later checkpoint skips part of a committed transaction and the database goes corrupt.

## Files

| file | what |
|---|---|
| `walrace.c` | the workload + checker, written from the public description only: writers, checkpointers and readers on one WAL database, each its own connection, all concurrent; every committed transaction recorded; check = every committed row present with its value + `PRAGMA integrity_check` = ok |
| `hydo.yaml` | the manifest: no service, one workload, one invariant; `params: v` builds 3.51.2 (`3510200`, default) or 3.51.3 (`--set v=3510300`) |
| `build.sh` | builds `bin/walrace-<v>` statically from the official amalgamation zips (sha256-pinned) on the bare-metal test machine (Ubuntu gcc 13.3.0, static glibc); byte-identical across builds |
| `hunt.py` | the blind explorer for this target (below) |

Pins: `sqlite-amalgamation-3510200.zip` sha256 `6e2a845a…f23ec`, `sqlite-amalgamation-3510300.zip`
sha256 `acb1e6f5…552b4` (full values in `build.sh`).

## Why a separate driver

`explore.py`'s generators place kills, cuts and disk faults by the stack's service names and
only know the M/R knobs. This target has no service and needs no fault; its levers are the
thread interleaving (seeded tick jitter, Milestone 43) and the workload's own seed (the
manifest's `S` knob: writer/checkpointer/reader counts, transaction sizes, checkpoint modes,
autocheckpoint). `hunt.py` reuses `explore.py`'s worker pool and tick-jitter formulas and
generates three arms: `timing` (jitter only), `workload` (`S` only), `both`. Run i is a pure
function of (i, salt), and its schedule string alone reproduces it.

## Results (2026-10-18, bare-metal test machine, 1 worker; results in `/var/tmp/m48-sqlite`)

**Not found.** No run violated the invariant, on hydo or natively.

| run | what | runs | violations |
|---|---|---|---|
| hydo sanity | `disk=none dseed=0 \| run` on 3.51.2 / 3.51.3 | 2 | 0 (OK on both) |
| hydo `hunt1` | arms timing / workload / both, first workload shape (2–3 writers, 1 checkpointer, 0–1 reader, 8 steps) | 150 (13.7 min) | 0 |
| hydo `hunt4` | arms workload / both, wider shape (2–5 writers, 1–2 checkpointers, 0–2 readers, 4 steps) | 1000 (89 min) | 0 |
| native control 1 | the `hunt4` shape on the host's 4 real cores, tmpfs | 3000 seeds | 0 |
| native control 2 | + per-run subset of checkpoint modes (swarm style) | 1265 seeds | 0 |
| native control 3 | + writers that also checkpoint on a second connection | 1278 seeds | 0 |
| native **aimed** | shape pinned to the write-up/fix (PASSIVE checkpoints back to back, writers, no readers) — NOT blind | 300 + 300 on 3.51.3 | 0 + 0 |

What `hunt4` explored: 1000 distinct end states (every fingerprint different); 826,405
involuntary and 401,368 voluntary thread switches inside the workload (the guest has one vCPU,
so every interleaving point is a context switch of the process); 500 runs with tick jitter,
33,904 of their 148,020 workload ticks delayed. Median run 3.9 s guest time, ~11 runs per
minute on one worker. Determinism: a `hunt4` schedule re-run from a fresh boot gave the same
fingerprint twice (`e75e8480c7583361`).

Why it is plausibly out of reach as built (after reading the 3.51.3 diff, i.e. no longer
blind): the fix adds, in `walCheckpoint`, a check after taking read-lock slot 0 that the WAL
salt did not change — the race is a writer wrapping the WAL between the checkpointer reading
its header copy and the `nBackfill < mxFrame` test. On hydo that needs a preemption inside a
window of perhaps a few hundred instructions, while preemptions only happen at timer ticks,
one per 2^20 retired branches (the PMU clock period); tick jitter moves where a tick lands but
not how often. Even natively, with real parallelism, this workload did not hit it in ~5,800
seeds, so the workload is also not close enough to whatever the write-up's (generic, unpublished)
workload does; the public reproducer page (theconsensus.dev) could not be fetched (403 /
JavaScript wall).

Levers tried: workload seed (writers, checkpointers, readers, transaction count/size,
autocheckpoint, yields), tick jitter at 5–1000 per mille with 2,000–500,000-branch delays,
more threads, more checkpoints (up to 12 M per run), swarm-style mode subsets and mixed roles
(natively). Not tried, needs code outside this directory: a shorter tick period or extra
inserted preemption ticks in `hvx_explore` (it does not expose the PMU period); several
processes instead of threads.

Images used by the hydo runs (built by `hydo-build` from this manifest; walrace at commit
`fe97d2c`, before the mode-subset / mixed / aimed options were added): 3.51.2
`explore-ob-kb-sqlite-3510200-disk.img` sha256 `05d75b28…`, 3.51.3 `…-3510300-disk.img`
`2ff12b40…`; `hunt1` used an earlier walrace (`41c8f4b`). Rebuilding from this commit gives
new hashes (the program changed).

Commands (box, `~/hydo-sqlite/hvx/knownbugs/sqlite`):
`python3 hunt.py --image explore-ob-kb-sqlite-3510200-disk.img --fixed explore-ob-kb-sqlite-3510300-disk.img --runs 1000 --workers 1 --arms workload,both --salt 0xB4 --out /var/tmp/m48-sqlite/hunt4`;
native controls: `/var/tmp/m48-sqlite/native*.sh`, `aimed-native.sh`.

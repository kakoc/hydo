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

## Results

(filled in below by the runs)

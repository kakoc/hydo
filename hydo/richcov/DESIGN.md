# Richer exploration signal: whole-system edges + assertion/state coverage (M43 design)

Milestone 41 tried AFL-style guidance on the front door's edges alone and it did not beat random
search: the front door's edge set saturates within a few runs, most of it is fault-independent
start-up code, and the bug that mattered was about the order of events, not about reaching new
code. This design widens the signal in two directions and says how a corpus scheduler combines
them. Everything here keeps the explorer's two invariants: a run is a pure function of its
schedule string, and a failure replays bit-for-bit.

## 1. Whole-system edge coverage (built; run on the box in M44)

**Which programs.** The front door (`hydostack`), Redis and MySQL. Kafka runs JIT-compiled Java
and is left out (see "Not covered").

**Making a trace attributable.** A PT trace is only a stream of executed addresses; to say which
program an address belongs to, the three programs must occupy disjoint, known address ranges in
every run. The `richcov` image (`hvx/richcov/build-richcov.sh`) arranges that:

| program | how | executable text |
|---|---|---|
| hydostack | Go, static, linked with `-T 0x2000001000`, symbols kept | 0x2000001000 + ~7 MB |
| redis-server | rebuilt from the same 7.2.4 tarball (`build-redis-debug.sh`), static musl, `-Ttext-segment=0x40000000`, DWARF | 0x40001000 + ~1.2 MB |
| mysqld | the pinned binary, unchanged (PIE, full `.symtab`, no DWARF) | 0x555555554000 + .text offset, **only with `norandmaps` on the guest kernel command line** |

Why not CR3 filtering: the hardware matches ONE CR3, so it can isolate one process per run, not
three. Why not "trace all user space and attribute through /proc/<pid>/maps": the busy JVM and the
shared libraries would multiply the trace size several times over, and the static busybox,
`hvx_seedrng` and the original static Redis all sit at 0x400000, so the same address would mean
different code in different processes and the decoder would need per-process page tables. Fixed,
disjoint layouts avoid both problems: the decoder reads code bytes straight from the ELF files.

**The PT filter.** The hardware offers two IP ranges. Range 1 = `[0x40000000, end of hydostack
text)` covers Redis and the front door (nothing else executes in that window: busybox is at
0x400000, mysqld and every shared library are far above). Range 2 = mysqld's executable segment at
the no-ASLR base. User mode only, return compression off (as in M40/M41).

**Decoding.** `hvx/richcov/ptmulti.c` decodes one trace into per-program edge files
(`<name>.edges`, `<name>.funcs`, `summary.txt`). Same edge rule as the M41 decoder
(`hvx/explore/ptedges.h`), plus: the chain is cut when the next instruction belongs to another
program (a process switch) or lies outside every program. Verified off the box on the saved M40
traces: on the front-door-only trace it produces exactly the same 41,618 edges as `ptedges.h`,
reruns are byte-identical, decoding takes ~3 s for 127 MB, and the clean vs kill-kafka@10 diff
shows the same 98 fault-only functions as M40 (`main.retry`, `main.kafkaHasKey`,
`main.outcomeUnknown`, ...).

**Feature for the scheduler.** As in `hvx/explore/guided.py`: `(program, from, to, bucket)` with
AFL hit-count buckets (1, 2, 3, 4-7, 8-15, 16-31, 32-127, 128+). A new feature in *any* program
counts as new coverage, but the scheduler also keeps per-program novelty counts (section 3) so the
front door's fast saturation no longer dominates.

## 2. Assertion and state coverage (implemented in M44, see "Implementation" below)

The stack already emits `HYDO-ASSERT` records (23 declared properties in `hvx/stack/main.go`,
format in `hvx/ASSERTIONS.md`). Three feature kinds are derived from a run's console:

1. **Property firsts.** For every `sometimes`/`reachable` property, `(id, ok=true)` is a feature
   the first time any run in the exploration produces it. Today's never-exercised properties
   (e.g. `oracle.redis_cache_miss`, `oracle.pending_order` before M39's changes) become explicit
   goals: a run that satisfies one is always kept, regardless of edges. `always`/`unreachable`
   failures are findings, not features.
2. **Detail buckets.** Properties carry `details` (store name, order id, retry attempt). Features
   are `(id, key, bucket(value))` for low-cardinality keys: `store` as-is; numeric values in
   power-of-two buckets (`attempt` 1, 2, 3-4, 5-8). Order ids are excluded (they would make every
   run "new"). **Needed SDK change:** `hydoassert` currently emits each (property, outcome) once
   per process, so only the first occurrence's details reach the console. Add a per-property
   "distinct details" mode: emit again when the bucketed detail tuple differs from every tuple
   already emitted by that process (still bounded, still deterministic).
3. **Per-order state combinations.** The oracle knows, for each order, its state in each store at
   the end: client view `{acked, failed-definite, unknown, never-tried}` × MySQL `{absent,
   present-unpublished, present-published}` × Kafka `{0, 1, >1}` × Redis `{absent, right, wrong}`.
   Emit one record per distinct tuple per run (e.g. a new `sometimes order.state` with the tuple
   in `details`, once per distinct tuple). A feature is a tuple never seen before. This is the
   signal M41 lacked: the old durable-defaults bug is the tuple `(failed-definite, present-published,
   1, absent)` — new state, reached by an ordering of faults, with no new code at all.

All three are pure functions of the console, which is part of the run's deterministic output.

## 3. Combining the signals in the corpus scheduler

Each run yields a feature set `F = E (edges, per program) ∪ P (property firsts) ∪ D (detail
buckets) ∪ S (state tuples)`. A schedule enters the corpus iff `F` contains any feature never seen
in the exploration. Each corpus entry records which kinds of new features it contributed.

Choosing the next entry to mutate (replaces `guided.py`'s "least-mutated entry"):

- **Weights by kind.** Rare kinds are worth more: score(entry) = Σ over its contributed features
  of w(kind) / (1 + times this feature has since been hit), with w(S) = w(P) = 4, w(D) = 2,
  w(E) = 1. A feature hit by everybody (start-up edges) decays to nothing; a state tuple reached
  once keeps its entry attractive.
- **Energy.** Divide by (1 + times the entry was mutated), as now, so no entry starves the rest.
- **Exploration floor.** Every k-th schedule (k = 4) is a fresh random seed, so the corpus never
  locks the search into one region. M41 showed random schedules are strong on this system; the
  floor keeps that strength.
- **Mutation operators** stay as in `guided.py` (kill target/point/downtime, netem target/params/
  window, cut point, disk class/prob/seed, durability knob). Add **splice**: take the fault list of
  one corpus entry and the cut point of another — the old bug needed a kill AND a later cut.

Evaluation, same as M41 so results are comparable: runs-until-first durable-defaults failure on
the old front door (`explore-disk.img` semantics, rebuilt in the richcov layout), several
repetitions, random vs edges-only vs full signal; and distinct failure signatures on the fixed
front door at equal budget.

## Implementation (M44)

- **Images.** `RICHCOV_VARIANT=old|fix bash hvx/richcov/build-richcov.sh` builds explorer
  (serve-mode) images in the coverage layout: `richcov-old-disk.img` (front door
  `hvx/explore/stack-src-sdk`, which has the M38 bug) and `richcov-fix-disk.img` (`hvx/stack`).
  `build-stack.sh` treats `STACK_OUT=richcov-*` like `explore*`. The front door source is first
  run through `patch-src.py`, which adds `richstate.go` (from `state.go.in`: one
  `HYDO-STATE client=.. mysql=.. kafka=.. redis=.. n=..` line per distinct per-order tuple,
  printed by the oracle right after the stores are read) and the SDK's distinct-details mode
  (`distinct.go.in`, a one-line change in `record()`); keys store/what/attempt/count/kafka_count,
  numbers in power-of-two buckets, at most 16 tuples per outcome. Nothing else changes.
  Client state is `acked | failed | other` (the oracle cannot tell "unknown outcome" from
  "never tried"; both are `other`).
- **Tracing.** `hvx_explore` takes `HVX_EXPLORE_PT_RANGES=<lo>-<hi>,<lo>-<hi>`,
  `HVX_EXPLORE_PTMULTI="<ptmulti> name=elf[@bias] ..."` and `HVX_EXPLORE_CMDLINE_EXTRA=norandmaps`;
  without them it is unchanged (the two pinned reproducers give `2403499c9a3f8419` and
  `f87130453609a5d6` with the new binary). Range 1 = Redis text start .. end of front-door text,
  range 2 = mysqld's executable segment at 0x555555554000.
- **Scheduler.** `hvx/explore/richguided.py` (modes `rich`, `edges`, `random`), section 3 as
  written, plus: a mutation that repeats an already-run schedule is redrawn.
- **Report.** `hvx/richcov/richreport.py <dir>`.

## Results (M44, bare-metal test machine, 1 worker, shared with two other workstreams)

Raw data under `/var/tmp/m44-rich` on the box; scripts in `hvx/richcov/eval/`; tables from
`richreport.py`.

- **Layout confirmed.** With `norandmaps`, mysqld runs at 0x555555554000 (36,780 edges, 0 decode
  errors, 0 instructions outside the three programs on a clean run); Redis at 0x40001000.
- **Cost.** Trace median 40 MB (max 268 MB), decode median 10 s (max 58 s), run median 25 s.
  **129 of 439 traces filled the 256 MiB buffer (truncated)**, mostly runs with a service kill
  and no power cut, and runs with I/O errors (MySQL recovery/restart is long). A 512 MiB buffer
  failed to allocate (ENOMEM) on the busy box.
- **Determinism.** Tracing on/off: same fingerprint. Two traced runs of the same schedule: edge
  files identical on the smoke test; over 29 untruncated schedule pairs, 23 byte-identical edge
  files and **28/29 identical bucketed features** (the exceptions are +-1 hit counts on a few
  mysqld edges, and one Redis run with 3 decode errors). All 40 fingerprints identical.
- **Old front door, runs until the durable-defaults bug (cap 60):** random >60 / 26 / 2;
  `rich` (as designed) 48 / 8 / 12 -- all three found by the random floor, not by a mutation;
  `richk` (per-kind normalized score, truncated edges ignored) 9 / 43 / 12 -- two found by
  mutations of corpus entries, one by the floor (the same floor schedule as rich-3).
- **Fixed front door, 40 runs:** `rich` and `edges` chose the same schedules (edge features,
  thousands per run, swamp the few state/property features in the as-designed sum) and ended
  with 8 state tuples, 16 property firsts; random reached 12 state tuples and 16 property
  firsts, slightly more edges in hydostack, fewer in Redis/MySQL. No durable-defaults failure in
  any arm. Guided arms spent their budget on I/O-error schedules (12 of 16 failures).
- **Verdict.** No demonstrated advantage over random at this sample size: random's spread (2 to
  >60) is larger than any difference. The as-designed scoring degenerates to edge guidance
  because MySQL's edge set never saturates (new edges in ~70% of runs); normalizing per kind
  (`richk`) is what made mutations, not the floor, find the bug -- a hint, not a result.

## Not covered (and what it would take)

- **Kafka**: its code is JIT-compiled Java. Needs the JVM's perf map
  (`-XX:+UnlockDiagnosticVMOptions -XX:+DumpPerfMapAtExit`, read from the guest after the run) and a
  third PT range, which the hardware does not have — so Kafka coverage would need its own runs
  (range 2 on the JIT code cache instead of mysqld).
- **MySQL source lines**: the pinned binary has function symbols only. Line-level would need a
  rebuild with `separateDebugInfo` (a long build) and that rebuilt binary in the image.
- **Guest kernel**: only `System.map` (function level) is available; the NixOS kernel has no
  `-dev`/DWARF output on the build box. Kernel coverage also needs OS-mode tracing, i.e. separate
  runs (the two ranges are taken by user programs).
- **Threads of one program**: a user-only trace cannot see thread switches, so one deterministic
  but artificial edge per switch is recorded (same limitation as `ptedges.h`).

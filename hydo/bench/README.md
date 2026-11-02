# Bug benchmark for the stack explorer (Milestone 46b)

> Note (M107): `explore.py` and the other Python files named below were the explorer and tools of
> the time. The explorer is now the Rust crate `hydo/crates/hydo-explore` (`hydo explore`, or the
> `hydo-explore` program, with the same options); the Python files were deleted in M107 and can
> be restored from commit 2a58c4b (`git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history` in a scratch checkout).
> The commands below are records of how things were run and measured.
>
> Note (M108): the benchmark runner is now the Rust program `hydo-bench` (`hydo/crates/hydo-bench`):
> `python3 run.py ...` is `hydo-bench run ...` (bench_more_faults.py's cells are `hydo-bench run --more-faults
> --cf-range LO-HI ...`), `confirm.py` is `hydo-bench confirm`, `listrun.py` is `hydo-bench list`, `depth1map.py`,
> `d7prob.py` and `addbuggify.py` are `hydo-bench depth1map|d7prob|addbuggify`, and `--table DIR...` is
> `hydo-bench table DIR...`; the same options, outputs and printed lines. Not ported: `--strategy portfolio` and
> `--rescore` (one-time M56 re-scoring). The Python files, `mkpatches.py` and `smp_ident.py` were deleted in M108;
> restore one with `git show 705cf98:hydo/bench/<file>` (docs/HISTORY.md M108).
>
> Note (M135): the benchmark's images are now built from one manifest, `manifest/hydo.yaml`, by
> `build-bench-manifest.sh` on the bare-metal machine (section "The manifest images
> (Milestone 135)" and "The manifest images after the switch (Milestone 136)" at the end, with the
> triggers recorded on them). `hydo-bench` runs their agent images by default (`--kind agent`;
> M135's shell images went with the shell init in M136, `--kind shell` is refused), and the hand-written
> stack images that the older sections below describe with `--kind stack`. The stack images
> (`build-bench*.sh` with `hv/hvx/images/build-stack.sh` and `hydo/explore/stack-explore.sh`)
> stay only as the engine's regression baselines (`deploy-metal.sh`, the pinned reproducers on
> `explore-disk.img` and `explore-fix-disk.img`), as the record of the measurements below, and as
> the source of the manifest build's root files (the `-pin-kr` control's root); do not delete
> them. They keep franz-go v1.17.1; the manifest images' order services are built from source
> with v1.22.2 (`build-order-services.sh`).

Every explorer strategy comparison up to Milestone 45 rested on ONE bug: the pre-M39 front door
(publish to Kafka, then fail the order because the Redis cache was down). This benchmark gives
the comparisons several targets of different kinds and difficulty. Each defect is a small,
realistic patch to a copy of the fixed front door (`hydo/stack/main.go`); everything else in its
image is byte-for-byte the control image `explore-nf-disk.img` (same guest, same M46 fault
tokens). The control rebuilt from this tree is byte-identical to the shipped
`explore-nf-disk.img` (`e5d8d6fb…`, front door `1f18f855…`).

## Detection

A run DETECTS its image's defect when (1) its schedule has durable settings and an honest disk
(explore.py class `durable-defaults`: disk `none` or `cut`, and no relaxed `M=`/`R=` knob
together with a cut — since M56 a relaxed knob without a power cut or lying disk no longer makes
a run `unsafe-knob`, because those settings only trade durability across a power loss;
`--knob-rule any` restores the M54 rule, under which the M54 Results below were scored; the
re-scored tables are in the M56 section), (2) its verdict
is a VIOLATION, and (3) the SAME schedule passes on the control image (`explore-nf-disk.img`;
for d0, whose image has the pre-M46 guest, `explore-fix-disk.img`). Condition (3) is needed
because the fixed front door has a bug of its own (below). `run.py` also checks the verdict
against the defect's own signature (column "oracle line") and re-runs the detecting schedule
once to compare fingerprints.

## The defects

Each trigger was run twice on its defect image (identical VIOLATION fingerprints both times)
and once on the control (`explore-nf-disk.img`), where it passes. All triggers are durable
settings, honest disk. "Kind" is the class of mistake; "needs" is what a schedule must contain.

| defect | kind | what the patch does (why it is realistic) | needs | trigger (guest part) | fingerprint | oracle line |
|---|---|---|---|---|---|---|
| d0-old-front-door | wrong error classification | the pre-M39 front door (`explore-disk.img`, its own older guest): fails an order already in Kafka because the Redis cache write failed | Redis down + a power cut before the retry | `K=redis@11+1 C=24` (dseed `4d5cb825dfaab05b`) | `2403499c9a3f8419` | `is in kafka ... never ACKed` |
| d1-kafka-first | wrong step order | publishes to Kafka BEFORE recording the order in MySQL, and calls a MySQL failure "not applied" (natural if you think of Kafka as the source of truth) | MySQL down during an order | `K=mysql@10+1` | `57b21c9a63e66f4a` | `present 2 times` |
| d2-batched-publish-flag | missing durability of the app's own records | writes the `published` flag behind, five orders per UPDATE, and ACKs before the flag is stored ("it is only bookkeeping") | a power cut, or any fault that shifts the batch boundary so the last batch never fills | `C=12` | `0982ff7f24617983` | `acked but not marked published` |
| d3-blind-retry | non-idempotent retry | a retry of an order whose earlier produce had an unknown outcome produces again without looking in Kafka first | a produce that errors but lands later (Kafka down or paused, or lost replies), then a retry | `P=kafka@5+2` or `O=kafka@5-10:resp:loss,100%` | `1e7ee78e0da23d6c` / `4f2bb4f141b1b67b` | `present 2 times` |
| d4-produce-timeout-definite | wrong error classification under a deadline | gives the produce its own 5 s budget (a timer around `ProduceSync`) and reports a timed-out produce as "not applied" | a slow or stuck Kafka (the produce lands later) AND a power cut before the client's retry: two faults | `P=kafka@5+2 C=8` or `O=kafka@5-10:resp:loss,100% C=12` | `8f9916c7a09e19e3` / `f2180513dc7ce87f` | `told it failed` |
| d5-myisam | missing durability | creates the orders table as MyISAM ("simpler, no transactions needed"): not crash safe, never fsyncs its data | a power cut | `C=15` | `c7e097222b66cc96` | `mysql order=... MISSING` |
| d6-client-deadline-definite | client-side deadline bug | the client counts its own timeout as a definite failure ("it timed out, so it did not happen") and the retry pass skips definite failures | a request that outlives the client's 20 s deadline but is applied (Kafka stuck or restarting) | `P=kafka@5+2` | `d69d7035542ca1c4` | `told it failed` |
| d7-skip-last-record | off-by-one, narrow | the Kafka lookup stops one record early (`last` read as the last record's offset; it is the next offset) | as d3, but the uncertain order must be the topic's LAST record when it is retried: a fault covering the end of the workload | `P=kafka@28+2` | `e3607a4b79305277` | `present 2 times` |

Design notes. d2, d4 and d7 replaced first attempts that the triggers could not confirm:
disabling the producer's idempotence (franz-go only retries a request inside the delivery
timeout after a connection error, which no fault produces mid-request), calling a produce error
"not applied" via the request context (franz-go's `ProduceSync` does not return early on context
cancellation for a record already sent, so the client's own deadline always fired first), and
skipping the topic's first record (the first produce also initialises the producer id, so order
0 cannot be "uncertain but landed").

## A bug in the fixed front door (found while confirming d7)

The control image itself fails with durable settings and an honest disk:

    disk=none dseed=0 | run O=kafka@25-30:resp:loss,100%
    -> HYDO-ORACLE: VIOLATION kafka order=26 present 2 times (must be exactly once)   fp f1c39a886803a8fa (2 of 2 runs)

Every reply from Kafka is dropped for orders 25-29, and the fault is only healed after the last
order, right before the client's retry pass. Order 25's retry finds its record (dedup hit), but
order 26's first produce is still being delivered by the franz-go producer when the retry's
lookup runs: the lookup misses it, the retry produces again, and both land. The front door's
"check Kafka before re-producing" assumes the earlier attempt is finished once its call
returned an error; with an in-flight produce it is not. Milestone 46's one-way sweep healed at
order 15, so the earlier produce had always finished before any retry. Related: `ProduceSync`
ignores context cancellation for a record already sent, so the front door's "every store call is
bounded by the request deadline" does not hold for Kafka. Because of this bug, `run.py` re-runs
every candidate detection on the control image and counts it only if the control passes
(`base_bug_hits` in the results lists the ones it rejected).

## A second bug in the fixed front door: franz-go's idempotent producer (Milestone 133)

On agent images of the benchmark manifest (`manifest/hydo.yaml`), the control loses an
acknowledged order after a long Kafka pause:

    disk=none dseed=0 | run P=kafka@14+16
    -> HYDO-ORACLE: VIOLATION [stack.exactly_once] kafka order=15 expected=100015 got=MISSING (mysql has it: true)   fp c4a120cca428aa48

The cause is franz-go v1.17.1 (the version in `hydo/stack/go.mod`), issue 1484, fixed in
v1.22.2: after Kafka is continued, the producer fails order 14's batch, which the broker had
already appended, and rewinds the sequence number under the same producer id and epoch; order
15's retry is then produced with order 14's sequence number, the broker takes it for a re-send,
answers success and stores nothing. The front door rebuilt with franz-go v1.22.2 passes the
same schedule. The shell images' timing has not reached the window (tick jitter included).
`kafka-diag.sh` makes a diagnostic copy of a manifest image that prints Kafka's batches with
their sequence numbers. Details: `docs/C2-BENCH.md` section 6 item 4.


## Files

- `patches/<defect>.patch` — the defect, a unified diff against `hydo/stack/main.go`
  (generated once by `mkpatches.py`, removed in M108: it read today's `hydo/stack/main.go`, which
  no longer has the code the patches change; the patches apply to `base/main-0112b48.go`).
- `build-bench.sh` — copies `hydo/stack`, applies a patch, builds
  `build/explore-bench-<defect>-disk.img` with `build-stack.sh` (`STACK_OUT=explore-bench-*`
  gets the explorer's serve mode; `STACK_GO_SRC` points at the patched copy). On the build machine
  box the pinned static musl gcc and Redis are gone (M46), so pass `HYDO_GCC_MUSL`,
  `HYDO_MUSL_ROOT`, `HYDO_REDIS_OUT` as M46 did. A build takes ~15 s.
- `hydo-bench confirm` (`hydo/crates/hydo-bench`; confirm.py until M108) — runs every trigger twice
  on its defect image (both must be the same VIOLATION fingerprint) and once on the control (must be OK).
- `hydo-bench run` (run.py until M108) — runs a strategy on every defect image with an early stop at
  the first detection; `hydo-bench table <dirs>` prints the comparison table.

## Commands (on the bare-metal machine, from a tree with `hv/hvx/hvx_explore` built)

    cargo build --release -p hydo-bench            # or copy a static x86_64 musl build to the bare-metal machine
    B=target/release/hydo-bench
    $B confirm --out /var/tmp/m46-bench/confirm
    $B run --strategy random  [--new-faults] --defects all --reps 1-3 --budget 60 --workers 3 --out /var/tmp/m46-bench/random
    $B run --strategy swarm   [--new-faults] ...
    $B table /var/tmp/m46-bench/{random,swarm,random-nf,swarm-nf}

(The M46 `portfolio` column came from the archived `portfolio.py`, removed in M108.)

## Results (2026-10-18/19, module srcversion `53856EFDC33F55145778DA8`, 3 workers)

3 repetitions × up to 60 runs per defect and strategy; repetition r uses seeds r*1000+1..
(the M42/M44 repetitions). Cell: repetitions that detected the defect, (run of first detection
per repetition), median. random/swarm stop at the first detection; portfolio always runs 60.
`+nf` = `--new-faults` (M46 pause / clock jump / one-way loss). Results in
`/var/tmp/m46-bench/<strategy>/results.jsonl` on the bare-metal machine; table `/var/tmp/m46-bench/table.md`.

| defect | random | random+nf | swarm | swarm+nf | portfolio |
|---|---|---|---|---|---|
| d0-old-front-door | 2/3 (-, 6, 8) med 7 | 2/3 (-, 6, 8) med 7 | 3/3 (17, 19, 38) med 19 | 3/3 (17, 19, 38) med 19 | 3/3 (39, 24, 18) med 24 |
| d1-kafka-first | 3/3 (2, 7, 6) med 6 | 3/3 (2, 7, 6) med 6 | 3/3 (9, 8, 1) med 8 | 3/3 (2, 8, 1) med 2 | 3/3 (4, 15, 2) med 4 |
| d2-batched-publish-flag | 3/3 (2, 6, 5) med 5 | 3/3 (5, 6, 5) med 5 | 3/3 (2, 4, 4) med 4 | 3/3 (5, 4, 4) med 4 | 3/3 (12, 14, 3) med 12 |
| d3-blind-retry | 3/3 (2, 58, 14) med 14 | 3/3 (2, 58, 14) med 14 | 3/3 (23, 54, 1) med 23 | 3/3 (5, 54, 1) med 5 | 2/3 (9, -, 2) med 5.5 |
| d4-produce-timeout-definite | 2/3 (-, 6, 32) med 19 | 2/3 (-, 6, 32) med 19 | 3/3 (39, 19, 38) med 38 | 3/3 (39, 19, 38) med 38 | 3/3 (18, 27, 21) med 21 |
| d5-myisam | 3/3 (3, 4, 5) med 4 | 3/3 (3, 4, 5) med 4 | 3/3 (2, 4, 4) med 4 | 3/3 (2, 4, 4) med 4 | 3/3 (12, 15, 3) med 12 |
| d6-client-deadline-definite | 3/3 (2, 6, 14) med 6 | 3/3 (2, 6, 14) med 6 | 3/3 (23, 19, 1) med 19 | 3/3 (5, 19, 1) med 5 | 3/3 (9, 28, 2) med 9 |
| d7-skip-last-record | 0/3 | 0/3 | 0/3 | 0/3 | 0/3 |
| detections (of 24) | 19 | 19 | 21 | 21 | 20 |

Every detection reproduced its fingerprint and matched its defect's oracle line; no candidate
was rejected by the control check (the fixed front door's own bug was never hit). Portfolio
detections came from the swarm (6), timing (5), sweep (5), random (3) and branch (1) arms.

Verdict.
- All five strategies find the same 7 of 8 defects; none finds d7 (the uncertain order must be
  the topic's last record at retry time -- a fault covering the end of the workload). So on
  this benchmark the strategies differ in speed and consistency, not in reach.
- random is fastest on the easy defects (median 4-7 runs) but misses d0 and d4 once each;
  swarm finds every reachable defect in every repetition but is slower (median 19-38 on d0,
  d4, d6). That is the M42 trade-off, now seen across seven defects instead of one.
- `--new-faults` adds nothing for random (the same first detections except d2 rep 1, 2 -> 5,
  where a new fault on run 2 hid the defect) and speeds swarm up on three defects
  (d1 8->2, d3 23->5, d6 19->5) by giving Kafka-stalling schedules more often. A Kafka KILL
  already makes "a produce that fails and lands later", which is why d3, d4 and d6 are found
  without the M46 faults at all.
- portfolio is the only strategy with the best d4 (3/3, median 21) and is never the worst by
  much, but it pays for exploring weak arms on the easy defects (median 12 on d2/d5 against 4-5).
- The narrow defect d7 is the case for systematic search: the M46 `oneway` sweep heals at order
  15, and no generator places a Kafka stall over the last order often enough. A sweep that ends
  faults at the workload's end would find it (its trigger `P=kafka@28+2` is such a point).

## d7 and exploration time (Milestone 55)

**Tail sweep** (`explore.py --sweep tail`: kill, both pause forms, one-way and two-way network
faults on every service, starting at one of the last 3 orders, plus cuts at 28-30; 165 points,
about 9 minutes per image on 3 workers). On the d7 image, 7 points fail with
`kafka order=29 present 2 times` while the same point passes on the control
(`explore-nf-disk.img`): `K=kafka@27+3`, `K=kafka@28+2`, `P=kafka@27+3`, `P=kafka@28+2`
(the README trigger, `e3607a4b79305277`), `P=kafka@29+1`, `O=kafka@29-30:resp:loss,100%`,
`N=mysql@27-30:loss,60%`. One-way 100% loss on Kafka from order 27 or 28 fails on the control
too (the M50 base bug), so it does not count. `K=kafka@29+1` fails on both images with
`kafka topic unreadable`: the oracle reads Kafka before it is back up (a harness artefact).
All other points pass on both. Every failure reproduced.

**Trigger set** (sweep plus 59 validation schedules, `tail-validate.txt` via `listrun.py`):
Kafka unavailable from some order until the END of the workload, so the order with an
uncertain send is the last record in the topic when the client retries.
- A pause reaching the end triggers from any start (20, 26, 27, 28, 29 measured). The
  generators draw `P=kafka@27+3`, `@28+2`, `@28+3`.
- A kill reaching the end triggers only from order 27 or 28; `K=kafka@26+4`, the only one the
  generators can draw, also fails on the control (base bug), so generated kills never count.
- Timed pauses (`P=kafka@at~off:dur`, at 25-28, dur 10-30 s): 0 of 36 trigger.
- Reply loss on Kafka over the last order only: 90% and 100% trigger, 30% and 60% do not.
  The generators never place one there (`a` is at most order 28).
- `N=mysql@27-30:loss,60%` exactly; loss 50/55% or a start at 25, 26 or 28 does not.

**Per-run probability** (`d7prob.py`, 10^6 seeds per strategy; pure function of the
generators): random 1 in 10^6, swarm 0 — without `--new-faults` d7 is practically
unreachable. With `--new-faults`: random 1 in 1,567, swarm 1 in 1,876 schedules in the set.
Measured precision of the set: 28 of 40 predicted schedules detect d7 (70%); most misses also
kill a service earlier in the run. The benchmark also counts a detection only in class
durable-defaults, which excludes any schedule with a relaxed knob (M/R) even when there is no
cut for it to matter: then 1 in 1,942 (random+nf) and 1 in 4,149 (swarm+nf) before precision,
about 1 in 2,800 and 1 in 5,900 after.

**Long budget:** random+nf, rep 1, 1,500 runs (2 h 19 min on 3 workers, ~650 runs/hour): run
1,420 (seed 2420, `K=kafka@24+3 R=everysec P=kafka@28+3 O=redis@18-27:resp:delay,284ms`) hit
d7 exactly where predicted (`present 2 times`, control OK), but was not counted because of the
irrelevant `R=everysec` (class unsafe-knob). swarm+nf, rep 3, 300 runs (34 min): the predicted
hit at run 207 is one of the 30% that do not trigger (an earlier `K=mysql@4+4`); no detection.

**Verdict:** d7 is a placement blind spot, not just short exploration: plain random/swarm never
reach it, and with the new faults the expected wait is ~2,800 runs (~4.3 h of box time) for
random and ~5,900 (~9 h) for swarm. The tail sweep finds it in 9 minutes.

## BUGGIFY fault points (Milestone 56)

Code-level fault points in the front door, FoundationDB style (recommendation 3 of
`hydo/explore/RESEARCH-search.md`). `hydoassert.Buggify(id, msg)` declares a point; for a run each
point is enabled with probability 0.25 and an enabled point fires on each evaluation with
probability 0.25 (FoundationDB's defaults, `flow/include/flow/Buggify.h`:
`P_..._BUGGIFIED_SECTION_ACTIVATED{ 0.25 }`, `P_..._BUGGIFIED_SECTION_FIRES{ 0.25 }`), both a
pure function of the run's seed, the point and the hit index. The seed comes from the guest
token `B=<seed-hex>[,<enable-pm>[,<fire-pm>]]` (written to `/tmp/hydo-buggify`, read at the first
`Place`); `explore.py --buggify` gives every schedule one from a separate random stream, so all
other fields are unchanged. Without `B=` every point is off. A `HYDO-BUGGIFY` console line lists
the enabled points; each point is a `sometimes` property `buggify.<id>` recorded when it fires.

The 14 points (`hydo/stack/buggify.go`) wrap every store call of `Place` and take the code's own
error path for that call: MySQL slow (2 s), each statement (insert / mark-sending / publish flag)
failing before it is sent or committing with its reply lost; Kafka slow (6 s), failing before the
record is sent, or written with the produce reporting a timeout; the Kafka lookup failing; the
Redis write failing; the ACK late (3 s) or lost. `addbuggify.py` routes a front door's calls
through the wrappers; `build-bench-bg.sh` builds `explore-bench-bg-<name>-disk.img` for d0..d7
and `control` (base front doors in `base/`: d1..d7 and the control patch `main-0112b48.go`, d0 is
`main-7e32c4a.go`; all in the current guest, so one control serves all eight).

**Without `B=`** (`confirm.py --bg-images`): all 10 triggers still fail twice identically with the
same verdicts as before, and all pass on the control (fingerprints moved: new binaries and
catalog lines): d0 `80fb3138c43b1350`, d1 `5548f222e4ee1381`, d2 `e45437152fdaa5c0`,
d3 `886965f79516e715` / `df28d56d30c88d37`, d4 `21c6259650708ced` / `e0e6a1fab1e57ed9`,
d5 `7965496592b4bd1c`, d6 `928f9c0f03289352`, d7 `52d633f175ecc3a4`. Same seed twice gives a
byte-identical console.

**Evaluation** (`run.py --buggify --bg-images`, 3 reps × ≤60 runs, 1 worker, module srcversion
`7CA13762C89AF3BF16F2CCF`; every detection reproduced its fingerprint and passed the control check;
results `/var/tmp/m56-bg/{random-bg,swarm-bg}` on the bare-metal machine; plain columns are Milestone 54's):

| defect | random | random+bg | swarm | swarm+bg |
|---|---|---|---|---|
| d0 | 2/3 (-, 6, 8) | 3/3 (3, 6, 3) | 3/3 (17, 19, 38) | 3/3 (2, 8, 12) |
| d1 | 3/3 (2, 7, 6) | 3/3 (1, 4, 2) | 3/3 (9, 8, 1) | 3/3 (6, 4, 1) |
| d2 | 3/3 (2, 6, 5) | 3/3 (2, 4, 3) | 3/3 (2, 4, 4) | 3/3 (6, 19, 4) |
| d3 | 3/3 (2, 58, 14) | 3/3 (2, 7, 2) | 3/3 (23, 54, 1) | 3/3 (6, 45, 1) |
| d4 | 2/3 (-, 6, 32) | 3/3 (8, 6, 26) | 3/3 (39, 19, 38) | 3/3 (2, 19, 12) |
| d5 | 3/3 (3, 4, 5) | 3/3 (3, 4, 5) | 3/3 (2, 4, 4) | 3/3 (2, 4, 4) |
| d6 | 3/3 (2, 6, 14) | 3/3 (2, 6, 14) | 3/3 (23, 19, 1) | 3/3 (23, 19, 1) |
| d7 | 0/3 | **3/3 (4, 7, 2)** | 0/3 | **3/3 (9, 45, 6)** |
| detections of 24 | 19 | **24** | 21 | **24** |

The research prediction holds under random (d7 3/3 within 30 runs; d3/d4/d6 medians 2/8/6) and
nearly under swarm (d7 rep 2 at run 45). d7 is now found without a fault at the end of the
workload: a lost produce reply or failed lookup makes an order's outcome uncertain at any order,
and the defect's lookup misses its record. d5 and d6 are unchanged (their external faults find
them first). One d2 detection (random+bg rep 3) is a Kafka duplicate rather than d2's signature;
it passed the control, so it is still the patch's. With `B=` d0 is also caught through its old
oracle, which calls any un-ACKed order found in Kafka a violation (a lost ACK is enough).

**Fixed front door (current `hydo/stack/main.go`, with the M50 fixes) under BUGGIFY**
(`explore-bench-bg-fixed-disk.img`, swarm, `--buggify --durable-only`, seeds 1–100): 100 runs,
**0 failures**; all 14 points fired (in 7–27 runs each). No new bug.

## Milestone 56: positioned faults, a fault process, the depth-1 sweep, and the knob rule

Box results in `/var/tmp/m56-pos/` (driver `drive.sh`, 2 workers); trigger maps and tables
copied to `hydo/bench/m56/`. Module srcversion `7CA13762C89AF3BF16F2CCF` (the M49 build).

**Knob rule (now the default).** A relaxed `M=`/`R=` knob makes a run `unsafe-knob` only with a
power cut or lying disk (`explore.py --knob-rule any` restores the M54 rule). Re-scoring all 120
M54 cells (`run.py --rescore`; candidates new to the durable class checked on the control
image; none rejected): every strategy still finds the same 7 defects, no missed cell turns into
a detection, and 10 cells get faster (e.g. swarm d3 23→18 and 54→13, random+nf d3 58→18, swarm
d6 23→18 and 19→13, random d1 7→3). M54 tables: `m56/tables.md` (both rules).

**New generators** (`explore.py --strategy swarm-pos | process`; 3 reps × ≤60 runs, early
stop; every detection reproduced its fingerprint):

| defect | process | swarm-pos | best M54 strategy (new rule) |
|---|---|---|---|
| d0 | 3/3 (29, 18, 1) | 3/3 (49, 51, 10) | swarm 3/3 (17, 19, 38) |
| d1 | 3/3 (2, 2, 4) | 3/3 (3, 16, 4) | swarm+nf 3/3 (2, 8, 1) |
| d2 | 3/3 (6, 1, 1) | 3/3 (3, 1, 17) | swarm 3/3 (2, 4, 4) |
| d3 | 3/3 (4, 4, 4) | 2/3 (-, 19, 19) | swarm+nf 3/3 (5, 13, 1) |
| d4 | 3/3 (13, 1, 1) | 3/3 (49, 51, 38) | portfolio 3/3 (18, 27, 21) |
| d5 | 3/3 (15, 1, 1) | 3/3 (3, 1, 10) | swarm 3/3 (2, 4, 4) |
| d6 | 3/3 (4, 1, 1) | 3/3 (49, 19, 19) | swarm+nf 3/3 (5, 13, 1) |
| d7 | **3/3 (28, 27, 4)** | 1/3 (-, 30, -) | none 0/3 |
| detections / 24 | **24** | 21 | 21 |
| box time, 24 cells | 1.0 h | 2.4 h | swarm ≈ 2.3 h (3 workers) |

`process` (a Jepsen-style staggered fault process, 2–4 different faults in consecutive windows)
finds every defect in every repetition, with medians of 1–4 runs on six of eight. One of its 24
detections (d4 rep 1) has a verdict outside d4's signature (`order=0 ... MISSING (mysql has
it)`), so 23/24 match. Per-run d7 trigger probability from the generators (`d7prob.py`
predicate, 2·10^5 seeds): process 1 in 25, swarm-pos 1 in 62 (vs random+nf 1 in 1,567).
Research predictions (`hydo/explore/RESEARCH-search.md` #2): "d7 3/3 within 20 runs" — NOT met
by either (process 3/3 but 2 of 3 after run 20; swarm-pos 1/3); "d0–d6 no more than 2× slower
than swarm" — met by process (faster on all but d0's first rep), NOT met by swarm-pos (d0, d4,
d6 2–3× slower). Swarm-pos spreads its runs over many categories and needs an honest disk for
most defects (it switches the disk category on half the time), which explains its slowness.

**Depth-1 sweep** (`--sweep depth1`, 1,274 points: K/N/P "+"/O with one order or to the end,
timed P 500 ms/30 s, C, T ±; ~2 h per image at ~690 runs/hour on 2 workers; `depth1-old` = the
384 K/N/C points for d0's pre-M46 guest). Trigger = VIOLATION on the defect image and OK on
the control at the same point (`depth1map.py`):

| defect | triggers | by kind | matching the defect's signature |
|---|---|---|---|
| d7 | 29 | P 20, K 5, O 3, N 1 | 29 |
| d3 | 196 | P 77, O 59, K 33, N 27 | 196 |
| d6 | 186 | P 79, O 59, K 32, N 16 | 186 |
| d4 | 14 | N 6, K 4, P 3, O 1 | **0** — d4 needs two faults (slow Kafka + a cut) |
| d0 | 4 (of 384) | K 4 | **0** — d0 needs two faults (Redis kill + a cut) |

d1, d2, d5 were not swept (box budget; every strategy finds them within a few runs). The d4
and d0 "triggers" are all `kafka order=N present 2 times`: the control's own duplicate bug
(below) landing on different points because the patched front door shifts timing — attribution
by "passes on the control" is not exact for a timing-dependent base bug; the signature check
catches it. d7's real trigger set is far larger than the M55 sample: Kafka paused until the end
from ANY start order 11–29, killed until the end from 11/19/21/27/28, reply loss on the last
order, one MySQL loss point.

**The control's own bug, mapped.** On `explore-nf-disk.img` (the M46 front door, before the M50
fixes), 89 of the 1,274 single faults fail: 87 are the M50 duplicate (`kafka order=N present 2
times`; 55 Kafka one-way, 24 Kafka kills, 8 Kafka pauses — any long Kafka outage), 1 is the
known harness artefact (`kafka topic unreadable` when Kafka is killed at the last order), 1 a
MySQL loss. 2 of the 205 failing points on the control + d7 sweeps re-ran with the same verdict
but a different fingerprint (`O=kafka@12-30:resp:loss,100%` on the control, `P=kafka@4+26` on
d7; both long Kafka outages) — not investigated.

**Verdict.** Fault position and combination matter more than the search strategy: a simple
staggered fault process beats every earlier strategy on all eight defects, including d7 and the
two-fault defects d0 and d4, at under half the machine time; per-category positions alone
(swarm-pos) do not. The depth-1 sweep is the ground truth for single-fault defects but cannot
reach two-fault ones (d0, d4).

## Harder defects d8–d12 (Milestone 59)

d0–d7 are all single-client defects that every strategy since M56 finds. d8–d12 add bug classes
the set lacked: concurrent requests (d8, d9, d12), a crash of the order service (d10) and a
disk that stops completing writes (d11). They share a NEW control, so they are kept apart from
d0–d7.

**Control** (`base/main-m59-control.go`): the current order service (`base/main-3773607.go`, a
copy of `hydo/stack/main.go` at commit 3773607: the M50 fixes and the M56 BUGGIFY wrappers) plus a
fix for the Milestone 58 flush-cut bug: when a request READS `published = 1` (it did not write
it), it commits a real write (`INSERT INTO stack.acks ... ON DUPLICATE KEY UPDATE n = n + 1`)
before the ACK. That commit's redo fsync covers the earlier commit, so the ACK never rests on a
visible-but-not-durable flag. A build of the unpatched `main-3773607.go` with the same tools was
byte-identical to `explore-bench-bg-fixed-disk.img` (`7974498b…`). Defect patches:
`patches-m59/<name>.patch` against the control; `build-bench-m59.sh` builds
`explore-bench-m59-<name>-pin-disk.img` (`BENCH_SUFFIX=-pin`). The images were built from the
pinned toolchain (`docs/BUILD-PINS.md`; `HYDO_GCC_MUSL`, `HYDO_MUSL_ROOT`, `HYDO_REDIS_OUT` and
`HYDO_SEEDRNG` unset). I extracted `/bin/hvx_seedrng` from each image's root partition: in
all of them it is `c2a440c5…`. Image sha256: control `bd9aa2a1…`, d8 `9a539633…`,
d9 `86b61cd0…`, d10 `878f6bae…`, d11 `0f82ca02…`, d12 `e55ea1b6…`.

**Crash oracle.** d9 makes the order service panic. Since M50 the order state lives in MySQL,
so a restarted service loses nothing and the stack's oracle passes. run.py and confirm.py
therefore count a run on an m59 image as a VIOLATION when the guest prints
`hydo-explore: front door log: panic|fatal error`. This is the property "no
process crashes", applied to the order service only. A SIGKILL leaves no such line. The rule
applies only to the m59 defects and their control, so earlier results keep their meaning.

| defect | kind | what the patch does (why it is realistic) | needs | trigger (guest part) | fingerprint | oracle line |
|---|---|---|---|---|---|---|
| d8-no-order-lock | concurrency, M50 bug 1 | removes the per-order lock ("the `published` column is the state machine"); two requests for one order both read `published=0`, the second's `UPDATE ... WHERE published = 0` changes no row but is not an error, and both produce | two requests for one order at once | `Q=2d` | `912820268124e937` | `present 2 times` |
| d9-redis-unlocked | concurrency, M50 bug 2 | removes the lock around the shared Redis connection ("a net.Conn is safe for concurrent use") | concurrent requests | `Q=4` | `ad597234f2d4897f` | `front door crashed: panic: runtime error: slice bounds out of range [15:10]` (the M50 message) |
| d10-outcome-in-memory | lost state on restart, M50 bug 3 | keeps "a send was started" in an in-memory map instead of `published = 2` in MySQL | the order service killed while a produce is in flight, then a retry | `X=front@12~20` | `6641177e3fa41dfc` | `present 2 times` |
| d11-ack-unflushed-flag | durability, the M58 bug | drops the control's extra write: ACKs on a `published = 1` it only read | the data disk stops completing FLUSHes (power cut at the k-th FLUSH) while the services keep running past the client deadline | `cf=238` (host token; `disk=cut dseed=0`) | `594a0dd4e8a6f192` | `acked but not marked published` |
| d12-shared-key-buffer | concurrency, data race | formats the Kafka record key into one reused per-server buffer ("no allocation per order"); a concurrent request rewrites the key before the record is serialized | two produces in flight at once (concurrent clients, or a stuck produce plus the next order) | `Q=2` | `843f83fcf2fab9bd` | `present N times` / `kafka order=N ... MISSING` |

Each trigger failed twice on its image with the same fingerprint, and passed on the control.
Control fingerprints (verdict OK): d8 trigger `909faca647bf1e4a`, d9 `b530be611c79196f`,
d10 `98ccdb150d322993`, d11 `5036d0ec822239b8` (acked 23 of 30, no violation), d12
`9a0b90a1febea0be`. Records: `/var/tmp/m59-bench/confirm.out` and `confirm2.out` on the bare-metal machine.

**Stand-in images (superseded).** A first build used the stand-in `hvx_seedrng` (`01adf1bd…`,
image names without `-pin`, still on the bare-metal machine). On those images the triggers were
`Q=2d`/`Q=2`/`X=front@9~20`/`cf=254`/`Q=2`, with fingerprints d8 `1ea2367350c96f6c`,
d9 `b79e932f269c77f0`, d10 `184af14cf3e7345a`, d11 `894c669947627042` and d12 `e2b254736e39ec3b`.
The seeder's bytes change guest timing. On the -pin images, d8 and d12 fail on the same
triggers with the same verdicts. d9 (`Q=2`), d10 (`X=front@9~20`) and d11 (`cf=254`) no longer
fail there, so new points were searched for. The verdict kinds are the same, but d10 now
reports order 13 instead of 10 and d11 order 22 instead of 23. On the -pin images d9 crashes
with Q=3, 4, 6, 8, 4d and 8d, but not with Q=2 or 2d. d10 fails on 26 of the 40 `frontkill`
points (M50's explore-cc1 failed 25 of 40).

### Strategies on d8–d12 (-pin images)

Each cell is 3 repetitions of up to 60 runs, with an early stop. Repetition r uses seeds
r*1000+1.., as in M54. Settings: `--concurrency` (60% of schedules get `Q=<n>[d]` from its own
random stream), 1 worker, and `--window 1`, so a control check never runs next to another VM.
A cell gives the repetitions that detected the defect, the run of the first detection in each
repetition, and the median. Every detection reproduced its fingerprint, matched its defect's
oracle line and passed on the control (no candidate was rejected). Module srcversion
`38E88060EEC5F955DFDFD03`. Results are in `/var/tmp/m59-bench/<strategy>/results.jsonl` on

| defect | random+cc | swarm+cc | process+cc | swarm+bg+cc | random (no cc) |
|---|---|---|---|---|---|
| d8-no-order-lock | 3/3 (4, 1, 7) med 4 | 3/3 (5, 2, 46) med 5 | 3/3 (4, 1, 4) med 4 | 3/3 (5, 2, 46) med 5 | 0/3 |
| d9-redis-unlocked | 3/3 (1, 1, 2) med 1 | 3/3 (5, 31, 1) med 5 | 3/3 (3, 2, 7) med 3 | 3/3 (30, 31, 1) med 30 | 0/3 |
| d10-outcome-in-memory | 0/3 † | 0/3 † | 0/3 † | 0/3 † | – |
| d11-ack-unflushed-flag | 0/3 † | 0/3 † | 0/3 † | 0/3 † | – |
| d12-shared-key-buffer | 3/3 (1, 1, 2) med 1 | 3/3 (5, 2, 1) med 2 | 3/3 (1, 1, 2) med 1 | 3/3 (5, 2, 1) med 2 | 3/3 (2, 6, 14) med 6 |
| detections of 15 | 9 | 9 | 9 | 9 | 3 of 9 |

† **d10 and d11 are unreachable by every generator. The 0/3 comes from the generators, not
from runs.** Every trigger found for d10 kills the order service with a timer (`X=front@...`).
Every trigger found for d11 cuts power at the k-th disk FLUSH (`cf=<k>`). I rendered all 180
schedules per strategy (the same seeds and options as the cells above). Not one of them
contains `X=` or `cf=`; `X=` appears only in `--sweep frontkill`, and `cf=` only in the
`hydo/cutflush` sweeps. The nearest things the generators do emit cannot trigger these
defects:
- `P=front@...` (in 39 of the 180 `process` schedules) pauses the order service with SIGSTOP.
  Its memory survives, so d10's lost state never happens.
- `C=` is a power cut that freezes every server at once, so no ACK can follow it. d11 needs the
  services to keep running after the disk stops completing FLUSHes.

This is a gap in the explorer: none of its generators can express a crash of the service under
test, or a disk that stops completing FLUSHes while the services keep running. The two
sweeps do contain the triggers: d10 fails on 26 of the 40 `frontkill` points and d11 fails on
the flush-cut points below. As a spot check, `random+cc` ran 23 schedules of repetition 1 on
d10 before the run was stopped, with no detection.

What the concurrency defects show:
- With `--concurrency` every strategy finds d8, d9 and d12 in every repetition.
- Without it, d8 and d9 are never found in 180 runs. The client sends one request at a time,
  and in these schedules no fault makes a stuck request overlap a retry of the same order.
- d12 is found without concurrency (medians: 6 runs without, 1 with). A killed Kafka leaves a
  produce stuck in the producer while the next order rewrites the shared key buffer, so the
  stuck record goes out with a wrong key (`kafka order=1 expected=100001 got=100009`).
- BUGGIFY slowed swarm on d9 (repetition 1: run 5 became run 30). The fault points change
  request timing, and the shared-connection panic needs two Redis calls at the same moment.
- `process` gives the most even results (medians 1–4).
- On the stand-in images (superseded), random+cc and swarm+cc found d8, d9 and d12 at exactly
  the same runs as in the table.

### Control sweeps (-pin images)

`/var/tmp/m59-bench/ctl-sweep.table` and `d11-sweep.table` on the bare-metal machine:
- **Control, all 40 `frontkill` points: 40 OK.** On d10 the same points give 26 duplicates.
  So the `published = 2` state in MySQL (M50 fix 3) is what makes a service crash safe.
- **Control, 20 flush-cut points: 20 OK.** The points are k = 204–283, the M58 failure points
  first and then every 4th k up to 283.
- **d11, the same 20 points: 2 VIOLATIONs.** cf=238 fails on order 22 (`594a0dd4e8a6f192`) and
  cf=279 on order 26 (`b29d83e1680d…`). A partial run before them covered k = 200–203 on d11, and
  all 4 points were OK.
- So the control's extra write closes the M58 window at the points where the defect is open.
  This was tested at 20 cut points, not at all 118.

### Files and commands (M59)

- `base/main-3773607.go`: the order service at 3773607.
- `base/main-m59-control.go`: the control (3773607 plus the M58 fix).
- `patches-m59/*.patch` and `build-bench-m59.sh`: the defects and how to build them. Builds go on
  the build machine with the pinned tools:
  `BENCH_SUFFIX=-pin bash hydo/bench/build-bench-m59.sh`, about 15 s per image.
- `run.py` options: `--defects m59` (only d8–d12), `--concurrency`, and `--window N`
  (schedules in flight). `HYDO_M59_SUFFIX` picks the image set; the default is `-pin`, and `""`
  picks the stand-in set.

      hydo-bench confirm --defects m59 --workers 1 --out /var/tmp/m59-bench/confirm
      hydo-bench run --strategy random|swarm|process [--buggify] --concurrency --defects m59 \
          --reps 1-3 --budget 60 --workers 1 --window 1 --out /var/tmp/m59-bench/<label>
      hydo-bench table /var/tmp/m59-bench/{random,swarm,process,swarm-bg}-cc /var/tmp/m59-bench/random-nocc

## m59 images with the Kafka retention fix and the M67 harness fix (`-pin-kr`, 2026-10-23)

The `-pin` m59 images carry two unrelated problems: the Milestone 63 Kafka retention bug (a
forward clock step of more than 7 days deletes acked orders; it made control runs fail and cut
d11 repetitions short), and the pre-M67 harness (an order service that cannot restart after a
`cf=` cut ends the run with `ERROR front door restart`). Branch `bench-m59-kr` (from `57864c6`)
applies exactly two changes and nothing else: `log.retention.ms=-1` in the stack's Kafka
`server.properties` (`build-stack.sh`, the M63 line) and the M67 change to
`hydo/explore/stack-explore.sh` (commit `941f9f3`, cherry-picked from a benchmark branch commit). MySQL's binary
log (the other M63 change) is deliberately NOT applied, because it closes d11 by itself.

    BENCH_SUFFIX=-pin-kr bash hydo/bench/build-bench-m59.sh   # build machine, pinned tools

Images `explore-bench-m59-<name>-pin-kr-disk.img` (each built twice, byte-identical; seeder
`c2a440c5…`): control `61e843f0…`, d8 `76d77289…`, d9 `ac4ee872…`, d10 `a13fbfee…`, d11
`be42df1c…`, d12 `b710296a…`. Against each `-pin` image: the MBR and the data partition are
byte-identical, the root partition has the same 14,026 files with the same modes and owners, and
only `/etc/kafka/server.properties` (one added line) and `/etc/stack-explore.sh` (the M67 change)
differ; `/bin/hydostack` is byte-identical per defect. They are the default m59 set in `run.py` since 2026-10-23 (`HYDO_M59_SUFFIX=-pin` selects the earlier set).

Checks (bare-metal machine, results `/var/tmp/kr`): `confirm.py --defects m59` — d8 `Q=2d` `6a56326d6de88bba`, d9 `Q=4` `6a5f06b675687ac4`, d11 `cf=238` `adadedf9698291bd` (order 22), d12 `Q=2` `d89a283a66ac19a9`, each twice identical; d10's old trigger `X=front@12~20` passes on the new image (timing moved), so its trigger here is `X=front@9~20` (`5d03c640c24f667d`, `kafka order=10 present 2 times`, three times identical; the control passes it, `20a1bc5f8906233b`); `frontkill` 24 of 40 fail on d10, 40 of 40 pass on the control. The control passes every trigger and the M63 clock-jump candidate (`496ca3033e335c4c`, twice; the `-pin` control still fails it, `9577ae9341ea86c2`). A workload makes 365 flushes (309 on `-pin`); every flush after 311 comes after the last order, so the cells keep `--cf-range 1-309` and their schedule strings equal M67's. d11 cut sweep k=200..364 step 4: k=248 (order 23) and k=280 (order 26) fail, twice each; the control passes both.

| defect | process, M67 (`-pin`) | process, `-pin-kr` | swarm, M67 | swarm, `-pin-kr` |
|---|---|---|---|---|
| d10 | 3/3 (14, 1, 10); 2/3 by signature | 3/3 (7, 35, 10); 2/3 by signature | 1/3 (5) | 2/3 (20, 17); 1/3 by signature |
| d11 | 1/3 (3) | 0/3 | 2/3 (13, 4) | 0/3 |

All detections reproduced. Off-signature hits: `mysql table unreadable` after a MySQL kill near the end (harness artefact, like M55's Kafka one) and `M=0` + a MySQL kill (the relaxed setting's documented window). The four schedules that found d11 in M67 give OK on the new image (the cut lands elsewhere). d11's window is about 3 of ~310 cut points and `cf=` is drawn in 40–60% of schedules, so three repetitions cannot separate a per-repetition rate between about 0 and 30%; 0/6 here and 3/6 in M67 are consistent. Branch `bench-m59-kr` (from pre-binlog `57864c6`) is a build recipe only — do not merge it.

## The manifest images (Milestone 135)

Since M135 the benchmark runs on images built from one manifest, `manifest/hydo.yaml` (the
`-pin-kr` control image's files and configuration as a manifest, M129, `docs/C2-BENCH.md`), with
`hydo build` and `hydo build --control agent`. Each order service (a defect or a control) gives a
shell image and an agent image that differ only in what `--control agent` changes.

**The order services** (M135): every order service is built from source by
`build-order-services.sh <dir>` (the pinned Go 1.26.5 as `HYDO_GO`; on the bare-metal machine
the copy of its Nix store path in `~/hydo-rp/nixgo`, which rebuilt the `-pin-kr` control's binary
`78290125…` byte for byte from that image's commit; the official go1.26.5 tarball gives other
bytes). The sources are put together as the stack builders do it: a copy of `hydo/stack` with the
stored base (`base/main-m59-control.go` for `control` and d8–d12 with `patches-m59/`,
`base/main-0112b48.go` without `buggify.go` for `nf`, `fix` and d1–d7 with `patches/`,
`base/main-7e32c4a.go` for d0, `hydo-bench addbuggify` over the same bases for `bg-*`). The
module files are `hydo/stack/go.mod` and `go.sum`, with **franz-go v1.22.2** since M135 (v1.17.1
let the idempotent producer acknowledge a record it never wrote, franz-go issue 1484, M133); the
stack images keep v1.17.1 (`hydo/stack/go.mod.baseline`, which `build-stack.sh` uses). Two
builds give the same bytes. Binaries (sha256 prefixes): control `4aefc78f…`, d8 `9d6c5865…`, d9
`686a5f40…`, d10 `18c74d20…`, d11 `99bb7f05…`, d12 `55e6c743…`, nf and fix `0e8d9b79…`, d1
`a3b33974…`, d2 `409ce491…`, d3 `4ef29104…`, d4 `4bfb8a19…`, d5 `33771b66…`, d6 `0c500027…`, d7
`31b37c9b…`, d0 `6953a8d8…`; bg-control `3d96345e…`, bg-d0 `d36425d9…`, bg-d1 `516696a9…`, bg-d2
`2c4c5559…`, bg-d3 `06b8bd83…`, bg-d4 `1b563425…`, bg-d5 `65edc90d…`, bg-d6 `a601a240…`, bg-d7
`3c190f30…`.

**Building the images** (on the bare-metal test machine; no Nix): `ORDER_BIN=<dir>
build-bench-manifest.sh <work dir> <release dir>` with `HYDO_AGENT_BIN` = the pinned hydo-agent
(0.2.0, `b72c5caa…` since M127, docs/BUILD-PINS.md; the images of M135 were built with M131's `1f517e04…`). `<work dir>/benchroot` is the debugfs rdump of the
`-pin-kr` control image's root (the MySQL and OpenJDK closures, Kafka's libs, the pinned Redis);
the release dir is a Linux release layout with this tree's `bin/hydo` and `hydo/`. Without stem
arguments it builds the default set: the control in full, then every other order service as a copy
with `/bin/hydostack` swapped in (`agent-patch.sh --swap`; M129 proved a swapped image equal to a
full build). `BG=1` adds the BUGGIFY order services (`bg-*`), `KEEP=1` keeps images that exist.
Without `ORDER_BIN` the order services come from the stack images' `/bin/hydostack` (franz-go
v1.17.1, as M129 built its images). Output, in `$OUT` (default `<work dir>/build`, with `bzImage`
and `initramfs-kafka.cpio.gz` linked in):

| stem | order service | used as |
|---|---|---|
| `control` | the M59 control | the control of d8–d12 |
| `d8-no-order-lock` … `d12-shared-key-buffer` | the M59 defects | d8–d12 |
| `nf` | the M46 order service (the M39 fix) | the control of d1–d7 |
| `d1-kafka-first` … `d7-skip-last-record` | the M54 defects | d1–d7 |
| `d0-old-front-door` | the pre-M39 order service | d0 |
| `fix` | the same binary as `nf` | the control of d0 |
| `bg-control`, `bg-d0-…` … `bg-d7-…` (`BG=1`) | the M56 BUGGIFY order services | `--bg-images` |

The images are `explore-ob-bench-<stem>-disk.img` (shell) and `explore-ob-bench-<stem>-agent-disk.img`
(agent). Run hydo-bench with `HYDO_BUILD=<that directory>`; `--kind agent` is the default,
`--kind shell` picks the shell images, `--kind stack` the stack images in this file's older
sections. `list --images` takes a defect name or `control:<defect>` for the kind's image. Every
stack-sized guest needs about 6 GiB of the bare-metal machine's memory: run one worker (`--workers 1`) and start
a step only when `free -m` shows at least 6 GiB available (`c2-cells.sh` waits for it).

**What differs from the stack images.** The generic init (shell kind) or the host controller
(agent kind) runs the workload instead of `stack-explore.sh`: other console lines, a restart of
every dead service before the retry pass, no periodic checks, so the fingerprints and some trigger
points differ from M54–M108's. `Q=` and `M=` are knobs of the manifest; `X=` (the stack's timed
kill) works on the agent kind only (the generic shell init ignores it; the format reserves the
letter, so it cannot be a knob). `B=` reaches the order service's BUGGIFY points on both kinds
(`buggify: true`). stack-explore.sh's M67 path (an order service that cannot come back after the
workload: no retry pass, the power cut's recovery boot decides) is in the manifest too, below.

**M67's path in the manifest.** After a `cf=` power cut the data disk stops completing writes,
MySQL's sessions hang, and an order service killed late in the run (`X=front@…`, or `K=front`
at the last step) cannot start again: it exits. stack-explore.sh then leaves the un-ACKed orders
un-ACKed, skips the retry pass and ends the workload, so the host's recovery boot gives the
verdict; without a cut it ends with `ERROR front door restart`. The generic init and the host
controller end such a run with `ERROR restart after the faults (front)` (M129 saw one, process
seed 3003). The manifest writes the path with the format's existing features:
- the order service runs as `/bin/busybox sh /etc/hydo-bench/front.sh`: `/bin/hydostack server`,
  and when that exits after the workload's last step (the `run:` command leaves
  `/tmp/hydo-bench-steps-done` after the range that ends at step 30), the script stays up and
  prints `hydo-bench: the order service exited after the workload's last step (status N); it
  stays down` (to its log and the console);
- `ready:` accepts that line as well as `server listening`, so the restart does not end the run;
- `retry:` sees the line, prints `hydo-bench: the order service cannot restart; no retry pass,
  the un-ACKed orders stay un-ACKed` and skips the retry pass (marker `/tmp/hydo-bench-no-retry`);
- the third invariant, `front.restarted`, answers "could not be evaluated" (exit 2) when the retry
  was skipped, so a run without a cut ends in `ERROR invariant front.restarted could not be
  evaluated: front door restart: …` unless another invariant is violated (stack-explore.sh's ERROR
  comes first; here a violation comes first). In a recovery boot it holds.
A kill or a pause still reaches the order service (faults act on a service's whole process
tree). An order service that hangs instead of exiting is still `ERROR restart after the faults`
(stack-explore.sh's `NOT ready` case is not covered). The format has no key for a restart that may
fail; docs/HISTORY.md "Milestone 135" says what such a key needs to do.

The M67 path, measured (twice each, identical): M129's ERROR schedule `disk=cut
dseed=806555b3e1262cc5 cf=147 | run P=redis@10+3 T=26:-3600 X=front@20~151` on the agent control
gives OK `acked=11/30` from the recovery boot (`91ebd4a2e0731975`); `disk=cut dseed=0 cf=147 | run
K=front@29+1` takes the path on both kinds (agent OK `acked=13/30` `c7e09c18bf68d064`, shell OK
`acked=14/30` `ad4939963f7aefc8`), and both consoles show the two `hydo-bench:` lines.

### The triggers on the manifest images (Milestone 135)

`hydo-bench confirm --kind agent --defects <all thirteen> --base --workers 1` on the images above
(franz-go v1.22.2, hydo-agent 0.2.0; results `/var/tmp/m135/confirm-agent` on the bare-metal machine): every
trigger failed twice with the same fingerprint and the defect's signature (class
`durable-defaults` for all), and passed once on its control. Three triggers were measured again
first, because the agent images' timing moved them (`hydo-bench list`, one run per point): d9 on
`Q=2,3,4,2d,3d,4d` crashes for `Q=3` and `Q=4d` (M129's agent images: five of six, M108's `Q=4`
no longer); d10 on `X=front@6..13~20` fails at 7, 9, 10, 11 and 13 (the control passes all eight,
so M72's `@9` stays); d11 on `cf=245..305` fails at 9 of 61 points, k = 249, 250 (order 23), 260
(24), 271, 272 (25), 282 (26), 292 (27), 302, 303 (28), on swapped images built before the swap
was made byte-reproducible (the fingerprints of those runs are not the table's; `cf=260` was then
confirmed on the final images).

| defect | trigger (agent kind) | verdict | fingerprint (twice) | control (OK) |
|---|---|---|---|---|
| d0-old-front-door | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | order=11 is in kafka (1 times) but the client was never ACKed | `8a7a57a44b2e1e8a` | fix: acked=24/30 `12ae3bb9fa046080` |
| d1-kafka-first | `disk=none dseed=0 \| run K=mysql@10+1` | kafka order=10 present 2 times | `27776b3e63e42ff0` | `f549cfc024ca4822` |
| d2-batched-publish-flag | `disk=cut dseed=0 \| run C=12` | mysql order=10 acked but not marked published | `5e15465dac8cb3e1` | acked=12/30 `9a5d5bdfccc2dafb` |
| d3-blind-retry | `disk=none dseed=0 \| run P=kafka@5+2` | kafka order=5 present 2 times | `2cd0278ca81afecf` | `001e6a275980fa96` |
| d3-blind-retry | `disk=none dseed=0 \| run O=kafka@5-10:resp:loss,100%` | kafka order=5 present 2 times | `1cac02b0305f5a9c` | `240b2565dac1e000` |
| d4-produce-timeout-definite | `disk=cut dseed=0 \| run O=kafka@5-10:resp:loss,100% C=12` | order=5 is in kafka (1 times) but the client was told it failed | `f1186262b0bc8037` | acked=7/30 `66cb2aa449d3f6b7` |
| d4-produce-timeout-definite | `disk=cut dseed=0 \| run P=kafka@5+2 C=8` | order=5 … told it failed | `d7bee1f2e6ba3f5a` | acked=6/30 `61d35b92a2c3eafa` |
| d5-myisam | `disk=cut dseed=0 \| run C=15` | mysql order=0 expected=100000 got=MISSING (kafka has it: true) | `b34e7ff647075d43` | acked=15/30 `2dfba0a1d795afe7` |
| d6-client-deadline-definite | `disk=none dseed=0 \| run P=kafka@5+2` | order=5 … told it failed | `77c5094837f0e617` | `001e6a275980fa96` |
| d7-skip-last-record | `disk=none dseed=0 \| run P=kafka@28+2` | kafka order=29 present 2 times | `32a0fc66c14173c0` | `eecd63a9e8d1ce49` |
| d8-no-order-lock | `disk=none dseed=0 \| run Q=2d` | kafka order=0 present 2 times | `112bd790c58cf83a` | `9fc6b149e94b7cca` |
| d9-redis-unlocked | `disk=none dseed=0 \| run Q=3` | [front.no_crash] front door crashed: panic: runtime error: slice bounds out of range [15:10] | `e5eedcdbc635fd62` | `643a520d0576c694` |
| d10-outcome-in-memory | `disk=none dseed=0 \| run X=front@9~20` | kafka order=10 present 2 times | `d15cccef5888b5d0` | `196d342bbd850774` |
| d11-ack-unflushed-flag | `disk=cut dseed=0 cf=260 \| run` | mysql order=24 acked but not marked published | `caa0b4855957a6da` | acked=24/30 `59d93b039af27ba7` |
| d12-shared-key-buffer | `disk=none dseed=0 \| run Q=2` | kafka order=0 present 2 times | `324da2b742df58e9` | `5aa98bced794bdeb` |

The control of d1–d7 (`nf`, the M46 order service) keeps its known bug (section "A bug in the fixed
front door"): `disk=none dseed=0 | run O=kafka@25-30:resp:loss,100%` gives `kafka order=26 present
2 times`, twice `4eb6204775941205`, as on the stack image; the franz-go upgrade does not change it
(that bug is the order service's lookup racing its own in-flight produce, not issue 1484). Other
checks on the agent control: `disk=none dseed=0 | run` OK `acked=30/30` `f0017db9b378d8e4`, and
`disk=none dseed=0 | run P=kafka@14+16` (the M129/M133 lost order) OK `a41a29598b2738ea`.

On the shell kind (`hydo-bench confirm --kind shell`, the same images' shell builds, results
`/var/tmp/m135/confirm-shell`), every trigger but d10's failed twice with one fingerprint and the
signature, and every control passed: d0 `f3823c5fb59509de`, d1 `88281feb616bbc1e`, d2
`8a454dfde2e16c80`, d3 `b79868d500d1c069` / `f2dec81fe23a9886`, d4 `e0c32971fc99b8d8` /
`6ae6aa92727d25d0`, d5 `1f33e100c6615eb6`, d6 `8b88d1a9c2eaea7f`, d7 `de93bec6815cf318`, d8
`a37e377a57bfde73`, d9 `Q=3` `67c6f9f7f92acd2d`, d11 `cf=255` `4df9e6583e79423c` (order 24), d12
`a183710476e82968`; the control's known bug twice `4edd5c6d1244cc8c`. d10's `X=front@9~20` passes
(`e5232a07be7245a2`) because the generic shell init ignores `X=`: d10 has no trigger on the shell
kind. Measured first on the shell kind: d9 crashes for every `Q=` but `Q=2d`; d11 fails at 8 of
the 61 points (245, 255, 256, 266, 277, 287, 297, 298).

## The manifest images after the switch (Milestone 136)

Since M136 hydo builds agent images only, so the manifest images are agent images only:
`build-bench-manifest.sh` builds one image per stem (`explore-ob-bench-<stem>-agent-disk.img`, the
`-agent` kept so that the names stay apart from M135's shell images in older directories), and
`hydo-bench --kind shell` is refused (`--kind agent`, the default, and `--kind stack` remain).

**M67's path is a manifest key.** The order service has `restart_may_fail: true`
(MANIFEST.md "A restart after the faults that may fail"): when its restart at the heal fails (it
exits, or its `ready:` check does not pass within `ready_timeout`), the run goes on without the
retry pass, the workload ends, a `cf=` power cut takes the run into its recovery boot, whose
invariants give the verdict, and without a cut the verdict is `ERROR restart after the faults
(front)`. The wrapper script `/etc/hydo-bench/front.sh`, the `ready:`/`retry:` that read its line
and the invariant `front.restarted` of M135 are gone; the manifest's order service is
`/bin/hydostack server` again, its `run:` and `retry:` are M129's. An order service that hangs on
its restart instead of exiting (M135: not covered by the wrapper) is now covered too.

**The images** (M136, on the bare-metal machine, `~/c3-work/bench/build`): the control built in full twice
(`96e63d7a…`, both builds the same bytes), every other stem a swap of it (`agent-patch.sh --swap`), with the pinned agent
`b72c5caa…` (M127's fix for children's signal mask; M135's images carried M131's `1f517e04…`)
and M135's order services (franz-go v1.22.2). Hashes: control `96e63d7a…`, d8 `3036cad8…`, d9 `d80cc957…`, d10 `49f015d5…`, d11 `a4d186da…`, d12 `33b19218…`, nf and fix `fe2e99f6…` (one binary, one image), d1 `f75d80a8…`, d2 `5b8c794b…`, d3 `33c89216…`, d4 `d1decb3c…`, d5 `2f04a0bb…`, d6 `aa707bf5…`, d7 `0f2ee55b…`, d0 `13982f64…` (the `bg-*` images were not rebuilt).

**Measured (twice each, one fingerprint):** on the control, `disk=none dseed=0 | run` OK `acked=30/30` (`f4865ef32569d91b`), `P=kafka@14+16` (M133's lost order, fixed by franz-go v1.22.2) OK `acked=30/30` (`27ace56c076f0aae`), M129's ERROR schedule `disk=cut dseed=806555b3e1262cc5 cf=147 | run P=redis@10+3 T=26:-3600 X=front@20~151` OK `acked=11/30` from the recovery boot (`8a8b1cb779a686b3`, as M135's wrapper gave), and `disk=cut dseed=0 cf=147 | run K=front@29+1` OK `acked=14/30` from the recovery boot (`955d5ef00c3b919a`). The last one first ended `ERROR front restart failed`: a `K=` kill whose restart point is the workload's end restarts the service after the last step, where the key did not apply yet; the controller now treats a failed restart there as a restart after the faults (left to the heal's pass, `tests/restart_may_fail.rs` `a_kill_restart_at_the_workloads_end_follows_the_key`), and the run gives the recovery boot's verdict as M135's wrapper did (`/var/tmp/m136/extra/m67b`).

**The triggers on the M136 images** (`hydo-bench confirm --kind agent --defects all --base`,
results `/var/tmp/m136/bench/confirm` on the bare-metal machine): every trigger of d0–d7 failed twice with one fingerprint and its defect's signature, and every control passed it: d0 `cbbf4fc222f54ca8` (order 11 in Kafka, never acknowledged; fix OK `bba72ff55f35cc98`), d1 `adc48e618fcc6b76`, d2 `37535b22454d4f29`, d3 `2bb4fc57f3c55ec9` / `a18e87bbf308b07f`, d4 `8a11c9e2313997c9` / `93aaa0248e846692`, d5 `b430e80234ab276a`, d6 `ec416019120fc489`, d7 `4abeae3a3020e667`; the control's known bug (`O=kafka@25-30:resp:loss,100%`) is still there, twice `57101737a30caa64`. Of d8–d12 (`--defects m59`), d8 `Q=2d` `c310b97659bbe1b6`, d10 `X=front@9~20` `3e2bdc70fa6bcb84` (order 10 twice) and d12 `Q=2` `e0cab9aa24458f7a` failed twice with their signatures and passed on the control (`d5a4fcd2a8cf5f88`, `2ccfc3c2262bfd26`, `eb8fb562a7603135`); the two timing-dependent triggers moved: M135's d9 `Q=3` and d11 `cf=260` pass on the M136 images. Measured again (one run per point): d9 crashes for `Q=2`, `Q=4`, `Q=3d` and `Q=4d` of `Q=2,3,4,2d,3d,4d`; d11 fails at 9 of the 61 points `cf=245..305` (249, 261, 262, 272, 273, 283, 284, 294, 304; M135: 249, 250, 260, 271, 272, 282, 292, 302, 303, the same shape one flush later). The new triggers (`defects.rs`), confirmed twice with one fingerprint and the signature and passed on the control: d9 `Q=2` `05ff670ada38dab7` (the M50 panic; control `eb8fb562a7603135`), d11 `cf=261` `46be4104b850feec` (order 24 acknowledged but not marked published; control `c2601b12624791af`).

## Reactive exploration (Milestone 139)

`hydo-bench run --reactive fault|heal|both` adds reactions (`A=<event>[@<step>][+<ms>]:<action>`,
hydo/onboard/MANIFEST.md "Reactions") to a strategy's schedules from their own random stream: the
host controller applies a fault (a power cut at that instant, a kill, a stop) or a heal (the
`P=` service resumed) at the first assertion record of an event the order service or its client
writes (`server.store_error`, `client.outcome_unknown`, ...). `--dry` prints a cell's schedules
and starts no VM. Plans: `c2-plans/m139.txt` (`px` = `--strategy process --more-faults --cf-range
1-309`, `pxr` = the same with `--reactive both`; d0–d7 and d10 three repetitions, d11 ten) and
`c2-plans/m139-modes.txt` (`pxf` = `--reactive fault`, `pxh` = `--reactive heal`), run by
`c2-cells.sh` with `KINDS=agent` on the M136 images; results `/var/tmp/m139/{cells,modes}` on the
box (`hydo-bench table` over the step directories).

| defect | px | pxr (both) | pxf (fault) | pxh (heal) |
|---|---|---|---|---|
| d0-old-front-door | 3/3 (7\*, 1, 1) | 3/3 (7\*, 2, 1) | 3/3 (7\*, 1, 1) | 3/3 (7\*, 2, 1) |
| d1-kafka-first | 3/3 (2, 2, 4) | 3/3 (2, 2, 2) | 3/3 (2, 2, 2) | 3/3 (2, 2, 4) |
| d2-batched-publish-flag | 3/3 (6, 1, 3) | 3/3 (6, 5, 2) | 3/3 (6, 1, 2) | 3/3 (6, 5, 3) |
| d3-blind-retry | 3/3 (4, 4, 4) | 3/3 (4, 4, 4) | 3/3 (4, 4, 4) | 3/3 (4, 4, 4) |
| d4-produce-timeout-definite | 3/3 (7\*, 1, 1) | 3/3 (7\*, 18, 7\*) | 3/3 (7\*, 1, 30\*) | 3/3 (7\*, 18, 1) |
| d5-myisam | 3/3 (7\*, 1, 1) | 3/3 (7\*, 1, 1) | 3/3 (7\*, 1, 1) | 3/3 (7\*, 1, 1) |
| d6-client-deadline-definite | 3/3 (4, 1, 1) | 3/3 (4, 4, 4) | 3/3 (4, 1, 4) | 3/3 (4, 4, 1) |
| d7-skip-last-record | 3/3 (17, 9, 4) | 3/3 (17, 9, 6) | 3/3 (17, 9, 4) | 3/3 (17, 9, 6) |
| d10-outcome-in-memory | 3/3 (7, 44, 10) | 3/3 (7, 44, 10) | 3/3 (7, 44, 10) | 3/3 (7, 44, 10) |
| by signature, of 27 | 24 | 23 | 23 | 24 |
| runs to the first detection, sum | 154 | 187 | 183 | 181 |
| d11-ack-unflushed-flag (ten repetitions) | 2/10 (3, 8) | 2/10 (8, 25) | – | – |

The run of the first detection in each repetition; an asterisk: that detection's verdict is not
the defect's signature (seed 1007's order-10 duplicate after `X=front@8~160`, which every
strategy draws). **Negative.** No defect is found more often, and the sums of runs to the first
detection are larger with every reactive mode. Per seed (the same base schedule in both columns),
of the 178 seeds of d0–d7 and d10 both px and pxr ran, 168 give the same verdict class; the
reactions turned 7 violations into passes and 3 passes into violations; of d11's 459 seeds 457 are
the same, one each way. Three detections came from a reaction where the base schedule passed:
d1 and d2 in repetition 3 (seed 3002, `A=server.redis_redialed@11:kill-mysql`: run 2 instead of
4 and 3) and pxr's d11 hit at seed 10025 (`T=19:300 O=kafka@8-10:resp:loss,48%
A=server.outcome_unknown:kill-front` with `cf=33`; the base schedule passes there, while px's
second d11 hit, seed 2003, passes with pxr's heal token added). Why the reactions cost more than
they gave: a reaction that comes early pre-empts the later fault that made the
base schedule fail (seed 3001: `A=client.place_failed+50:cut` cuts the power at order 4, before
the base's `C=30`, which is where d4 and d6 fail); a heal resumes Kafka while d7 needs it down to
the end (seed 3004), or its event never comes (`client.outcome_unknown` is not written when the
order service answers with an error) and the pause it lengthened to the end hides the base's
failure (seed 2001, d0, d2, d4, d6). Reactions acted in 504 of the 792 drawn (heals 324 of 370;
cuts, kills and stops 40–48 %). The lesson of M41–M56 holds: where faults land and in which
combination matters more than how the next fault is chosen, and a fault anchored to an event the
system writes lands in a narrower set of places than a seeded one.

The token stays useful by hand, to aim a fault at a moment that is already known to matter
(`hydo-bench list`, each schedule twice with one fingerprint, `/var/tmp/m139/demo`): `disk=cut
dseed=0 | run O=kafka@5-30:resp:loss,100% A=client.place_failed+2000:cut` (a power cut two seconds
after the first failed order, with Kafka's replies lost) fails d4 with its signature (`order=5 is
in kafka … told it failed`, `dc71e9dd855afb04`) and passes on its control; `disk=none dseed=0 |
run O=kafka@5-10:resp:loss,100% A=client.outcome_unknown+20:kill-front` fails d10 (`kafka order=5
present 2 times`, `5dad4111935ccd9d`) and passes on its control (on the d1–d7 control it hits
that control's known duplicate). `P=kafka@5+25` with and without `A=client.outcome_unknown@5+500:
cont-kafka` gives one fingerprint on d4 (`55c8b28686400e7b`): there the order service never writes
that event, the reaction never acts, and the guest runs exactly as without the token.

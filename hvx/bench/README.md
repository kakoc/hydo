# Bug benchmark for the stack explorer (Milestone 46b)

Every explorer strategy comparison up to Milestone 45 rested on ONE bug: the pre-M39 front door
(publish to Kafka, then fail the order because the Redis cache was down). This benchmark gives
the comparisons several targets of different kinds and difficulty. Each defect is a small,
realistic patch to a copy of the fixed front door (`hvx/stack/main.go`); everything else in its
image is byte-for-byte the control image `explore-nf-disk.img` (same guest, same M46 fault
tokens). The control rebuilt from this tree is byte-identical to the shipped
`explore-nf-disk.img` (`e5d8d6fb…`, front door `1f18f855…`).

## Detection

A run DETECTS its image's defect when (1) its schedule has durable settings and an honest disk
(explore.py class `durable-defaults`: no `M=`/`R=` knob, disk `none` or `cut`), (2) its verdict
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


## Files

- `patches/<defect>.patch` — the defect, a unified diff against `hvx/stack/main.go`
  (generated by `mkpatches.py` from the comments in it).
- `build-bench.sh` — copies `hvx/stack`, applies a patch, builds
  `build/explore-bench-<defect>-disk.img` with `build-stack.sh` (`STACK_OUT=explore-bench-*`
  gets the explorer's serve mode; `STACK_GO_SRC` points at the patched copy). On the build machine
  box the pinned static musl gcc and Redis are gone (M46), so pass `HYDO_GCC_MUSL`,
  `HYDO_MUSL_ROOT`, `HYDO_REDIS_OUT` as M46 did. A build takes ~15 s.
- `confirm.py` — runs every trigger twice on its defect image (both must be the same
  VIOLATION fingerprint) and once on the control (must be OK).
- `run.py` — runs a strategy on every defect image with an early stop at the first detection;
  `run.py --table <dirs>` prints the comparison table.

## Commands (on the box, from `~/hydo-bench/hvx/bench`)

    python3 confirm.py --out /var/tmp/m46-bench/confirm
    python3 run.py --strategy random  [--new-faults] --defects all --reps 1-3 --budget 60 --workers 3 --out /var/tmp/m46-bench/random
    python3 run.py --strategy swarm   [--new-faults] ...
    python3 run.py --strategy portfolio ...
    python3 run.py --table /var/tmp/m46-bench/{random,swarm,random-nf,swarm-nf,portfolio}

## Results (2026-10-18/19, module srcversion `53856EFDC33F55145778DA8`, 3 workers)

3 repetitions × up to 60 runs per defect and strategy; repetition r uses seeds r*1000+1..
(the M42/M44 repetitions). Cell: repetitions that detected the defect, (run of first detection
per repetition), median. random/swarm stop at the first detection; portfolio always runs 60.
`+nf` = `--new-faults` (M46 pause / clock jump / one-way loss). Results in
`/var/tmp/m46-bench/<strategy>/results.jsonl` on the box; table `/var/tmp/m46-bench/table.md`.

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
`hvx/explore/RESEARCH-search.md`). `hydoassert.Buggify(id, msg)` declares a point; for a run each
point is enabled with probability 0.25 and an enabled point fires on each evaluation with
probability 0.25 (FoundationDB's defaults, `flow/include/flow/Buggify.h`:
`P_..._BUGGIFIED_SECTION_ACTIVATED{ 0.25 }`, `P_..._BUGGIFIED_SECTION_FIRES{ 0.25 }`), both a
pure function of the run's seed, the point and the hit index. The seed comes from the guest
token `B=<seed-hex>[,<enable-pm>[,<fire-pm>]]` (written to `/tmp/hydo-buggify`, read at the first
`Place`); `explore.py --buggify` gives every schedule one from a separate random stream, so all
other fields are unchanged. Without `B=` every point is off. A `HYDO-BUGGIFY` console line lists
the enabled points; each point is a `sometimes` property `buggify.<id>` recorded when it fires.

The 14 points (`hvx/stack/buggify.go`) wrap every store call of `Place` and take the code's own
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
results `/var/tmp/m56-bg/{random-bg,swarm-bg}` on the box; plain columns are Milestone 54's):

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

**Fixed front door (current `hvx/stack/main.go`, with the M50 fixes) under BUGGIFY**
(`explore-bench-bg-fixed-disk.img`, swarm, `--buggify --durable-only`, seeds 1–100): 100 runs,
**0 failures**; all 14 points fired (in 7–27 runs each). No new bug.

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

# Searching fault schedules and interleavings: what the literature says, and what hydo should adopt

> Note (M107): `explore.py` and the other Python files named below were the explorer and tools of
> the time. The explorer is now the Rust crate `hydo/crates/hydo-explore` (`hydo explore`, or the
> `hydo-explore` program, with the same options); the Python files were deleted in M107 and can
> be restored from commit 2a58c4b (`git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history` in a scratch checkout).
> The commands below are records of how things were run and measured.
>
> Note (M108): `portfolio.py`, `richguided.py`, `abranch.py` and the rest of `hydo/explore/archive/` were deleted
> in M108 (restore with `git show 705cf98:hydo/explore/archive/<file>`); the benchmark's `run.py` is now
> `hydo-bench run` (`hydo/crates/hydo-bench`).

Written 2026-10-20 for the explorer (`explore.py`, `portfolio.py`, `richguided.py`, `abranch.py`) and the bug benchmark (`hydo/bench`, CLAUDE.md M54). Every claim below comes from the source linked next to it; quotes are short. A few sources were read only through their abstracts or secondary summaries, and that is said where it applies.

## Where we stand (the problem this answers)

- Coverage guidance, mid-run branching, assertion-guided branch points and a bandit portfolio did not beat random or swarm schedules on our stack (M41, M44, M45). Swarm is the most reliable (M42, M54).
- On the 8-defect benchmark, all five strategies find the same 7 defects and none finds **d7** (Kafka lookup skips the last record). d7 needs a fault on Kafka at the very **end** of the workload (`P=kafka@28+2`), and the generator places each fault at a uniformly drawn order, so a d7-shaped schedule comes up roughly once per ~1000 runs. Branch `tail-sweep` is measuring that probability exactly and adding an end-of-workload sweep; it is not repeated here.
- A **narrow race** (SQLite 3.51.2 WAL reset, a window of a few hundred instructions) was not found even with 16× more ticks or inserted ticks (M49, M52).

So the two gaps are **where in time faults land** and **how preemptions relate to memory accesses**, not the amount of guidance.

---

## 1. How deep are real bugs? (bug studies)

**Idea.** Measure how many faults and events real failures need, and when they must happen. If the answer is "few", small bounded search is enough.

**Evidence.**
- Yuan et al., OSDI'14, 198 user-reported failures in Cassandra, HBase, HDFS, MapReduce, Redis ([summary](https://blog.acolyer.org/2016/10/06/simple-testing-can-prevent-most-critical-failures/), [paper](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/yuan)): "98% of the failures are guaranteed to manifest on no more than 3 nodes"; "most failures require no more than three input events"; "74% of them are deterministic"; "92%" of catastrophic failures come from "incorrect handling of non-fatal errors".
- Alquraan et al., OSDI'18, 136 network-partition failures in 25 systems ([paper](https://usenix.org/system/files/osdi18-alquraan.pdf)): "83% … require an additional three or fewer input events"; "All of the failures that involve multiple events only manifest if the events happen in a specific order"; "88% … manifest by isolating a single node"; "29% … are caused by partial partitions"; "80% … are either deterministic or have known timing constraints".
- Mohan et al., OSDI'18 (CrashMonkey/ACE, [arXiv](https://arxiv.org/pdf/1810.02904)): most reported file-system crash bugs "can be reproduced using small workloads of three or fewer" operations, and all came "from crashes after fsync() related system calls".
- NIST combinatorial testing ([bulletin](https://csrc.nist.gov/files/pubs/shared/itlb/itlbul2016-05.pdf)): most failures involve one or two parameters, and none seen involved more than six.

**Maps to hydo.** Our schedules already have 1–4 faults. What the studies stress, and we under-weight, is **order and position**: a fault must land at a particular step (Alquraan finding 8), and the error-handling path after it is where bugs live (Yuan, 92%).

**Effect on the benchmark.** Explains the result exactly: d7 is depth 1 (one Kafka pause) but needs a *specific position*. A small bounded search over positions will find it; more guidance will not.

---

## 2. Probabilistic Concurrency Testing (PCT) and its distributed variants

**Idea.** A bug has *depth* d = the minimum number of ordering constraints needed to expose it. PCT gives threads random priorities and inserts d−1 priority-change points at random steps. Each run finds any depth-d bug with probability at least 1/(n·k^(d−1)) for n threads and k steps ([Burckhardt et al., ASPLOS'10](https://www.microsoft.com/en-us/research/?p=158870); bound as stated in [Lockstep's PCT docs](https://hexdocs.pm/lockstep/Lockstep.Strategy.PCT.md)).

**Distributed variants.** PCTCP (OOPSLA'18) applies the idea to message-passing systems over the partial order of events, with a bound of 1/(w²·n^(d−1)) and bugs found in ZooKeeper and Cassandra ([SPLASH page](https://2018.splashcon.org/event/splash-2018-oopsla-randomized-testing-of-distributed-systems-with-probabilistic-guarantees)). taPCTCP (OOPSLA'19) adds trace awareness ([page](https://2019.splashcon.org/details/splash-2019-oopsla/61/Trace-Aware-Random-Testing-for-Distributed-Systems)). Only abstracts were read for both.

**Evidence it works.** Thomson, Donaldson & Betts, TOPC'16, 49 real buggy programs (SCTBench), 100,000 schedules each ([paper](https://www.doc.ic.ac.uk/~afd/papers/2016/TOPC.pdf)): "PCT d=3 found bugs in 48 of the 49 benchmarks — more than any other technique". The controlled **random** scheduler found 43, "more than IPB (38) and DFS (33)", and "for schedule limits between 10 and 1000, Rand finds up to 6 more bugs than" delay bounding. So random is a strong baseline, as we found too, and PCT is the one technique that beat it.

**Maps to hydo.**
- For **faults**, PCT's lesson is: pick the d−1 change points uniformly over the *events of the run*, not over a coarse grid, and keep d small. Our fault placement is already uniform over orders, so for depth-1 faults like d7 PCT adds nothing new by itself; the position grid is the issue (section 5).
- For **thread interleavings**, our tick jitter and inserted ticks (M43, M49) are PCT-like change points over *branch counts*. With k ≈ 10⁸–10⁹ branches per run, k^(d−1) is enormous, which is why a race window of a few hundred instructions is not hit: a PCT change point has to land inside the window, and a random branch count almost never does.

**Effect.** None on d7. On narrow races, PCT tells us *why* we miss them and that the fix is to shrink k: count change points over *relevant* events (shared-memory accesses, lock operations, syscalls) instead of all branches. See recommendation 4.

**Cost.** Low for fault placement (already have it); high for an access-level PCT.

---

## 3. Systematic bounded search (CHESS, delay bounding)

**Idea.** Enumerate all schedules with at most c preemptions (CHESS) or at most c "delays" of a deterministic scheduler (delay bounding), smallest bound first.

**Evidence.** CHESS found 9 previously unknown bugs, "each of which was exposed by an execution with at most 2 preempting context switches" ([PLDI'07](https://www.microsoft.com/en-us/research/publication/iterative-context-bounding-for-systematic-testing-of-multithreaded-programs/)). Delay bounding explores at most I^K schedules for I interleaving points and bound K ([MSR-TR-2010-123](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/msr-tr-2010-123.pdf)). On SCTBench, delay bounding beat preemption bounding, but random was close (section 2).

**Maps to hydo.** Our sweeps (`--sweep kill|cut|net|killcut|pause|clock|oneway|frontkill`, and `tail` in progress) are bounded-exhaustive search over *fault* placements with bound 1 or 2. What is missing is a single sweep that covers the whole depth-1 space (every kind × service × position × short/long) as one unit, and a coarse depth-2 space.

**Effect.** A complete depth-1 sweep contains d7's trigger by construction. See recommendation 1.

---

## 4. Crash-consistency testing (ALICE, CrashMonkey/ACE)

**Idea.** Enumerate the crash points that matter (after each persistence call, or each fsync), exhaustively, inside a small bounded workload.

**Evidence.** ALICE found 60 vulnerabilities in 11 applications ([OSDI'14](https://usenix.org/node/186195)). CrashMonkey+ACE, with workloads of at most 3 core operations, reproduced "24 out of the 26 crash-consistency bugs reported in the last five years" and found 10 new ones ([OSDI'18](https://usenix.org/conference/osdi18/presentation/mohan)).

**Maps to hydo.** Our power cut `C=<order>` is placed at an order boundary, and the lying disk drops writes by request index. We do not enumerate cuts *at persistence events* (each FLUSH, each fsync of each store). hvx already sees every virtio-blk FLUSH and every request, so "cut after the k-th FLUSH" is cheap to add, and the space is small (tens to hundreds of FLUSHes per run).

**Effect.** The benchmark's durability defects (d2, d5) are already found within a few runs, so little change there. But this is the systematic way to find the class of bug the M26/M27/M34 flagships showed, and it would also find "cut right after the last write" cases like d7's cousins in storage code.

**Cost.** Low to medium: a host token `cutf=<k>` in `hvx_explore.c` (cut at the k-th FLUSH of the workload), and a sweep over k.

---

## 5. Jepsen's nemesis: faults as a recurring process over time

**Idea.** Jepsen does not place one fault per test. A nemesis generator emits fault start/stop operations throughout the test, staggered around an interval, then a **final generator** heals everything before the final checks.

**Evidence.** In `jepsen.nemesis.combined`, `default-interval` is `10` seconds "between nemesis operations", each package's generator is `(gen/flip-flop start (gen/repeat stop))` wrapped in `gen/stagger`, packages "are mixed together randomly", and a `:final-generator` is used "to clean up any failure modes at the end of a test" ([source](https://raw.githubusercontent.com/jepsen-io/jepsen/main/jepsen/src/jepsen/nemesis/combined.clj)). The package is for "throwing a broad array of simple failures at the wall" ([docs](https://cljdoc.org/d/jepsen/jepsen/0.1.16/api/jepsen.nemesis.combined)).

**Maps to hydo.** This is the clearest difference from our generator. Ours draws **one** window per fault kind, uniformly placed. A recurring fault process alternates fault on/off for the whole workload, so *every* phase, including the last orders before the heal, has a fault with high probability in every run. That is exactly what d7 needs.

**Effect on d7.** Under a flip-flop process with mean on/off of a few orders, the chance that a Kafka-affecting fault is active during the last 2–3 orders is roughly the duty cycle times the share of runs where Kafka is the target, i.e. tens of percent per run instead of about 0.1%. **Prediction: d7 in 3/3 repetitions within 20 runs.** It costs some depth elsewhere (swarm's lesson: more faults per run can hide bugs that need a quiet system), so it should be *one swarm feature*, not the default.

**Cost.** Low: a new guest token, for example `F=svc:kind:on/off-pattern-seed`, applied by `stack-explore.sh` / `hydo-init.sh` each step; generator and shrinker support in `explore.py`.

---

## 6. FoundationDB BUGGIFY: code-level fault points

**Idea.** Put named fault points in the code itself (`BUGGIFY`). Each run activates each site with probability 0.25, and an active site fires on each hit with probability 0.25, from a seeded generator per site ([Go port docs](https://pkg.go.dev/fdb.dev/pkg/dst): "An inactive site never fires for the rest of the run"). It is swarm testing at the level of code sites, and it reaches error paths that external faults rarely hit.

**Evidence.** The mechanism is described by third-party ports and talks; FoundationDB's own numbers were not found. Yuan's study (92% of catastrophic failures from mishandled errors) is the indirect evidence for why this pays off.

**Maps to hydo.** We have external faults only. hydo's assertion SDK (`hydo/stack/hydoassert`) is the natural place for `hydo.Buggify("site")`: seeded from the schedule, reported on the console, so a run stays a pure function of its schedule string and is shrinkable (drop or pin sites). Typical sites in the front door: "Kafka produce returns a timeout after sending", "MySQL commit returns an error after committing", "sleep before the ACK".

**Effect on the benchmark.** d3, d4, d6 and d7 are all error-handling bugs around a Kafka send of unknown outcome. A site "produce times out after the record was sent", fired per call with a few percent, hits the **last** order in a sizeable fraction of runs (each order has an independent chance), so d7 no longer needs a lucky placement. **Prediction: d7 found in 3/3 repetitions within ~30 runs; d3/d4/d6 median under 10 runs.** Caveat: it needs source access, so it only applies to code we instrument (the front door, or user projects using the SDK), not to unmodified MySQL or Kafka.

**Cost.** Medium: SDK function in Go (and shell for manifests), a host token carrying the BUGGIFY seed and probabilities, report lines, shrinker support.

---

## 7. Feedback-driven fault selection (Mallory, CrashFuzz, FaultFuzz, Anduril, LDFI)

**Idea.** Choose the next faults from what earlier runs did, using code coverage, I/O events, causal traces or happens-before summaries.

**Evidence.**
- Mallory, CCS'23 ([arXiv](https://arxiv.org/pdf/2305.02601)): it "dynamically constructs Lamport timelines", abstracts them into "happens-before summaries", and uses Q-learning to pick faults. It found "22 zero-day bugs (of which 18 were confirmed by developers)". The 54% more states and 1.87× faster bug finding than Jepsen come from a summary of the paper; the abstract only says it "explores more behaviours and takes less time to find bugs".
- CrashFuzz, ICSE'23 ([page](https://conf.researchr.org/details/icse-2023/icse-2023-technical-track/173/Coverage-Guided-Fault-Injection-for-Cloud-Systems)): mutates crash/reboot combinations by coverage feedback; 4 new and 1 known bug in ZooKeeper, HDFS, HBase. FaultFuzz (ICSE'24 demo) is similar ([page](https://conf.researchr.org/details/icse-2024/icse-2024-demonstrations/15/FaultFuzz-A-Coverage-Guided-Fault-Injection-Tool-for-Distributed-Systems)).
- Anduril, SOSP'24 (abstract only; [paper](https://web.eecs.umich.edu/~ryanph/paper/anduril-sosp24.pdf) could not be fetched): static causal analysis plus feedback to find "the root-cause fault and timing", for reproducing 22 known failures.
- LDFI / Molly, SIGMOD'15 ([summary](https://blog.acolyer.org/2015/03/26/lineage-driven-fault-injection/)): reasons backwards from a correct outcome's lineage to the faults that could break it; at Netflix, a press report quotes "about 200 experiments" instead of brute force ([InfoQ](https://www.infoq.com/news/2016/03/failure-testing-netflix)), not verified against the paper.
- Rose, EuroSys'26 ([paper](https://www.cs.purdue.edu/homes/pfonseca/papers/eurosys26-rose.pdf)): for *reproduction*, it injects at specific offsets inside functions, prioritising "call sites to system calls"; where "Jepsen's random fault injection achieves only a 1% replay rate", Rose reaches 100%.

**Maps to hydo.** Our coverage and assertion guidance (M41, M44, M45) is the same family and did not help on our stack. The successful tools differ in *what* they feed back: Mallory uses event-order summaries (our `HYDO-STATE` tuples are the closest), Anduril and Rose use *where in the code* the fault lands, and LDFI uses *which events supported a good outcome*. LDFI's angle is directly relevant to d7: for each acknowledged order, its support is "MySQL commit + Kafka produce + ACK"; LDFI would target the support of every order, including the last, instead of a random one.

**Effect.** Uncertain on our benchmark; our own negative results suggest the signal matters more than the learner. The LDFI view ("for each outcome, fault its support") is cheap to approximate: target faults at each order index in turn, which is what a complete depth-1 sweep does.

**Cost.** High for Mallory-style learning; low for the LDFI approximation (it reduces to recommendation 1).

---

## 8. Swarm testing and how configurations are chosen

**Evidence.** Groce et al., ISSTA'12: swarm "found 42% more distinct ways to crash a collection of C compilers" than the tuned default, with "each feature … a 50% chance of being included" ([Regehr's summary](https://blog.regehr.org/archives/591), [paper](https://www.cefns.nau.edu/~adg326/issta12.pdf)). The mechanism: some features suppress bugs, and features compete for space in a fixed-size test.

**Maps to hydo.** We have it (M42), and it is our most reliable strategy (M54). Two refinements follow from the paper's mechanism: (a) **position** should be a swarm feature too ("this run's faults go late", "early", "anywhere"), because uniform position is the hidden default that hides d7; (b) a recurring-fault process (section 5) should be one swarm feature, so runs with a quiet system still exist.

---

## 9. Adaptive random testing (not recommended)

Arcuri & Briand, ISSTA'11, 3,727 mutated programs: ART is "highly inefficient even on trivial problems" once distance computation is counted ([abstract](https://pure.ul.ie/en/publications/adaptive-random-testing-an-illusion-of-effectiveness/)). A 2024 paper proposes a cheaper variant ([arXiv](https://arxiv.org/pdf/2410.17907)). Our schedules are cheap to generate and expensive to run, so spreading them out could help slightly, but swarm already gives diversity; low priority.

---

## 10. Antithesis (public material)

Their docs describe random inputs, a wide range of faults, and "an intelligent guidance component" using reinforcement learning that "drives the platform to seek out new and interesting states", with branching timelines ([how it works](https://antithesis.com/docs/introduction/how_antithesis_works/)). The guidance metric is not published. Two published mechanisms map directly to us:
- **Quiet periods**: a workload can call `ANTITHESIS_STOP_FAULTS` for N seconds; faults stop and resume ([pausing faults](https://antithesis.com/docs/environment/fault_injection/pause_faults/)). That allows liveness checks mid-run.
- **Test Composer phases**: `eventually_` commands kill other commands and "stop all fault injection" before checking recovery; `finally_` commands run only when everything completed; `anytime_` commands check invariants during the run ([reference](https://antithesis.com/docs/test_templates/test_composer_reference/)).

We have the final heal-then-check (`final:` invariants, the oracle) and periodic invariants (`every:`), but not mid-run quiet periods with a liveness check. That matters less for our benchmark than for real services with leases.

---

## Ranked recommendations

Each prediction is falsifiable on `hydo/bench` (8 defects, 3 repetitions × 60 runs, early stop, `run.py`).

1. **A complete depth-1 sweep, `--sweep single` (bounded-exhaustive, sections 1, 3, 4, 7).** Every fault kind (kill, pause `+n`, timed pause, one-way req/resp, two-way loss, cut, lying disk at a fixed seed) × every service × every order index × two durations, durable settings. About 4–5 kinds × 4 services × 30 positions × 2 ≈ 1,000 runs, about 2 hours on the box. Extends the `tail` sweep (branch `tail-sweep`) from the last orders to all orders. **Prediction:** finds all 8 defects, d7 included, because each defect's hand-written trigger is a depth-1 schedule (`hydo/bench/README.md`). Also gives the per-defect *trigger set*, which tells us how rare each defect is under random placement. Cost: low.

2. **Position as a swarm feature, plus a recurring-fault process (sections 5, 8).** Swarm draws a position profile per run (uniform / early / late / last-3) and, as one more feature, a Jepsen-style on/off fault process over the whole workload. **Prediction:** swarm finds d7 in 3/3 repetitions within 20 runs, and detections of d0–d6 do not get slower by more than 2× median. Cost: low.

3. **BUGGIFY sites in the SDK and front door (section 6).** `hydo.Buggify("site")` with per-run activation 0.25 and per-hit firing p, seeded by a host token; sites around every store call (error after success, timeout after send, delay before ACK). **Prediction:** d7 in 3/3 within 30 runs; d3, d4, d6 median under 10 runs under random. Cost: medium. Only for code we or the user instrument.

4. **Crash points at persistence events (section 4).** Host token `cutf=<k>`: cut after the k-th FLUSH of the workload; sweep k. **Prediction:** on d2 and d5, finds the defect at the first FLUSH that follows the faulty write, in a sweep of under 200 points; on the real MySQL/Redis/Kafka images, enumerates every distinct durable state the services can leave behind. Cost: low (hvx already counts FLUSHes).

5. **Access-directed preemption for narrow races (section 2).** Count PCT change points over shared-memory events instead of all branches: on the Mac, exact hardware watchpoints (M51 research: ~350k hits/s) on the pages a workload shares; on Intel, EPT write-protection of selected pages. Place d−1 = 1–2 extra ticks uniformly over those *accesses*. With k shrinking from ~10⁹ branches to ~10⁴–10⁵ accesses, PCT's per-run bound for depth 2 rises from ~10⁻⁹ to ~10⁻⁵–10⁻⁴. **Prediction:** the M43 calibration race at window 0.004% is hit in over 50% of 200 seeds (now 25% at best, M49); the SQLite WAL-reset bug becomes findable within a few thousand runs if its two racing accesses are on a watched page. Cost: high (choosing pages, overhead); do it after 1–4.

Not recommended now: Q-learning or bandit fault selection (Mallory-style), and adaptive random testing. Our own results (M41, M44, M45, M54) and the SCTBench result that random is a strong baseline both say the bottleneck is *where faults and preemptions can land*, not how cleverly the next schedule is picked.

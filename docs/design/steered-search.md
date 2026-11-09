# Steered search: exploring by how close a run came to failing (Milestone 209)

Status: built, opt-in (`hydo explore --steer`). The plain exploration is unchanged. On the
Qdrant test it did not help ([The answer](#the-answer)); the numbers are in
[Measurements](#measurements).

## The problem

The Qdrant flaky-test example (`hydo/onboard/examples/qdrant-flaky-10200/RESULTS.md`) runs one
Rust test. The test fails when a background worker drains a queue of 35 operations before the
test reads the queue's length. The test prints the length it reads:

```text
Post-load update queue info: ShardUpdateQueueInfo { length: 18, op_num: Some(481), deferred_points: Some(0) }
```

On the KVM engine with tick jitter and forced preemption points, 4 of 400 random schedules
failed. The other 396 runs read lengths from 2 to 36, and 116 of them read 18. The failing runs
read 0. So each run says how close it came to failing, and the random exploration throws that
away.

A steered search keeps the schedules whose runs came closest to failing and spends part of its
budget on small changes to them. The number comes from the system under test. hydo does not
guess it.

## How a project reports the number

A run's value is one number: the best of all the numbers the run reports. "Best" is the smallest
for the goal `min` and the largest for the goal `max`. There are three ways to report it.

**The Rust SDK.** `hydo_assert::minimize(id, msg)` and `hydo_assert::maximize(id, msg)` declare a
value. Its `value(v)` call reports a number:

```rust
let queue = hydo_assert::minimize("queue.len", "the test fails when the queue is empty");
queue.value(info.length as i64);
```

The SDK writes a console record the first time, and then only when the number is better than
every earlier number of that id in the process:

```text
HYDO-STEER {"id":"queue.len","goal":"min","v":18,"msg":"the test fails when the queue is empty"}
```

One id writes at most 64 records in one process, so a counter that improves at every report
cannot fill the console. The record has its own tag. The assertion readers and the property
table never see it. The record goes where the SDK's assertion records go (`HYDO_ASSERT_OUT`).

**A pattern in the manifest.** A program without the SDK often prints the number already, as
the Qdrant test does. A manifest's `steer:` entry names it:

```yaml
steer:
  - id: queue_len
    match: 'ShardUpdateQueueInfo \{ length: (\d+)'
    goal: min
```

The explorer applies `match` to every line of the run's consoles and to the verdict line. The
first capture group is the number. Without a group, the whole match is the number. `steer:` is a
format-1 addition: hydo-build checks the regular expression, and an image of a manifest without
it has the same bytes as before.

**A pattern on the command line.** `hydo explore --steer-match REGEX` does the same for an image
whose manifest has no entry. The measurements below use it, so the recorded Qdrant images stay
as they are.

`--steer <id>` takes the manifest's entry of that id when there is one, else the SDK's records
of that id. `--steer-goal min|max` overrides the goal. Without it, the entry's goal counts, then
the goal of the first SDK record, then `min`.

**The other SDKs.** The Rust SDK is the reference (`sdk/README.md`). Its shared cases
`sdk/cases/steer/` give the exact bytes. The Go, Python, and JavaScript case readers know the
need `steer` and skip those cases until their SDK has the calls. Those SDKs are not changed yet,
because the Python and JavaScript files are inside recorded guest images (open item 78).

## How the explorer uses the number

The search runs in rounds of `--steer-batch` schedules (default 4). Each schedule of a round is
one of two kinds:

- A random schedule is the strategy's schedule of the next seed of `--seeds`, exactly as the
  plain exploration draws it.
- A mutant is one change to an entry of the corpus.

Once the corpus has an entry, a schedule is a mutant with probability `--steer-mix`/1000
(default 500). So at least half of the budget goes to random schedules in their usual order.
With `--steer-mix 0` the search runs the plain exploration's schedules, seed by seed, with the
search's records added.

**The corpus.** The corpus keeps the `--steer-corpus` runs with the best values (default 16),
best first. Ties keep the earlier run first. A parent is chosen with the weight
1 / ((rank + 1) × (picks + 1)). The best entries are chosen most often. An entry that was chosen
often gives way to the others, as fuzzers spread their effort over a corpus.

**A mutant is one change** to the parent's schedule:

- a step or a count moved, in 4 tries of 10. A fault's step and `pctn=` move by 1 to 3. A length
  such as `pctk=`, `tjmax=` or `q=` moves by about a tenth, or is doubled or halved;
- a seed drawn again: `dseed=`, `tj=`, `pct=`, `xt=`, `qs=` (2 in 10);
- a part removed: a fault token, the disk fault, the knobs, a timing token (1.5 in 10);
- a part taken from the strategy's schedule of another seed (2.5 in 10).

The mutant must read back as a schedule, and it must not be a schedule the search already ran.
Otherwise the search tries again, 16 times, and then takes the next random schedule. Every
change is recorded in words, for example `pctn=39 -> pctn=40`.

**The end.** The search runs the number of schedules that `--seeds` names. With `--steer-stop`
it stops after the round of the first failure. Every first run then goes through the usual flow:
the re-runs that check the fingerprint, the shrinks, and `report.md`.

## What the results record

The results format gains two additions:

- `Run.steer` on every first run: the run's `value`, its `origin` (`random` with `drawn_seed`,
  or `mutant` with `parent` and `change`), and `best` when the value beat every earlier run;
- `Exploration.steer`: the source, the pattern, the goal, and the options. It also has the
  counts of random schedules, mutants, and runs with a value, and the best value. Then come the
  first failing run, the progress of the best value, and the final corpus.

`report.md` gets a section "Steered search" with the progress table. The printed summary line
gains `steer`, `steer_mutants`, `steer_best` and `steer_first_failure`. An exploration without
`--steer` writes none of them.

## Determinism

A run's value is a function of its consoles, and the consoles are a function of the schedule.
The search's choices come from one stream seeded from the first seed of `--seeds`. The corpus
changes only between rounds, and each round's results are read in tag order. So a steered search
is a pure function of its options and the image. The number of workers and the order in which
runs finish do not change it. A steered schedule is an ordinary schedule: `hydo run` and `hydo repro` run it
without the search.

## Related work

The idea comes from fuzzing research:

- IJON (Aschermann, Schumilo, Abbasi, and Holz, IEEE Symposium on Security and Privacy 2020)
  lets a developer annotate a program with `IJON_MAX` and `IJON_MIN`. The fuzzer keeps an input
  for each annotation slot that reaches a new best value. The paper reports that this solves a
  maze and levels of Super Mario Bros., which coverage feedback alone does not.
- FuzzFactory (Padhye, Lemieux, Sen, Simon, and Vijayakumar, OOPSLA 2019) generalises this. A
  domain-specific feedback, which the paper calls waypoints, decides which inputs to keep, beside
  coverage.
- AFL (Zalewski) keeps a corpus, mutates its entries and favours some of them; AFLFast (Böhme,
  Pham and Roychoudhury, CCS 2016) assigns the effort per entry with a power schedule.

hydo's search mutates fault schedules, not program inputs, and has one value instead of one per
code location.

## Why this is not hydo's coverage guidance again

hydo has tried guidance three times, and each time it found nothing the random fault process did
not find:

- Milestone 41 kept a schedule when it reached a new control-flow edge (Intel Processor Trace).
  The edges saturated in a few runs and were mostly start-up code that does not depend on the
  faults. The bug was about timing, and its code was covered at once.
- Milestone 44 used whole-system coverage and a state signal. Coverage grew alike with and
  without guidance, and the guided corpus filled with variants that differed only in hit counts.
- Milestone 45 branched at the points where assertions fired. Variants of one schedule repeated
  each other, so the budget covered fewer situations than fresh schedules did.

Coverage says which code ran. It does not say how close a run came to the failure. The steered
value does, when it is chosen well: the developer names a number that moves towards the failure,
as the Qdrant queue length moves towards 0. The value can still mislead. A failure can sit next
to runs whose values are far from it, or every run can give the same value. Two checks show
when that is so:

- the value's spread and its correlation with failing: `events.jsonl` has every run's value and
  verdict;
- the same seeds without mutants (`--steer-mix 0`): its random schedules are the steered search's
  random schedules, in the same order, so the two runs to the first failure compare seed by seed.

Because half of the schedules stay random, a useless value costs at most about twice the runs of
the plain exploration to the same failure.

## How to measure the benefit

Measure runs to the first failure, not failures per run, because a steered search keeps
returning to a failure it found. For each of at least five seed ranges, run the plain exploration
and the steered search with `--steer-stop` and the same options. Report the median and the
spread of both. A range that ends without a failure counts as its budget, and the report says
how many did.

## Measurements

The measurements ran on the Qdrant example's recorded buggy image (`9aa2213a…`) with
`--steer-match 'length: (\d+)' --steer-stop`. The plain exploration ran with `--steer-mix 0`.

**KVM** (the build machine, `--backend kvm --timing --preempt --workers 1`, a budget of 400):

| seeds | plain | steered | the steered search's failing schedule |
|---|---|---|---|
| 1–400 | 37 | 78 | the random schedule of seed 37 |
| 1001–1400 | 145 | 118 | a mutant: `tj=` drawn again |
| 2001–2400 | 22 | 47 | the random schedule of seed 2022 |
| 3001–3400 | 152 | 221 | a mutant: `tjmax=1000 -> tjmax=900` |
| 4001–4400 | 72 | 133 | the random schedule of seed 4072 |

The numbers are the runs to the first failure. The median is 72 for the plain exploration (22 to
152) and 118 for the steered search (47 to 221). The steered search was earlier in 1 of 5 ranges.
In 3 ranges it failed on the plain exploration's failing schedule, about twice as late, because
half of its schedules were mutants.

The mutants moved the value but not the verdict. Of the 288 mutants, 7.6 % read a length of 5
or less, against 3.7 % of the 436 plain runs. But 2 mutants failed (0.7 %), against 5 plain runs
(1.1 %). The failing run's recorded neighbour, `pctn=39` instead of `pctn=40`, reads 22. So the
failure does not sit at the end of a slope of small lengths. In these runs, a mutant of a
schedule with a small length failed less often than a new random schedule.

**hvx** (the bare-metal machine, one vCPU, `--timing --preempt --workers 3`, a budget of 200).
On one vCPU the update worker takes at most one operation in the window, so the length
reads 34 to 36. The plain explorations of seeds 1–200, 1001–1200, and 2001–2200 read 34 in 18,
23, and 18 runs. The steered searches of the same seeds read 34 in 40, 41, and 31 runs. No run
went below 34, and none of the six explorations failed.

## The answer

On the Qdrant test, steering did not help. On KVM it found the failure later, within the factor
of two that its half of random schedules bounds. On hvx the value hardly moves, and no
exploration failed. The queue length measures the state that fails. But the explorer's changes
are not small steps in that state. One forced preemption point or one jitter seed changes how
the whole window plays out. A value helps only when the schedules that come closer also make the
failure more likely; `events.jsonl` shows whether they do. The bug benchmark's d11 (an
acknowledgement on an unflushed flag) was not measured: its console has no such number.

## Left for later

- The Go, Python, and JavaScript SDKs (open item 78).
- Several values at once, or a value per code location, as IJON's slots.
- A distance computed by hydo itself, for example the gap between a fault's step and the step
  where a service logs an event.

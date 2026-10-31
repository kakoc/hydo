# Finer preemption for narrow races (milestone 49)

Two levers, both opt-in and inert when absent (side module `/dev/hvxpre`: `side-metal.sh`
ALL PASS 25 checks; pinned reproducers `2403499c9a3f8419` / `f87130453609a5d6`):

- **Shorter tick period** (`pp=<branches>` in hvx_explore schedules, `HVX_PMU_PERIOD` here).
  The virtual TSC still advances by the default tick (2 ms) per tick unless `pptsc=` says
  otherwise. Chosen on purpose: scaling the tick TSC with the period keeps the timer rate per
  unit of work, so the number of thread switches would not change; leaving it makes guest time
  run faster per unit of work (a slower machine), and the timer -- with a possible switch --
  comes once per tick.
- **Inserted ticks** (`HVX_SET_XTICK`; `xt=`/`xtr=`/`xtpm=` or `xtx=` in schedules,
  `HVX_XT_*` here): extra ticks between the grid ticks at seeded points, or at an explicit
  list of guest branch counts.

## Calibration race (hydorace, OUT=50000, SEATS=5000; 200 seeds per cell, every hit replayed)

| window W (fraction) | default | jitter (M43) | pp=2^16 + jitter | inserted, 16 slots x 50% |
|---|---|---|---|---|
| 50 (0.1%)   | 0/1 | 42/200 (21%) | 194/200 (97%) | 167/200 (84%) |
| 10 (0.02%)  | 0/1 | 8/200 (4%)   | 117/200 (59%) | 66/200 (33%)  |
| 2 (0.004%)  | 0/1 | 4/200 (2%)   | 49/200 (25%)  | 20/200 (10%)  |

All 664 hits replayed with identical full-state fingerprints. Cost per boot (median, whole
boot + workload): default and jitter 1.5 s (~413 ticks), pp=2^16 6.2 s (~6650 ticks, 4.1x),
inserted 4.0 s (~3720 ticks, 2.6x). Files: `pre_race1.sh` (one boot), `pre_campaign.sh`,
`pre_table.py` (removed in M108: `git show 705cf98:hv/hvx/race/pre_table.py`); results on the box in `/var/tmp/m49-preempt/camp*.txt`.

## SQLite 3.51.2 WAL-reset corruption (hydo/knownbugs/sqlite, hunt.py arms pp and xt; hunt.py was removed in M108)

600 blind runs on `explore-ob-kb-sqlite-3510200-disk.img` through `/dev/hvxpre`, workload seed
varied per run: **no violation**, so no minimal schedule and nothing to check on 3.51.3.

| arm | runs | median s | median ticks / extra ticks | median involuntary switches |
|---|---|---|---|---|
| pp=2^14 + jitter | 131 | 6.6 | 3753 ticks | 2707 |
| pp=2^12 + jitter | 169 | 14.5 | 13603 ticks | 6347 |
| inserted, 8 slots | 157 | 3.7 | 218 extra | 748 |
| inserted, 32 slots | 143 | 4.2 | 842 extra | 1094 |

For comparison the M48 hunt (default period, jitter) had a median 58 workload ticks, 711
switches, 4.0 s. Faster guest time probably also shortens the work the workload does (committed transactions per run vary widely; not checked), which
caps how much the switch count grows. Results: `/var/tmp/m49-preempt/sq-hunt` on the box.

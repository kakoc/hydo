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
`pre_table.py`; results on the box in `/var/tmp/m49-preempt/camp*.txt`.

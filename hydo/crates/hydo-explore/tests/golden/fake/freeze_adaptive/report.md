# Explorer report

| | |
|---|---|
| schedules | 6 |
| strategy | sweep:freeze |
| image | scripted-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 5.6 |
| worker_boot_s | 2.5 |
| total_wall_s | N |
| failures | 0 |
| reproduced | 0 |
| repro_runs | 2 |
| stage1_points | 6 |
| stage2_points | 0 |
| trace_runs | 8 |
| stage1_failures | 0 |
| stage2_failures | 0 |
| stage1_wall_s | N |
| stage2_wall_s | N |
| fc_window | 4 |
| fc_links | svc |
| fc_centres | fail |
| error_kinds_new | 1 |
| runs_with_new_error_kinds | 6 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|

## Minimal reproducers

## Property table over every schedule (hydo guide sdks)

```
type                   id                                    ok    fail  verdict                  first failure / description
eventually             orders.drained                         6       0  PASS                       [slowest in time: 3.20 s]  [the project's own check]
sometimes              retry.taken                            0       6  NOT EXERCISED            -  [the project's own check]
liveness: 1 eventually properties, 0 missed a deadline
runs=6 (setup-complete seen in 0); properties=2; violations=0
```

## Liveness (eventually properties, hydo guide sdks)

| property | held in time | missed deadline | slowest in time | deadline | slowest held late | first miss |
|---|---|---|---|---|---|---|
| `orders.drained` | 6 | 0 | 3.20 s (seed-1) | - | - | - |

## Error logs (errors: in the manifest)

Known list: none; unknown kinds: report. 1 new kind, 0 only known.

### New kinds

| signature | service | runs | first seed | at (first run) | example |
|---|---|---|---|---|---|
| `4c53c64297e2` | app | 6 | 1 | setup | `config reloaded` |

#### `4c53c64297e2` (app): `config reloaded`

- example: `config reloaded`
- first schedule (seed 1): `disk=none dseed=0 | run F=redis<workload@7#1:150000`
- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first
- expected? `hydo errors accept 4c53c64297e2 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)


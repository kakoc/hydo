# Explorer report

| | |
|---|---|
| schedules | 228 |
| strategy | sweep:protocol |
| image | explore-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 5.2 |
| worker_boot_s | 2.5 |
| total_wall_s | N |
| failures | 36 |
| reproduced | 36 |
| error_kinds_new | 1 |
| runs_with_new_error_kinds | 228 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| durable-defaults | `VIOLATION charged twice` | 36 | 37 38 39 40 41 42 43 44 | 36/36 |

## Minimal reproducers

### durable-defaults: `VIOLATION charged twice`

- from seed 37, 1 shrink runs
- minimal schedule: `disk=none dseed=0 | run H=payments.charge@0-1:drop`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION charged twice order=0`
- fingerprint, two runs: `41947dc896f68534` / `41947dc896f68534` (identical)
- by hand: `hydo run --image <dir>/explore-disk.img "disk=none dseed=0 | run H=payments.charge@0-1:drop"`

## Property table over every schedule (hvx/ASSERTIONS.md)

```
type                   id                                    ok    fail  verdict                  first failure / description
eventually             orders.drained                       228       0  PASS                       [slowest in time: 3.49 s]  [not in catalog]
sometimes              retry.taken                            0     228  NOT EXERCISED            -  [not in catalog]
liveness: 1 eventually properties, 0 missed a deadline
runs=228 (setup-complete seen in 0); properties=2; violations=0
```

## Liveness (eventually properties, hvx/ASSERTIONS.md)

| property | held in time | missed deadline | slowest in time | first miss |
|---|---|---|---|---|
| `orders.drained` | 228 | 0 | 3.49 s (seed-177) | - |

## Error logs (errors: in the manifest)

Known list: none; unknown kinds: report. 1 new kind, 0 only known.

### New kinds

| signature | service | runs | first seed | at (first run) | example |
|---|---|---|---|---|---|
| `4c53c64297e2` | app | 228 | 1 | setup | `config reloaded` |

#### `4c53c64297e2` (app): `config reloaded`

- example: `config reloaded`
- first schedule (seed 1): `disk=none dseed=0 | run H=payments.charge@0-1:status=500`
- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first
- expected? `hydo errors accept 4c53c64297e2 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)


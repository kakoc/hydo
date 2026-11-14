# Explorer report

| | |
|---|---|
| schedules | 40 |
| strategy | swarm |
| image | scripted-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 4.5 |
| worker_boot_s | 2.5 |
| total_wall_s | N |
| failures | 9 |
| reproduced | 9 |
| error_kinds_new | 2 |
| runs_with_new_error_kinds | 40 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| io-errors | `ERROR harness: guest did not finish (I/O error)` | 1 | 13 | 1/1 |
| lying-disk | `VIOLATION lost acknowledged write` | 6 | 1 14 18 21 26 33 | 6/6 |
| unsafe-knob | `VIOLATION lost acknowledged write` | 2 | 5 38 | 2/2 |

## Minimal reproducers

### lying-disk: `VIOLATION lost acknowledged write`

- from seed 1, 7 shrink runs
- minimal schedule: `disk=lying:1 dseed=eac0b527c5ac67d9 | run C=20`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION lost acknowledged write order=20`
- fingerprint, two runs: `376816de033c523e` / `376816de033c523e` (identical)
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=lying:1 dseed=eac0b527c5ac67d9 | run C=20"`

### unsafe-knob: `VIOLATION lost acknowledged write`

- from seed 5, 2 shrink runs
- minimal schedule: `disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION lost acknowledged write order=9`
- fingerprint, two runs: `813fb0487f0a4769` / `813fb0487f0a4769` (identical)
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no"`

## Property table over every schedule (hydo guide sdks)

```
type                   id                                    ok    fail  verdict                  first failure / description
eventually             orders.drained                        40       0  PASS                       [slowest in time: 3.49 s]  [not in catalog]
sometimes              retry.taken                           19      21  PASS                     -  [not in catalog]
liveness: 1 eventually properties, 0 missed a deadline
runs=40 (setup-complete seen in 0); properties=2; violations=0
```

## Liveness (eventually properties, hydo guide sdks)

| property | held in time | missed deadline | slowest in time | deadline | slowest held late | first miss |
|---|---|---|---|---|---|---|
| `orders.drained` | 40 | 0 | 3.49 s (seed-39) | - | - | - |

## Error logs (errors: in the manifest)

Known list: none; unknown kinds: report. 2 new kinds, 0 only known.

### New kinds

| signature | service | runs | first seed | at (first run) | example |
|---|---|---|---|---|---|
| `07df7023b937` | redis | 19 | 1 | 12 | `connection to redis lost 3` |
| `4c53c64297e2` | app | 40 | 1 | setup | `config reloaded` |

#### `07df7023b937` (redis): `connection to redis lost <n>`

- example: `connection to redis lost 3`
- first schedule (seed 1): `disk=lying:3 dseed=eac0b527c5ac67d9 | run K=redis@12+2 C=20`
- minimal schedule (10 shrink runs): `disk=none dseed=eac0b527c5ac67d9 | run K=redis@12+1`
- run twice: fingerprints `6b450cd54a2e0485` / `6b450cd54a2e0485` (identical), the kind in both runs
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=none dseed=eac0b527c5ac67d9 | run K=redis@12+1"`
- expected? `hydo errors accept 07df7023b937 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)

#### `4c53c64297e2` (app): `config reloaded`

- example: `config reloaded`
- first schedule (seed 1): `disk=lying:3 dseed=eac0b527c5ac67d9 | run K=redis@12+2 C=20`
- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first
- expected? `hydo errors accept 4c53c64297e2 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)


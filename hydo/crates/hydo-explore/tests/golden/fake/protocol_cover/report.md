# Explorer report

| | |
|---|---|
| schedules | 30 |
| strategy | random |
| image | scripted-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 5.9 |
| worker_boot_s | 2.5 |
| total_wall_s | N |
| failures | 14 |
| reproduced | 14 |
| error_kinds_new | 2 |
| runs_with_new_error_kinds | 30 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| durable-defaults | `VIOLATION charged twice` | 6 | 3 11 15 17 25 27 | 6/6 |
| io-errors | `ERROR harness: guest did not finish (I/O error)` | 4 | 8 9 19 29 | 4/4 |
| lying-disk | `VIOLATION lost acknowledged write` | 4 | 7 12 24 28 | 4/4 |

## Minimal reproducers

### durable-defaults: `VIOLATION charged twice`

- from seed 3, 1 shrink runs
- minimal schedule: `disk=none dseed=1d0b14e4db018fed | run H=payments.charge@1-2:drop`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION charged twice order=1`
- fingerprint, two runs: `6f69e4a55a588db6` / `6f69e4a55a588db6` (identical)
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=none dseed=1d0b14e4db018fed | run H=payments.charge@1-2:drop"`

### lying-disk: `VIOLATION lost acknowledged write`

- from seed 7, 22 shrink runs
- minimal schedule: `disk=lying:1 dseed=63cbe1e459320dd7 | run C=5`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION lost acknowledged write order=5`
- fingerprint, two runs: `817db284189a57a9` / `817db284189a57a9` (identical)
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=lying:1 dseed=63cbe1e459320dd7 | run C=5"`

## Property table over every schedule (hydo guide sdks)

```
type                   id                                    ok    fail  verdict                  first failure / description
eventually             orders.drained                        30       0  PASS                       [slowest in time: 3.39 s]  [the project's own check]
sometimes              retry.taken                           16      14  PASS                     -  [the project's own check]
liveness: 1 eventually properties, 0 missed a deadline
runs=30 (setup-complete seen in 0); properties=2; violations=0
```

## Liveness (eventually properties, hydo guide sdks)

| property | held in time | missed deadline | slowest in time | deadline | slowest held late | first miss |
|---|---|---|---|---|---|---|
| `orders.drained` | 30 | 0 | 3.39 s (seed-24) | - | - | - |

## Error logs (errors: in the manifest)

Known list: none; unknown kinds: report. 2 new kinds, 0 only known.

### New kinds

| signature | service | runs | first seed | at (first run) | example |
|---|---|---|---|---|---|
| `4c53c64297e2` | app | 30 | 1 | setup | `config reloaded` |
| `e4192fb4fa7d` | shop | 16 | 1 | 3 | `connection to shop lost 3` |

#### `4c53c64297e2` (app): `config reloaded`

- example: `config reloaded`
- first schedule (seed 1): `disk=ioerr:1 dseed=910a2dec89025cc1 | run K=shop@3+2 H=payments.charge@0-1:status=500,effect=yes`
- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first
- expected? `hydo errors accept 4c53c64297e2 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)

#### `e4192fb4fa7d` (shop): `connection to shop lost <n>`

- example: `connection to shop lost 3`
- first schedule (seed 1): `disk=ioerr:1 dseed=910a2dec89025cc1 | run K=shop@3+2 H=payments.charge@0-1:status=500,effect=yes`
- minimal schedule (13 shrink runs): `disk=none dseed=910a2dec89025cc1 | run K=shop@3+1`
- run twice: fingerprints `67bc59e2e9f5ebdf` / `67bc59e2e9f5ebdf` (identical), the kind in both runs
- by hand: `hydo run --image <dir>/scripted-disk.img "disk=none dseed=910a2dec89025cc1 | run K=shop@3+1"`
- expected? `hydo errors accept e4192fb4fa7d --reason "..."` (or `--during <fault>` to accept it only while a fault is on)


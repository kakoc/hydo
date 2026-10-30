# Explorer report

| | |
|---|---|
| schedules | 40 |
| strategy | sweep:frontkill |
| image | explore-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 5.2 |
| worker_boot_s | 2.5 |
| total_wall_s | N |
| failures | 20 |
| reproduced | 20 |
| error_kinds_new | 1 |
| runs_with_new_error_kinds | 40 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| durable-defaults | `VIOLATION front lost an order` | 20 | 21 22 23 24 25 26 27 28 | 20/20 |

## Minimal reproducers

### durable-defaults: `VIOLATION front lost an order`

- from seed 21, 25 shrink runs
- minimal schedule: `disk=none dseed=0 | run Q=2 X=front@0~0`
- needs a tick-timing change: no
- verdict: `HYDO-ORACLE: VIOLATION front lost an order order=0`
- fingerprint, two runs: `cad881312f4e46e3` / `cad881312f4e46e3` (identical)
- by hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/explore-disk.img <outdir> "disk=none dseed=0 | run Q=2 X=front@0~0"`

## Property table over every schedule (hvx/ASSERTIONS.md)

```
type                   id                                    ok    fail  verdict                  first failure / description
eventually             orders.drained                        40       0  PASS                       [slowest in time: 3.45 s]  [not in catalog]
sometimes              retry.taken                            0      40  NOT EXERCISED            -  [not in catalog]
liveness: 1 eventually properties, 0 missed a deadline
runs=40 (setup-complete seen in 0); properties=2; violations=0
```

## Liveness (eventually properties, hvx/ASSERTIONS.md)

| property | held in time | missed deadline | slowest in time | first miss |
|---|---|---|---|---|
| `orders.drained` | 40 | 0 | 3.45 s (seed-21) | - |

## Error logs (errors: in the manifest)

Known list: none; unknown kinds: report. 1 new kind, 0 only known.

### New kinds

| signature | service | runs | first seed | at (first run) | example |
|---|---|---|---|---|---|
| `4c53c64297e2` | app | 40 | 1 | setup | `config reloaded` |

#### `4c53c64297e2` (app): `config reloaded`

- example: `config reloaded`
- first schedule (seed 1): `disk=none dseed=0 tj=e875c522f1ae54c9 tjpm=100 tjmax=500000 | run X=front@0~20`
- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first
- expected? `hydo errors accept 4c53c64297e2 --reason "..."` (or `--during <fault>` to accept it only while a fault is on)


# PostgreSQL with the ready-made SQL workloads: results

The proof that hydo's ready-made SQL workloads (Milestone 182, `hydo sql-gen` and the guest
program `hydo-sql`, docs/design/workloads.md) are not tied to libSQL: the voting app's database
(`../compose-voting`: PostgreSQL 15, `postgres:15-alpine` by the same digest, the same user and
password, its data directory on the data disk), driven by all five workloads with nothing written
by hand but the service and a knob. Everything ran on **the build machine with the KVM engine**
(`--backend kvm`), with this tree's `hydo`, `hydo-kvm`, `hydo-build` and `hydo-sql`.

## The project

| file | what it is |
|---|---|
| `hydo/hydo.yaml` | the PostgreSQL service (the voting app's `db`), readiness by `pg_isready`, the knob `S` (`synchronous_commit`, set with `ALTER SYSTEM` and a reload), the composer's `test:` settings |
| `hydo/fetch-images.sh` | the image by digest for the machine's architecture |
| `hydo/workload/`, `hydo/checks/` | the output of `hydo sql-gen --dialect postgres --workloads bank,list-append,register,set,counter --endpoint 'db=postgres://postgres:postgres@10.89.0.15:5432/postgres?sslmode=disable'`: batch and interactive transfers and list-append transactions, register operations, set inserts, counter increments; the bank checks, a SQL check, the list-append history (`strict-serializable`: one primary), the register history (linearizable, model `cas`), the set and counter read-backs |

```sh
sh hydo/fetch-images.sh
hydo build hydo/hydo.yaml
hydo run --backend kvm "disk=none | run"
hydo explore --backend kvm --seeds 1-30 --strategy swarm --new-faults --workers 2
```

## Runs (image `426366da…`, each schedule twice from a fresh boot, the same `fp` and `ctl`)

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none \| run` | OK (117 commands, 109 ok, 8 failed: compare-and-sets whose expected value was not there, definite failures) ; `sql.la` 38 operations no anomaly (strict-serializable), `sql.rg` linearizable | `ed0b5c852dd54ddd` | `cad198b39e0131cf` |
| `disk=cut \| run C=12` | OK | `371855b076db79c5` | `53ae797bc32a8b9c` |
| `disk=cut \| run C=12 S=off` | OK (this cut point lost nothing acknowledged) | `d1958b559b8676b9` | `7e933ff48a9259cc` |
| `disk=none \| run K=db@10+4` | OK (100 ok, 17 failed while PostgreSQL was down) | `2931bc502fbc8bec` | `9a4bcae00377c7b2` |

## Exploration

`hydo explore --backend kvm --seeds 1-30 --strategy swarm --new-faults --workers 2`: 30
schedules in 188 s, 12 failures, all reproduced. The generated checks found PostgreSQL losing
acknowledged data on a disk that drops acknowledged writes (`disk=lying`), as they should:

| class | signature | minimal schedule | fp (twice) |
|---|---|---|---|
| lying-disk | `VIOLATION [sql_set] db: 5 of 17 acknowledged elements missing` | `disk=lying:4 dseed=78c9bb2d421fee74 \| run C=48 S=off P=db@36~500:11595 T=36:-86400 W=06dd08ca,p2,q3,r300` | `e56444fcc7f82e70` |
| lying-disk | `VIOLATION [sql_counter] db: counter 1 < 4 acknowledged increments: an acknowledged increment was lost` | `disk=lying:1 dseed=92505b32924cfd59 \| run C=10 S=off T=4:-150 W=672ff4c4,p1,q3,r150` | `d546f1d6c8549d16` |
| lying-disk | `VIOLATION service cannot recover (committed data unrecoverable) (db)` | `disk=lying:1 dseed=b3c3d806a5d34604 \| run C=25 W=f4973749,p4,q2,r300,oparallel_driver_sql_itransfer+parallel_driver_sql_itxn` | `b9b6a3e73b669f33` |

The other 9 failures are I/O-error disks under which PostgreSQL does not start again (`ERROR db
restart failed`, `ERROR restart after the faults (db)`) and one `ERROR tc setup failed`: faults
the project would turn off or accept, not findings. No schedule with an honest disk failed: no
lost update in the interactive transfers, no list-append anomaly, no non-linearizable register.

## What this shows

- The same generator and program that replaced libSQL's 868-line client drive PostgreSQL's wire
  protocol (SCRAM authentication, `BEGIN ISOLATION LEVEL SERIALIZABLE`, one simple-query message
  per batch transaction, SQLSTATE-based outcomes), with the same files.
- The checks a user would otherwise write in shell (set and counter read-backs against
  `$HYDO_ACKED`, the bank's invariant, a SQL check) caught lost acknowledged writes, and the
  explorer shrank them through the composer's `W=` token like any hand-written command.

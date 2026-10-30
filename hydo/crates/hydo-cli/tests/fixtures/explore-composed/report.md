# Explorer report

| | |
|---|---|
| schedules | 16 |
| strategy | random |
| image | explore-ob-redis-composed-disk.img |
| workers | 2 |
| sweep_wall_s | 126.4 |
| runs_per_hour | 455.6 |
| mean_run_s | 7.0 |
| worker_boot_s | 44.1 |
| total_wall_s | 1331.1 |
| failures | 9 |
| reproduced | 9 |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| durable-defaults | `VIOLATION [total_conserved] balances sum to #, not #` | 4 | 6 10 13 16 | 4/4 |
| io-errors | `ERROR redis restart failed` | 2 | 8 9 | 2/2 |
| io-errors | `ERROR restart after the faults (redis)` | 1 | 1 | 1/1 |
| lying-disk | `VIOLATION [acked_writes_survive] key k:# lost` | 1 | 7 | 1/1 |
| lying-disk | `VIOLATION [total_conserved] balances sum to #, not #` | 1 | 12 | 1/1 |

## Minimal reproducers

### durable-defaults: `VIOLATION [total_conserved] balances sum to #, not #`

- from seed 6, 52 shrink runs
- minimal schedule: `disk=none dseed=bd64a5d9adefe000 | run W=:p2,r200,cserial_driver_rewrite+parallel_driver_transfer,0.0.0.741982988,1.1.1.2145334547,2.2.0.439026114,4.3.1.1907381756,5.4.1.1187301234,12.7.1.931266821,13.8.0.1223352624,18.10.0.1899335559,20.11.1.1861045581`
- needs a tick-timing change: no
- composition (test composer, W=): r0: serial_driver_rewrite | r1: parallel_driver_transfer | r2: serial_driver_rewrite | r3: parallel_driver_transfer | r4: parallel_driver_transfer | r7: parallel_driver_transfer | r8: serial_driver_rewrite | r10: serial_driver_rewrite | r11: parallel_driver_transfer
- stable form (commands by id; `hydo save` writes it): the minimal schedule
- verdict: `HYDO-ORACLE: VIOLATION [total_conserved] balances sum to 503, not 500`
- fingerprint, two runs: `b60a695011cf4087` / `b60a695011cf4087` (identical)
- by hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/explore-ob-redis-composed-disk.img <outdir> "disk=none dseed=bd64a5d9adefe000 | run W=:p2,r200,cserial_driver_rewrite+parallel_driver_transfer,0.0.0.741982988,1.1.1.2145334547,2.2.0.439026114,4.3.1.1907381756,5.4.1.1187301234,12.7.1.931266821,13.8.0.1223352624,18.10.0.1899335559,20.11.1.1861045581"`

### lying-disk: `VIOLATION [acked_writes_survive] key k:# lost`

- from seed 7, 49 shrink runs
- minimal schedule: `disk=lying:11 dseed=63cbe1e459320dd7 | run C=1 W=:p1,r100,cparallel_driver_set,0.0.0.2130986824`
- needs a tick-timing change: no
- composition (test composer, W=): r0: parallel_driver_set
- stable form (commands by id; `hydo save` writes it): the minimal schedule
- verdict: `HYDO-ORACLE: VIOLATION [acked_writes_survive] key k:0 lost`
- fingerprint, two runs: `fd0dff892a691b1b` / `fd0dff892a691b1b` (identical)
- by hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/explore-ob-redis-composed-disk.img <outdir> "disk=lying:11 dseed=63cbe1e459320dd7 | run C=1 W=:p1,r100,cparallel_driver_set,0.0.0.2130986824"`

### durable-defaults (found as lying-disk; the minimal schedule needs only durable-defaults): `VIOLATION [total_conserved] balances sum to #, not #`

- from seed 12, 82 shrink runs
- minimal schedule: `disk=none dseed=943ff9fc99de8f03 | run K=redis@6+1 W=86f364de,p1,q1,r50,n7,x0.3-4,oanytime_read+parallel_driver_set`
- needs a tick-timing change: no
- composition (test composer, W=): r4: serial_driver_rewrite | r5: parallel_driver_transfer | r6: serial_driver_rewrite | r7: parallel_driver_transfer | r9: parallel_driver_transfer | r12: serial_driver_rewrite | r13: parallel_driver_transfer
- stable form (commands by id; `hydo save` writes it): `disk=none dseed=943ff9fc99de8f03 | run K=redis@6+1 W=:p1,r50,cserial_driver_rewrite+parallel_driver_transfer,5.4.0.1933323583,6.5.1.1969323408,7.6.0.1677547070,8.7.1.1565087473,11.9.1.1720564279,15.12.0.818962248,17.13.1.1343998502` (the resolved composition did not fail the same way (HYDO-ORACLE: OK invariants=5 held (compose.commands: started=7 ok=5 failed=2 timeout=0 violation=0 killed=0 skipped=0)); the minimal schedule keeps the seed form)
- verdict: `HYDO-ORACLE: VIOLATION [total_conserved] balances sum to 495, not 500`
- fingerprint, two runs: `ab1c1cc42cf40d18` / `ab1c1cc42cf40d18` (identical)
- by hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/explore-ob-redis-composed-disk.img <outdir> "disk=none dseed=943ff9fc99de8f03 | run K=redis@6+1 W=86f364de,p1,q1,r50,n7,x0.3-4,oanytime_read+parallel_driver_set"`

## Property table over every schedule (hvx/ASSERTIONS.md)

```
type                   id                                    ok    fail  verdict                  first failure / description
reachable              fault.paused                           0       0  NOT EXERCISED            a service was paused (SIGSTOP)
sometimes              fault.pause_mid_request                0       0  NOT EXERCISED            a pause began while a client request was in flight
reachable              fault.clock_back                       0       0  NOT EXERCISED            the realtime clock was stepped backwards
reachable              fault.clock_fwd                        0       0  NOT EXERCISED            the realtime clock was stepped forwards
reachable              fault.oneway_req                       0       0  NOT EXERCISED            a one-way network fault hit the req direction
reachable              fault.oneway_resp                      0       0  NOT EXERCISED            a one-way network fault hit the resp direction
sometimes              compose.anytime_read.succeeded        13       0  PASS                     anytime_read exited 0 at least once
always_or_unreachable  compose.anytime_read.no_violation       0       0  PASS                     anytime_read never reports a violation (exit 100)
sometimes              compose.parallel_driver_set.succeeded      14       0  PASS                     parallel_driver_set exited 0 at least once
always_or_unreachable  compose.parallel_driver_set.no_violation       0       0  PASS                     parallel_driver_set never reports a violation (exit 100)
sometimes              compose.parallel_driver_transfer.succeeded      14       0  PASS                     parallel_driver_transfer exited 0 at least once
always_or_unreachable  compose.parallel_driver_transfer.no_violation       0       0  PASS                     parallel_driver_transfer never reports a violation (exit 100)
sometimes              compose.serial_driver_rewrite.succeeded      13       0  PASS                     serial_driver_rewrite exited 0 at least once
always_or_unreachable  compose.serial_driver_rewrite.no_violation       0       0  PASS                     serial_driver_rewrite never reports a violation (exit 100)
eventually             answers                               13       0  PASS                     Redis answers PING within 15 s after the faults are healed  [slowest in time: 1.11 s]  [not in catalog]
always                 compose.commands                      13       0  PASS                     no composed command reported a violation (exit status 100)  [not in catalog]
always                 eventually_serves                     13       0  PASS                     after the faults stop, eventually_serves succeeds  [not in catalog]
always                 finally_summary                       13       0  PASS                     at the end, finally_summary succeeds (skipped unless every started command completed)  [not in catalog]
always                 acked_writes_survive                  12       1  FAIL                     {"out":"key k:0 lost"}  [not in catalog]
always                 total_conserved                        8       5  FAIL                     {"out":"balances sum to 499, not 500"}  [not in catalog]
liveness: 1 eventually properties, 0 missed a deadline
runs=16 (setup-complete seen in 0); properties=20; violations=2
```

## Liveness (eventually properties, hvx/ASSERTIONS.md)

| property | held in time | missed deadline | slowest in time | first miss |
|---|---|---|---|---|
| `answers` | 13 | 0 | 1.11 s (seed-12) | - |


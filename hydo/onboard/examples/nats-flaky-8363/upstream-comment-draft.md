This flake can be made deterministic, which may help with reviewing #8794: with one P the test fails on every second iteration.

**Repro**

```
cd server/ats
GOMAXPROCS=1 go test -run '^TestRegisterAndUnregister$' -count=2 .
```

On main (59bf235a, go1.26.10) this fails 10 of 10 times, always in the second iteration:

```
--- PASS: TestRegisterAndUnregister (0.71s)
    ats_test.go:72: Expected same number of go routines after removing all registered: 3 vs 2
--- FAIL: TestRegisterAndUnregister (0.50s)
```

With #8794 applied (436addad) the same command passes 10 of 10 times.

**Why one P makes it deterministic**

1. The deferred `Unregister()` at the end of iteration 1 sends on `done` while the ticker goroutine is parked in its `select`. The runtime puts that goroutine into the P's `runnext` slot.
2. When the test function returns, `tRunner` wakes the goroutine that waits for it. That one takes `runnext` and pushes the ticker goroutine to the back of the local run queue.
3. `t.Run` for iteration 2 starts the new test goroutine in `runnext` again, and it calls `runtime.NumGoroutine()` before the ticker goroutine has run -> baseline 3, and after its own `Unregister()` and sleep the test counts 2.
4. A failed iteration stops at line 72 before its second `Register()`, so it leaves no goroutine behind and the next iteration passes. That's why `-count=N` with one P fails exactly every second iteration.

To check this I added one `runtime.Gosched()` before the baseline is sampled: the test then passed 5 of 5 times with `GOMAXPROCS=1 -count=2`.

**Rates with more than one P**

With more P's another P can take the ticker goroutine from the queue in time, so it depends on timing. Each row is 2 runs of `-count=100` (200 iterations; at most 100 can fail):

| where | GOMAXPROCS | failed iterations |
|---|---|---|
| Apple M2 Pro, macOS, go1.26.10 | 1 | 100 of 200 |
| same | 2 | 1 of 200 |
| same | 4 | 9 of 200 |
| same | default (12) | 27 of 200 |
| same, `-race` | default | 2 of 200 |
| same, 12 busy loops beside it | 2 / default / `-race` | 11 / 26 / 13 of 200 |
| Xeon E3-1270 v6, Linux 6.8, idle, static linux/amd64 binary | 1 | 100 of 200 |
| same | 2 | 25 of 200 |
| same | 4 | 5 of 200 |
| same | default (8) | 96 of 200 |

Go uses one P on a machine or VM with one CPU, so a one-CPU CI runner fails the test every time with `-count=2` or more. In a Linux x86_64 VM with one vCPU it failed 209 of 209 runs. In a deterministic VM with two vCPUs that run in turns of a fixed number of guest branches, the result depends only on the turn length: turns of up to 18,700 branches pass, 18,800 or more fail, every time.

wdyt about adding a `GOMAXPROCS=1 -count=2` run as a regression check next to #8794?

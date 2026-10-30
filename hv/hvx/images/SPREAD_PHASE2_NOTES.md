# SPREAD_ON_HVX Phase 2 — two brokers + consistency oracle + seeded disk faults

Status: **DONE** (2026-08-23). Deliverables (a) determinism, (b) seeded-fault reproducibility,
and stretch (c) branch — all observed. One documented finding: the broker-crash op stalls the
fs backend (see below), so it is opt-in and OFF by default.

## What runs
TWO co-located `spread` brokers on the **`fs` backend**, both listening on loopback
(B1 127.0.0.1:8080, B2 127.0.0.1:8081) and **sharing ONE rw ext2 data disk** (`/data`, MBR
partition p2). They coordinate only through the shared directory log (observed: B2 tails B1's
appends to the same `applied_seq`). Driven by the FULL mini-Jepsen op-program runner
(`spread-drv` no-arg `runProgram`): sends/receives/nacks across both brokers, the metric-gated
drain (drains until `spread_queue_messages==0` on every live broker), then the invariants
(at-least-once / FIFO / no-split-brain / no-phantom). `/init` maps `spread: consistent` ->
`HYDO-ORACLE: OK`, any `BUG-ASSERT` -> `HYDO-ORACLE: VIOLATION`.

Disk layout (like Phase 1): ext4 **ro-root** (p1) carries the spread closure + both binaries,
EXCLUDED from the snapshot + hash-stable; the ext2 **rw data disk** (p2) is the shared `fs`
`--data-dir` AND the seeded-fault target. Whole-boot fingerprint = guest RAM + p2 + console.

## Results (observed in output)
- **(a) determinism, fault-free** — verdict `HYDO-ORACLE: OK consistent 2brokers`; fingerprint
  **`7b576827407c0644`** IDENTICAL across REPLAY (two boots), ROUND-TRIP (snapshot->restore),
  and BRANCH (snapshot at a pre-initramfs marker, continue twice). ro-root `81203333c65e3342`
  stable. `RPT sat send-accepted / queue-drained / msg-nacked`.
- **(b) seeded DISK fault (DROP, 30%) on the shared data disk** — `mode=fault PASS`:
  seedA=0x51a0 run1==run2 = **`9b2b4ef8f30e2a66`** (reproduces bit-for-bit), seedB=0x51a1 =
  **`3f966b6255c45996`** (seed-SENSITIVE — a different seed gives a different fingerprint), all
  verdicts `OK consistent 2brokers`. The fault fingerprint DIFFERS from the clean baseline
  (`9b2b…` vs `7b57…`) — the DROP deterministically perturbs the on-disk `/data` bytes.
  (No consistency VIOLATION surfaced: at 30% DROP, in a single run without a broker restart,
  committed messages are still delivered from spread's in-memory state; DROP only diverges the
  on-disk persistence. Surfacing a durability bug is Phase-3 search, not required here — the
  deliverable is that the FAULTED loop is deterministic + reproducible, which holds.)
- **(c) branch, stretch** — included above (BRANCH IDENTICAL).

Fingerprint summary: clean `7b576827407c0644` · DROP seedA `9b2b4ef8f30e2a66` · DROP seedB
`3f966b6255c45996` — three distinct, each internally reproducible. ro-root `81203333c65e3342`.

## FINDING: the broker-crash op (X 2) stalls the fs backend — OFF by default
`X 2` SIGKILLs broker 2 (failover test). On the `fs` backend this makes the **surviving broker
busy-SPIN** (it waits out the dead broker's shared-log lease); because the guest never goes idle,
the deterministic clock barely advances and the driver's metric-gated drain cannot reach
`messages==0` within its (virtual-time) budget. Symptom: an hvx boot that would finish in ~4 s
instead runs indefinitely at 100% vCPU — it even saturates the L1 dev VM's single vCPU hard
enough to drop its ssh. Reproduced twice; the fault-free (crash-omitted) run always finishes in
~3.8 s and is `consistent`. Diagnosis: this is spread's fs-backend failover behavior (a
lease/lock timeout), not a harness bug — the loopback survivor legitimately waits for the lease.
Consequence: **`spread_crash` defaults to 0** (crash omitted). It is opt-in via the kernel
cmdline token `spread_crash=1` (or `SPREAD_CRASH=1 bash run-spread2-demo.sh …`) for a future
failover experiment — which will first need a bounded/shortened lease or a bounded drain so the
survivor does not busy-spin. The pid-file plumbing for X is in place (`/b1.pid`,`/b2.pid` are
build-time symlinks into the tmpfs so /init can write the broker pids on the read-only root).

## Files
- `build-linux.sh` §2n — the phase-2 rootfs target (distinct `spread2-*` artifacts; §2m Phase 1
  untouched). `/init` starts both brokers on shared `/data`, writes pids, creates queue `q`, and
  runs the dynamically-built op-program (crash op selectable by `spread_crash`).
- `build-spread2.sh` — fast isolated builder (evals §2n; single source of truth).
- `hvx/hvx_spread_verify.c` — added `fault` mode: same seeded DROP twice must fingerprint
  IDENTICAL (verdict OK or VIOLATION both accepted); seed-sensitivity reported (informational).
  Reuses the existing replay/roundtrip/branch machinery + the generic `HYDO-ORACLE:` verdict.
- `hvx/run-spread2-demo.sh` — `serve|replay|roundtrip|branch|fault|all` (+ `SPREAD_CRASH`,
  `SPREAD_FAULT_PROB`, `SPREAD_SEED_A/B`).
- `hvx/spread-drv/main.go` — the reused driver (the `smoke` subcommand is Phase 1; Phase 2 uses
  the no-arg op-program runner).
- `hvx/native-spread2-smoke.sh` — F0 native (L0) sanity for 2-broker fs sharing.

## Reproduce (on the box)
```
cd ~/hydo && ./build-spread2.sh && bash hvx/deploy-hvx.sh
bash hvx/run-spread2-demo.sh all         # serve + replay + roundtrip + branch + fault (crash=0)
# failover experiment (KNOWN to stall on fs — bound the drain/lease first):
SPREAD_CRASH=1 bash hvx/run-spread2-demo.sh serve
```

## L1 hygiene learned
- Recover a wedged L1 with `kill -9 $(cat ~/l1/l1.pid)` then `bash ~/hydo/hvx/run-l1.sh`.
  Do NOT `pkill -f l1-hvx` from an ssh command — the pattern matches your own command string and
  self-kills the session (RC 255). Use the pidfile.
- For a possibly-hanging boot, cap `hvx_linux` itself with `timeout <n>` so it self-terminates
  and cannot leave an orphan pegging L1.

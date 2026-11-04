# SPREAD_ON_HVX Phase 3 — the autonomous snapshot-restore search loop

## PHASE 4 (2026-08-27): reachability-GUIDED search — the coverage substitute (repeat's fork-fuzz port)
Ported repeat's assertion-guided search onto the spread search loop so it STEERS toward interesting
states instead of generating purely-random workloads. hvx has no edge-coverage bitmap, so the novelty
signal is the ORACLE's own reachability markers: the in-guest driver already prints `RPT decl/sat
<name>` (send-accepted, send-fenced, queue-drained, send-concurrent, broker-paused, msg-nacked, ...).
ALL changes are host-side in `hv/hvx/hvx_spread_search.c` + `hv/hvx/run-spread3-search.sh` — the boot path,
`hvx.c`, `build-linux.sh`, and `spread-drv` are UNTOUCHED, so the Phase-2 baseline `7b576827407c0644`
is preserved by construction and each program still runs deterministically (snapshot restore).

What was built (mirrors `repeat`'s `cmd_fork_fuzz` + `propose_spread_seq` + `--guide`):
- `scan_run_markers()` parses `RPT sat` from the reconstructed console -> a per-program marker BITMASK.
- A CORPUS of programs; a program is KEPT iff its marker-COMBINATION was never seen (marker-combination
  novelty replaces edge-coverage novelty — the coverage substitute). Random mode keeps no corpus.
- The generator draws `sm_below(corpus_n)` + MUTATES (add/drop/tweak ops, add-biased to GROW), plus 20%
  fresh — building on programs that already reached a new state. Splitmix64, so same seed+snapshot =>
  same campaign. `HVX_GUIDE=1` adds the champion popcount hill-climb (repeat's `--guide`: focus-mutate
  the deepest-combo program toward a host-computed, driver-free DISTANCE = markers-missing-from-best).
- Grammar extended with Z/U (partition/heal a broker) to unlock the HARD markers `broker-paused` and
  `send-fenced` (a concurrent `P` while a broker is paused times out -> fenced) — an op ORDERING blind
  random rarely lands in one SHORT program but the corpus can ASSEMBLE. Fresh programs shortened to
  3..6 ops so depth must be assembled (with long programs a single blind draw already covers the
  shallow 6-marker space — an honest limit of a reachability-only signal).
- Selected by `HVX_SEARCH_MODE=guided|random`; per-VIOLATION it prints a BIT-FOR-BIT reproducer
  (`BUG-REPRO` + the exact `BUG-ASSERT` line + program + seed + fault seed).

Guided-vs-random, 30 programs/campaign, short grammar, seeds 1..8 (all 30/30 consistent, 0 inconclusive):
- **send-fenced (hard marker) reached: guided 6/8 seeds vs random 4/8.**
- **deepest single-program combo: guided >= random on 7/8 seeds (mean 5.0 vs 4.6);** guided strictly
  deeper on seeds 1,2,6; random deeper only on seed 3.
- **corpus growth: guided accumulates 5..7 reusable building-block programs; random keeps 0.**
- distinct markers over the campaign: near-tie (mean 5.6 vs 5.5) — the union saturates the small
  6-marker space either way; the discriminator is the DEEPEST single-program combination, not the union.
- Illustrative clean case, seed 1: guided markers 6/11 (incl send-fenced) / deepest pop=5 / combos 6 /
  corpus 6  vs  random 5/11 (NO send-fenced) / deepest pop=4 / combos 5 / corpus 0.
Honest read: the guidance PAYS OFF on DEPTH and hard-marker reach + gives a reusable corpus, but the
edge is modest because the reachability signal is only ~6-dimensional and runs are independent, so
blind sampling of short programs covers the shallow space nearly as well. Coverage-guidance shines
when the signal is high-dimensional (repeat's 65536 edges); a reachability-only substitute is real but
bounded. `HVX_GUIDE=1` champion mode reached deepest ~5.0 (no better than breadth here — assembling
msg-nacked INTO the fence combo in one short program is genuinely rare).

Determinism (the Phase-3 discipline, preserved): (a) same program twice -> IDENTICAL fingerprint
(prog#0 run1==run2 on EVERY seed, both modes — fp scoped to console + durable rw-disk p2, RAM excluded);
(b) same seed => same campaign: guided seed 1 run A vs run B IDENTICAL across ALL 30 programs
(timing-stripped). Z-based fencing (a ~2 s virtual-clock HTTP timeout) reproduces byte-exactly under
restore — the determinism proof covers it.

No REAL BUG-ASSERT surfaced. NB: an early guided seed-2 run reported 2 `fifo-reorder` asserts that were
HARNESS FALSE POSITIVES — the mutator had duplicated a message body (two `S g3 m1`), and the driver's
FIFO oracle dedups delivered bodies, so a duplicate reads as a reorder. FIXED by `restamp_body()`
(unique per-program bodies, restoring the generator's invariant); after the fix all 8x2 campaigns are
30/30 consistent. (Discipline: investigated the "bug" before claiming it — it was mine, not spread's.)

KNOWN substrate limit (NOT this change; reproduces identically in BOTH modes, so unbiased): beyond
~restore 35 the guest degrades to INCONCLUSIVE runs (driver output truncated) — a deep-restore-count
issue in `hvx.c` (hydo-16's), which the original Phase-3 never hit (it ran 30). Campaigns are capped at
nprog<=30 (the proven-clean range).

Reproduce (on the build machine, L1 up, harness already built into /tmp/hvx-src):
```
HVX_SEARCH_MODE=guided bash hv/hvx/run-spread3-search.sh 30 1   # reachability-guided (corpus)
HVX_SEARCH_MODE=random bash hv/hvx/run-spread3-search.sh 30 1   # blind-random baseline
HVX_SEARCH_MODE=guided HVX_GUIDE=1 bash hv/hvx/run-spread3-search.sh 30 1   # + champion hill-climb (--guide)
```

## PHASE 3 COMPLETE (2026-08-23): the deterministic search loop runs.
hydo-16 fixed the deep-restore wall (fuzz ports 0x510/0x511 collided with the virtio-blk doorbell
range; with a disk attached the virtio handler answered `in 0x511` first -> "not ready" forever;
routed the fuzz ports past virtio, fingerprint-neutral). The loop now COMPLETES: boot once to the
READY snapshot, then per generated op-program restore (~0.15 s) + stage via HVX_SET_FUZZ_INPUT + run
to a deterministic halt + read the mini-Jepsen verdict.

Determinism-mismatch RESOLVED (my part):
- ROOT CAUSE (RAM-diff of two restores of the SAME program): console byte-IDENTICAL, rw-disk p2
  byte-IDENTICAL, only **8 bytes of ONE kernel page** (printk-ring / scheduler bookkeeping, values
  flipping 0x00<->0xff) differ -- benign kernel-internal non-determinism, NOT app/log/queue state.
  A deterministic post-DONE `halt` (guest halts IF=0 -> EXIT_HLT at a fixed icount; harness stops
  there) did NOT eliminate it -- the 8 bytes are intrinsic kernel state at the halt, not a
  stop-point artifact.
- FIX: fingerprint the DETERMINISTIC, finding-relevant state = console (the driver's verdict/
  METRICS/RPT) + p2 (the durable fs-backend queue/log), EXCLUDING volatile guest RAM
  (`fingerprint_search`, spread-search-v2). Kept the deterministic halt + `run_until_halt` (stop at
  EXIT_HLT, no wall-clock SIGALRM that would perturb idle-heartbeat ticks) for a clean stop.
- PROOFS (observed): (a) same program twice -> IDENTICAL fp (e.g. 69e15931cf04f181, 3e68b4d4d3b4c387);
  (b) distinct programs -> distinct fps (23+ unique of 30; a few benign collisions are programs with
  the same observable end-state, e.g. all-fenced -> empty queue); (c) Phase-2 no-input baseline still
  **7b576827407c0644** (module fix + harness changes snapshot-neutral).
- FIRST CAMPAIGN: no-fault 30 programs (seed 1) -> 30/30 consistent, avg restore+run **0.247 s**;
  seeded DROP fault (seed 0x51a0, prob 30%) 30 programs -> 30/30 consistent, avg **0.245 s**,
  deterministic under fault (fault fp != no-fault fp, so the DROP is exercised + reproducible). No
  BUG-ASSERT surfaced (finding a bug is future search breadth; the deliverable is the deterministic
  machine, which any finding would replay bit-for-bit).
- Reproduce: `hv/hvx/images/build-spread3.sh && bash hv/hvx/deploy-hvx.sh` then
  `bash hv/hvx/run-spread3-search.sh 30 1` (no fault) or the direct harness with a fault seed:
  `hvx_spread_search <bz> <initramfs-spread3> <spread3-disk.img> "<cmdline> spread_mode=full" 512 30 1 0x51a0 30`.

## Split-test result (2026-08-23, hydo-16's decisive test): HANGS — real-Linux + fuzz-read
## scheduling after a deep-snapshot restore, INDEPENDENT of the brokers.
Built `spread_mode=fuzzonly` in the §2o `/init` (build-linux.sh): boot real Linux + JUST fuzz-read
(NO brokers, NO spread-drv driver, NO oracle), print `FUZZREADY`, spin on the port, echo whatever is
read (`FUZZONLY-READ=`). Ran boot -> `FUZZREADY` -> snapshot -> restore -> `HVX_SET_FUZZ_INPUT` a
known program -> check. **Result: the staged byte is NEVER read/echoed after restore — 0 console
output, idle-HLT (heartbeat THROTTLE), `SPREAD-RUN-DONE` never reached (20 s / 53 iters); restore
latency was 0.157 s.** fuzz-read runs fine the FIRST time (it prints `FUZZREADY` before the
snapshot), but after `HVX_SET_SNAPSHOT` the runnable busy-poller is not dispatched — the idle task
runs instead. Since this reproduces with NO brokers and NO driver, the Phase-3 wall is NOT the
spread app (brokers/driver/pipe) — it is **real Linux + a userspace process (fuzz-read) failing to
be re-dispatched after a deep-snapshot restore**, i.e. the Linux scheduler/heartbeat interaction
after restore. This is DIFFERENT from hydo-16's bare busy-poll isolation (no real-Linux userspace
process) which resumes byte-exact. Handoff: hydo-16 to build a real-Linux + fuzz-read-only
"spread-lite" isolation on the hvx side to chase the heartbeat/scheduler-after-restore interaction.
Reproduce: `SPREAD_MODE=fuzzonly bash hv/hvx/run-spread3-search.sh 1 1` (or `/tmp/p3fuzzonly.sh`).

## Option A update (2026-08-23): the synchronous fuzz-input port was BUILT + PROVEN, but a
## SECOND wall appeared — deep mid-service snapshot restore does not RESUME the guest.

Status: **Synchronous input port DONE and proven clean; per-input restore latency 0.157 s
(sub-second, the payoff). BUT the per-input warm run is blocked: restoring the DEEP mid-service
snapshot (two brokers + the reader running) does not resume the guest's userspace execution — the
guest sits idle, 0 output. Distinct from Phase-2's SHALLOW pre-initramfs branch (re-ran fine) and
its deep round-trip (which only re-FINGERPRINTED, never re-ran).**

What works (observed):
- **Module port (`hv/hvx/hvx.c`)**: `HVX_SET_FUZZ_INPUT` (ioctl 25, `hvx_abi.h`) + IN ports 0x510
  (data) / 0x511 (status), fed via `hv/hvx/images/fuzz-read.c` (guest reader, `iopl(3)` — NOT `ioperm`, whose
  0..1023 range excludes 0x510). Confirmed by dmesg: the ioctl stages the buffer, the ports return
  the staged bytes during boot. Snapshot-EXCLUDED like the serial ring (named-field snapshot
  serialization never touches `fuzz_*`).
- **Snapshot-exclusion PROOF (the coordinator's obligation): the no-input Phase-2 baseline is still
  exactly `7b576827407c0644`** with the port compiled in (replay IDENTICAL, ro-root
  `81203333c65e3342`). Adding the port perturbs nothing. (Reproducing 7b57… required reverting the
  driver + not staging fuzz-read onto the Phase-2 disk — the driver binary + p1 layout feed the RAM
  page cache, which IS fingerprinted; so search assets live on a SEPARATE spread3 disk / §2o.)
- **Boot to READY + snapshot + restore**: reaches the `FUZZREADY` boundary, snapshots, and
  `HVX_SET_SNAPSHOT` + RAM/rw reload measures **0.157 s** (vs ~4 s cold boot — a ~25x per-input win
  if the warm run worked).

The wall (confirmed with a RIP probe + module port-trace): after `HVX_SET_SNAPSHOT` from the deep
`FUZZREADY` state, the guest's RIP alternates userspace(0x4010a1)/kernel and THROTTLEs on the idle
heartbeat, but does ZERO port I/O and produces ZERO console output for 90 s (242 iters) — it never
resumes fuzz-read's read of the staged program. So `HVX_SET_SNAPSHOT` restores the STATE (Phase-2
round-trip re-fingerprints deep state identically) but does not RESUME execution from a deep,
multi-process userspace snapshot. Hypothesis: the restored guest idles in a state the heartbeat
cannot wake (e.g., IF=0 / masked LAPIC timer / a scheduler-state field not in the snapshot), which
a shallow pre-initramfs snapshot (Phase-2 branch, guest actively in the kernel) never hits.

What a fix would take (next step): make `HVX_GET/SET_SNAPSHOT` capture+restore the execution-critical
state a deep userspace snapshot needs to RESUME — most likely the pending-interrupt / LAPIC-timer /
`inject_valid` fields and any "guest was runnable" bit — then re-run the branch test at the
`FUZZREADY` depth (not just re-fingerprint) as the acceptance. All Phase-3 upper-half pieces
(generator, restore loop, verdict parse, fingerprint, the port, fuzz-read, §2o rootfs) are built and
plug straight in once a deep snapshot resumes. Files: `hv/hvx/hvx_spread_search.c` (search harness with
the seeded generator + determinism proof + `HVX_SET_FUZZ_INPUT` feed), `hv/hvx/images/fuzz-read.c`,
`build-linux.sh` §2o (spread3 search rootfs), `build-spread3.sh`, `hv/hvx/run-spread3-search.sh`.

---

## (Prior status — the serial channel, superseded by the port above)

Status: **BLOCKED on a hvx primitive (as the plan anticipated). Machine BUILT; the serial input
channel proved unworkable under hvx's idle model. A synchronous input port (a small module change)
is required.** Reported per the coordinator's "hit a wall -> stop + report symptom + hypothesis +
what a synchronous port would take" instruction, rather than thrashing.

## What was built (all correct + reusable; only the INPUT CHANNEL is the blocker)
- **In-guest driver `looprun`** (`hv/hvx/images/spread-drv/main.go`): reads ONE op-program from the serial
  console until a lone `RUN` line, drives the two brokers + the full mini-Jepsen oracle, prints the
  verdict, bracketed by `SPREAD-READY-FOR-INPUT` / `SPREAD-RUN-DONE`. Two read strategies were tried
  (blocking scanner; then a NON-BLOCKING busy-poll `readProgramBusy` to avoid idle-parking the vCPU).
  Both PASS natively (`spread: consistent`, `SPREAD-RUN-DONE`).
- **/init SEARCH mode** (`build-linux.sh` §2n, kernel cmdline `spread_mode=search`): after both
  brokers are up + queue `q` created, prints the BUSY snapshot-boundary marker `SPREAD-SNAPSHOT-READY`
  then runs `spread-drv looprun < /dev/console`. Default (token absent) = the Phase-2 baked path
  (unchanged). One rootfs, cmdline-selected workload (Phase-3-aligned).
- **Host search harness** (`hv/hvx/hvx_spread_search.c`): boots ONCE to `SPREAD-SNAPSHOT-READY`, takes a
  file-backed snapshot (HVX_GET_SNAPSHOT + save RAM + save the rw data disk p2), then per program:
  HVX_SET_SNAPSHOT + reload RAM/p2, stage the program over `HVX_PUT_INPUT` (256B chunks) BEFORE
  resume, run to `SPREAD-RUN-DONE`, parse the verdict from the reconstructed console, and fingerprint
  (this program's console + RAM + p2, EXCLUDING the absolute console length so the SAME program
  fingerprints identically at any loop position). Includes: a **seeded generator** (`gen_program`, a
  splitmix64 port of repeat's `propose_spread_seq` — S/R/N/P over g0..g3, unique bodies, tier-faults
  and broker-crash omitted), a **determinism proof** (prog#0 twice -> identical verdict + fingerprint),
  and an optional per-restore seeded DROP disk-fault schedule (reuses Phase-2 fault wiring).
- **Run script** `hv/hvx/run-spread3-search.sh`; harness wired into `hv/hvx/deploy-hvx.sh`. Native F0:
  `hv/hvx/images/native-spread3-looprun.sh` (PASS).

## The wall (confirmed with instrumentation)
The serial-fed loop cannot make progress because **`HVX_RUN` never returns to userspace while the
guest is permanently idle, and hvx's idle heartbeat — which the workload REQUIRES — is what causes
that.** Details:
- The guest DOES reach both markers — booting the search disk with `hvx_linux` shows the full boot,
  `create q -> 201`, `SPREAD-SNAPSHOT-READY`, `SPREAD-READY-FOR-INPUT`. So the bytes/markers are fine.
- But the driver then blocks reading the serial for a program (none fed yet during boot-to-READY) ->
  the guest goes permanently idle -> with `HVX_VM_IDLE_HEARTBEAT` on, the module keeps jumping the
  clock and re-entering the guest IN-KERNEL without returning `HVX_RUN`. A heartbeat print inside the
  harness's wait loop confirmed it: **`HVX_RUN` returns exactly ONCE (`it=0`, reason=THROTTLE, 4858
  console bytes) and the second call never returns.** So the host can neither drain the READY marker
  nor feed the program — the run loop is stuck in one `ioctl(HVX_RUN)`.
- Turning the heartbeat OFF makes an idle guest return `EXIT_HLT` (so the host COULD drain + feed),
  but then the workload's metric-gated drain stalls: its `time.Sleep(500ms)` timer-waits only fire if
  the clock advances on idle, which is exactly the heartbeat's job. So heartbeat-off breaks the drain.
- The guest **busy-poll** (`readProgramBusy`, non-blocking reads, no sleep) was the attempt to keep
  the guest runnable so `HVX_RUN` returns with the heartbeat ON. It passes NATIVELY, but under hvx the
  harness ran its full budget (60s / 200s force-kill) without completing a single program — most
  likely the Go runtime still parks the idle M behind the busy goroutine (re-introducing the idle),
  and/or a boot-time idle. It could not be confirmed to reach READY because each stuck run saturates
  the nested L1 dev VM's single vCPU so hard that L1 stops responding (recovered each time via
  `kill -9 $(cat ~/l1/l1.pid)` + `run-l1.sh`).

This is precisely the gap the plan flagged: *"hvx has no synchronous one-shot fuzz-input port
(repeat's fw_cfg); build a small one if driving per-restore."* The serial channel (HVX_PUT_INPUT +
UART RX) is asynchronous/interrupt-driven and, combined with the mandatory idle heartbeat, cannot be
interleaved with a host restore loop.

## What a synchronous input port would take (the fix)
A module-level fw_cfg-equivalent — the same shape repeat solved with `guest/fwcfg-input.c`:
1. **Module (`hv/hvx/hvx.c`)**: handle a guest port-I/O read on a chosen port (e.g. `IN al, 0x510`) by
   returning the next byte from a host-set buffer, synchronously (no interrupt). Add one ioctl,
   `HVX_SET_FUZZ_INPUT`, to set that buffer (snapshot-excluded, like HVX_PUT_INPUT). ~1 port handler
   + 1 ioctl, ~40 lines.
2. **Guest reader**: a ~30-line freestanding reader that does `in al, dx` in a loop until the `RUN`
   terminator, writing the program to a pipe/file the driver reads — OR the driver reads the port
   directly. No blocking read, no idle: the guest spins on port I/O (each `IN` is a VM-exit the
   module services immediately, so `HVX_RUN` returns normally and the heartbeat stays ON for the
   workload), and the read is a PURE FUNCTION of the host buffer -> byte-deterministic across restores.
3. **Everything else already built plugs straight in** — the generator, the restore loop, the
   determinism check, the fingerprint, the /init search mode. Only the input CHANNEL changes
   (port I/O instead of `/dev/console`), a small localized edit to `looprun` and the harness's
   `feed_input`.

Estimated effort: a few hours (the primitive is small and well-understood — repeat's fw_cfg port is
the exact template, and OpenThesis's `libvmcall.c` shows the host↔guest port pattern).

## Reproduce (what runs today, up to the wall)
```
cd ~/hydo && hv/hvx/images/build-spread2.sh && bash hv/hvx/deploy-hvx.sh
# native (works): bash hv/hvx/images/native-spread3-looprun.sh   -> SPREAD-READY-FOR-INPUT ... spread: consistent ... SPREAD-RUN-DONE
# under hvx (blocks in the READY wait as above):
bash hv/hvx/run-spread3-search.sh 24 1
# diagnosis: boot the search disk with hvx_linux to SEE the guest reach both markers, then idle:
#   sudo env HVX_DISK=/tmp/spread2-disk.img HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 \
#     /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-spread2.cpio.gz "<cmdline> spread_mode=search" 640
```

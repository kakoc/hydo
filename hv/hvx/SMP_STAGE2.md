# SMP Stage 2: real Linux boots with 2-4 vCPUs, reproducibly (2026-10-22)

Stage 2 of `hvx/SMP_PLAN.md`: real Linux 6.6.35 (`~/hydo/build/bzImage` +
`initramfs.cpio.gz`, command line `acpi=off x2apic_phys`) boots with 2, 3 and 4 vCPUs on
the Stage 1 scheduler, brings every CPU online ("smp: Brought up 1 node, N CPUs"),
reaches the initramfs shell marker, and is bit-for-bit reproducible: two fresh boots and
branch-from-snapshot give identical full-state fingerprints. One-vCPU guests are
unchanged (the 25-check regression gate passes with every fingerprint unchanged).

Developed on a side-by-side module `hvxsmp2.ko` (`/dev/hvxsmp2`, built by
`hvx/smp2/build-hvxsmp2.sh` in `~/hydo-smp2` on the bare-metal test machine). The main `hvx.ko`
was not touched. `hvxsmp2` was unloaded at the end.

## What was built

All new module behaviour sits behind a new opt-in flag, so a VM without it runs exactly
the code it ran before.

- **`HVX_VM_SMP_BOOT` (flags bit 6, `hvx_abi.h`).** Needs `HVX_VM_SMP_SCHED`
  (CREATE_VM returns EINVAL otherwise) and `n_vcpus > 1` (ignored with one vCPU). On a
  CPU without the "unrestricted guest" VMX control CREATE_VM returns ENODEV.
- **AP start-up by INIT and STARTUP IPIs, as on hardware and in KVM** (`hvx.c`:
  `hvx_lapic_init_sipi`, `hvx_ap_init`, `hvx_ap_sipi`). vCPUs 1..n-1 start in
  wait-for-SIPI (`halted == 2`, a new value of the existing scheduler field, so no ABI
  struct changed size). An x2APIC ICR write with delivery mode 5 (INIT, level assert)
  puts the target in wait-for-SIPI and resets its local APIC (ID and IA32_APIC_BASE
  kept, pending interrupts dropped); INIT de-assert is ignored. Delivery mode 6
  (STARTUP) with vector V starts a vCPU that is in wait-for-SIPI in real mode at
  CS = V<<8, IP = 0 (base V<<12, 64 KiB limits, CR0 = ET|NE, EFER = 0); a STARTUP to a
  running vCPU is ignored, so Linux's second SIPI is harmless. Only state changes: the
  target runs at its next scheduler turn. A vCPU never INITs or STARTs itself.
- **Unrestricted guest** (`setup_vmcs`): with SMP_BOOT the secondary control is set,
  CR0.PE and CR0.PG are no longer forced to 1 (the other CR0 fixed bits still are), and
  the IA-32e-mode entry control follows the guest's EFER.LMA. Because the AP moves from
  real mode through protected mode to long mode without VM exits (CR0 is guest-owned),
  the run loop also re-syncs that entry control with `GUEST_IA32_EFER.LMA` before every
  entry (SMP_BOOT only).
- **MP table per CPU** (`hvx_boot.h`): `build_mptable_n` / `load_bzimage_n` list one
  processor entry per vCPU, APIC ID = vCPU index, CPU 0 the boot CPU.
  `build_mptable` / `load_bzimage` call them with 1, so every existing client writes
  the exact bytes it wrote before.
- **CPUID topology** (SMP_BOOT only): leaf 1 EBX[23:16] = n and EDX.HTT, leaf 0xB with
  an SMT level (1 thread) and a core level (n cores, shift = ceil(log2 n)). Linux reports
  "Max logical packages: 1".
- **Device interrupts routed by destination** (`hvx_dev_irq`, `hvx_ioapic_dest`,
  `hvx_msi_dest`): with SMP_BOOT, COM1 and virtio-blk INTx go to the vCPU named by the
  physical destination of their IOAPIC redirection entry (bits 63:56), and virtio-blk /
  virtio-net MSI-X to the vCPU named by the message address (bits 19:12). A logical
  destination or an unknown ID falls back to vCPU 0. Without SMP_BOOT the helper is the
  old priority chain on vCPU 0 only. Device completions (`hvx_dev_due`) come due on
  whichever vCPU runs, since in scheduler mode the instruction clock is VM-wide (Stage 1
  already arms completions from the VM clock); without SMP_BOOT only vCPU 0, as before.
  Observed: COM1 routed to vCPU 1 in a 2-vCPU boot, the virtio-blk queue MSI-X to
  vCPU 3 in a 4-vCPU disk boot; both delivered and the disk read completes.
- **Scheduler:** a vCPU in wait-for-SIPI is never picked and no interrupt wakes it.
  `HVX_SET_SCHED` accepts `halted` 0..2 (2 only with SMP_BOOT).
- **Harness `hvx/hvx_smp_boot_verify.c`** (a copy of `hvx_boot_verify.c` for N vCPUs):
  boots with `PMU_CLOCK | SMP_SCHED | SMP_BOOT | PAUSE_EXIT`; the fingerprint hashes the
  console, every vCPU's `HVX_GET_SNAPSHOT`, the scheduler state from `HVX_GET_SCHED`
  (not its statistics) and all guest RAM (and the disk with `HVX_DISK`). Checks: two
  fresh boots; branch at the first return after the "Trying to unpack rootfs" marker;
  branch at the end of turn T/2 (`stop_turn`); every branch must equal the fresh boot,
  not only itself. Modes: `HVX_MODE=one` (one boot), `scan` (each vCPU's mode at every
  turn end), `apbranch` (below), `HVX_BRANCH_TURNS` (extra branch points). It reports
  wall time and scheduler statistics (turns, landing steps, overshoots, time jumps,
  wake-ups, per-vCPU branches and turns, where COM1 / virtio-blk interrupts were routed).
- **Hand-written test `hvx/hvx_sipi_test.c`:** vCPU 0 sends INIT, INIT de-assert and two
  STARTUPs to vCPU 1, which runs a stub at 0x8000 through real mode (writes via a segment,
  branch loop), loads a GDT, protected mode (branch loop), PAE + EFER.LME + paging, and
  64-bit code. Swept over turn lengths 1..300 so turn boundaries fall in all three modes.

## Walls found and fixed

The INIT/SIPI path and the Linux boot worked on the first run; nothing in the kernel
side had to be fixed after testing. The walls were in the harness and in tuning:

1. **Stop point past the marker (harness).** `run_until_marker` always called `HVX_RUN`
   before looking at the console. Returns are rare in scheduler mode (about 7 THROTTLE
   returns per boot), so the return that reached the mid marker could already contain the
   final marker; the branch's straight run then went one slice further than a fresh boot
   and its fingerprint differed (console equal, RAM and CPU state not) -- seen with 4
   vCPUs and seeded turns. Fixed: check the console first when the current position is a
   THROTTLE return; after a stop a fresh boot does not make (a turn stop or a max_insns
   stop) run on to the next return. The same latent pattern is in `hvx_boot_verify.c`; it
   does not trigger there with the default boot (mid and final markers are in different
   slices), and that file was not changed.
2. **Guest time balloons with long PAUSE spins.** With `pause_k = 16` and 4 vCPUs the boot
   took 4.0 s and guest time reached 195 s (vs 15 s): every PAUSE exit is a retired exit,
   and in the free-run COUNTER mode a retired exit moves the virtual TSC by `tsc_step`
   (0x10000 ticks = 41 us) while the vCPU has a timer deadline armed, so 16 PAUSEs per
   yield cost 0.65 ms of guest time; stop_machine and optimistic mutex spinning (no
   `vcpu_is_preempted`, the guest does not know a lock owner's vCPU is not running) do
   many such yields. Default `pause_k = 4`: 0.84 s and 15 s of guest time. Deterministic
   either way; this is a speed and guest-time effect only.
3. **"Marking TSC unstable" on 2 vCPUs.** The clocksource watchdog marks the TSC unstable
   against refined-jiffies in the 2-vCPU boot. This is not an SMP effect: the one-vCPU
   boot on the PMU clock prints the same message (checked on this module). With 3 and 4
   vCPUs the timing differs and the TSC stays the clocksource. With `tsc=reliable` on the
   command line the watchdog is off and 2 and 4 vCPUs pass all checks too. The TSC warp
   test between CPUs (`check_tsc_sync_target`) runs and reports nothing: with one VM
   clock and serialized vCPUs the TSC never goes backwards across CPUs.

## Results (bare-metal test machine, `/dev/hvxsmp2`, quantum 2^20, pause_k 4)

| vCPUs | config | boot wall | turns | fingerprint (fresh x2, branch@marker, branch@turn) |
|---|---|---|---|---|
| 1 (no scheduler, same flags) | | 0.75 s | - | (time only) |
| 2 | default | 0.87 s | 10,059 | `6584c954c2839491` all four equal |
| 3 | default | 0.86 s | 2,363 | `e1a2bb0dd744a9ab` all four equal |
| 4 | default | 0.84 s | 1,640 | `1dfafae6887e3a01` all four equal |
| 2 | `tsc=reliable` | 0.86 s | 2,614 | `79ed785d310ef13f` all four equal |
| 4 | `tsc=reliable` | 0.85 s | 2,177 | `35915a10e2233351` all four equal |
| 2 | quantum 65,536 | 3.42 s | 13,250 | `a0cb8d327581b1ed` all four equal |
| 4 | quantum 65,536 | 3.36 s | 6,148 | `0ef3c2e56fb1c1ec` all four equal |
| 2 | seeded turns (seed 7) | 2.32 s | 271,949 | `2e00f6715fa87a53` all four equal |
| 4 | seeded turns (seed 7) | 1.10 s | 2,950 | `88b3bc87402dbc91` all four equal |
| 2 | virtio-blk disk boot | 0.78 s | 2,675 | `0633dc872c0f43fd` all four equal, plus 6 branches at turns 300..1200 |
| 4 | virtio-blk disk boot | 0.88 s | 4,019 | `5853dd303083e5f3` all four equal |

Every row was run with the final module (srcversion `395FD48F279DF0062A24A5C`) and the
final harness. "All four equal" = boot #1, boot #2, branch at the mid marker and branch at
turn T/2 (continued and restored) give the same fingerprint. Disk boots use
`initramfs-disk.cpio.gz` + `vblk-disk.img` with final marker
"hydo-disk-marker: vda read complete"; three of the six extra branch points (turns 800,
1000, 1100) fall while the virtio driver loads and reads the disk.

- Statistics, 2 vCPUs default: 182,115,528 guest branches (vCPU 0 158,230,161, vCPU 1
  23,885,367), 173 ticks, about 472,000 landing steps, 0 PMU overshoots, 0 turn
  overshoots, 0 deferred turn ends, 9,556 all-halted clock jumps, 9,839 wake-ups.
  4 vCPUs: 182,747,153 branches, 469,000 landing steps, 288 jumps, 1,335 wake-ups.
- SMP costs about 12-16% wall time over the same boot on one vCPU (0.75 s).
- **Branch inside AP start-up (`HVX_MODE=apbranch`):** vCPU 1 becomes runnable after
  turn 99; stepping it one exit at a time (`HVX_RUNC max_insns = 1`) gives 10 stop points
  before it reaches the kernel's long-mode CS: the unstarted real-mode state (CS 9900),
  six points inside the real-mode trampoline and `verify_cpu` (CS 9800, CR0 = ET|NE),
  and two in protected mode without paging (CR0 0x50033, EFER 0 then 0x901). A snapshot
  at each, continued and restored, equals the fresh boot: 10/10 for 2 vCPUs, 10/10 for
  4 vCPUs.
- **Under host load:** 12 boots with 2 vCPUs and 12 with 4 vCPUs while 5 busy loops ran
  on the 8-thread host: all 12 + 12 fingerprints identical to the baselines.
- **`hvx_sipi_test`:** PASS, 6.3 s. Default quantum: all markers written, the AP runs its
  stub once (second STARTUP ignored), ends with EFER.LMA and CS 0x18, fingerprint
  `eef5c1ebd4df2e7d` twice. Turn lengths 1..300: replay identical for every length,
  44 branch-from-snapshot checks at turn T/2 identical, sweep hash `c8d015ab3b57252d`.

## Regression gate (on `/dev/hvxsmp2`, final module)

- `SIDE_TMP=/var/tmp/smp2-side HVX_DEV=/dev/hvxsmp2 hvx/race/side-metal.sh`:
  **ALL PASS (25 checks)**, every fingerprint unchanged, both boot verifies included
  (no disk `7a5559028ffbf52c`, with disk `36583d533d7c989b`).
- `hvx_sched_pmu_test`: PASS with the Stage 1 numbers (turn hash `97b3468c16d739fa`,
  landing hashes `f841184e6fdde8b5` / `19337dfb08c526f9`, fingerprints
  `95e6c9a02d5d584d`, `05521a9492a3ce9f`, `b544e114dc426e85`, snapshot
  `73a465516c4e0094`).
- `hvx_sched_race_test`: PASS, the same table and grid hash `8cb7c61a2393f44e`.
- `hvx_sched_sweep_test 2000`: PASS, 2000 turn lengths, 0 mismatches, 894 every-turn
  branches and 189 mid-turn branches with 0 mismatches, sweep hash `39971eed9c2f35fe`
  (the main `hvx.ko`, which carries Stage 1 without these changes, gives the same
  hash and counts for the same test binary; that run only used `/dev/hvx`, it did not
  reload anything).
- One-vCPU boot on the PMU clock through this module (`hvx_boot_verify`,
  `HVX_PMU_CLOCK=1`): whole boot `d33e654985f994f7`, branch `4348b40259e1353d`, both
  identical.

## What remains / not verified

- **The four-service stack with 2 vCPUs** (the second half of the plan's Stage 2) was not
  attempted. `hvx_explore` and `hvx_stack_verify` still snapshot and fingerprint vCPU 0
  only (another agent owns those files).
- **Not in the main `hvx.ko`**: only the side module ran this code; `deploy-metal.sh` was
  not run.
- CPU hot-plug (offline/online, a second INIT to a running vCPU) and INIT to vCPU 0 are
  not tested. NMI IPIs (delivery mode 4) are still dropped; Linux uses them only for
  backtraces and panic.
- Logical-destination IOAPIC entries and MSI messages fall back to vCPU 0. For
  virtio-net only the first deliverable queue is considered when routing; with queues
  routed to different vCPUs a second queue waits until the first is delivered. Neither
  occurs in these boots; virtio-net was not booted with SMP.
- No paravirtual help: no `PV_UNHALT` / `PV_SCHED_YIELD`, no steal time, so the guest's
  optimistic spinning cannot see that a lock owner's vCPU is not running; PAUSE turn ends
  bound it.
- Only the no-disk and virtio-blk initramfs boots were tried, with 256 MiB; no rootfs,
  JVM, or service images. Linux's behaviour with 5-8 vCPUs is untested.
- The guest-time growth from PAUSE spins (wall 2) is a property of the free-run COUNTER
  clock with PAUSE exits; it is deterministic but makes guest time depend strongly on
  `pause_k` and the vCPU count.

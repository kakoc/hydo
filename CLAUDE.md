# hydo — project context & handoff

`hydo` is a **deterministic hypervisor**. It runs software so that, given the same inputs, execution is bit-for-bit identical every time, which makes record/replay, snapshots and branch-from-snapshot possible. The project has two engines: the original Rust KVM monitor (`src/`), which hit a hard KVM wall at `RDTSC`, and **hvx**, our own VT-x hypervisor kernel module (`hvx/`), which owns the VMCS and is where all current work happens. There is also an Apple Silicon backend (`hvx-mac/`) built on Hypervisor.framework.

- `README.md` describes the architecture, the paravirtual ABI, the clock strategies and the verification model.
- `hvx/HISTORY.md` holds the full history: every milestone section (1–69 and later), the old roadmap for booting Linux as L2, and the old long "Key files" list. Look up a milestone there by its number for details, evidence and older fingerprints.

## How to record new work (follow this)

New milestone sections go to the END of `hvx/HISTORY.md`. Each new milestone also gets a one-line entry in the "Milestone index" below and an update of "Current state" (and "Open items" if anything stays open) in this file. Keep this file short: details, evidence and superseded numbers belong in `hvx/HISTORY.md`, and this file lists only current values.

## Machines and how to run

### The bare-metal machine (where hvx runs)

- A Xeon E3-1270 v6 (not hybrid, working PMU), 32 GB, Ubuntu with kernel 6.8, passwordless sudo, main tree at `~/hydo`. hvx is loaded on the host itself; there is no nesting (M33). `AGENTS.local.md` says how to reach it; scripts use `on_bare_metal` from `machines.local.sh`.
- If the bare-metal machine wedges, only the user can restart it. Ask the user.
- Module locking: every VM run goes through `~/bin/hvx-run` (a shared flock on `~/.hvx-module.lock`), and every module reload goes through `~/bin/hvx-reload` (exclusive). Use them so that a reload never pulls the module out from under another session's run (M35).
- Regression runner: `hvx/deploy-metal.sh [load] [flagships]` runs from the Mac. It syncs the userspace sources (never `hvx.c`/`hvx_abi.h` unless `load` is given), compiles on the box against the box's `hvx_abi.h`, runs the tests through `hvx-run`, and checks each result against its baseline. Today it reports "ALL PASS (31 checks)". `load` also rebuilds and reloads the main module; `flagships` also runs every real-service verify.
- The main module loaded now is the M68+M69 build: srcversion `CA5BF8BF2A0D66B312A68EF`; its load message ends "freeze in SMM yes".
- Guest images built on the build machine are copied with rsync into the bare-metal machine's `~/hydo/build/` (`AGENTS.local.md` says how). Results of past runs live under `/var/tmp/m<NN>-*` on the bare-metal machine.
- Other sessions use their own trees on the bare-metal machine (for example `~/hydo-tj`, `~/hydo-smp2`, `~/hydo-avx`) so that they do not overwrite each other's binaries. Use your own tree when another session is active.

### The build machine (guest images only)

- The build machine, NixOS, kernel 6.6.35, 24 cores, 31 GB. We have no root there. It is the only machine with nix, so every guest image (`build-linux.sh`, `build-stack.sh`, `build-kafka.sh`, `build-mysql.sh`, `hvx/bench/build-bench*.sh`, `hydo-build`, the Mac guests) is built there and then copied to the bare-metal test machine.
- The pinned toolchain (static musl gcc, busybox, static Redis, `mke2fs`, `libfaketime` and others) is documented in `hvx/BUILD-PINS.md`; GC roots are in `~/hydo-gcroots`. Images built without these pins are not reproducible (M61).
- The disk fills up (340 GB, 99% full on 2026-10-21). Run `df -h ~` before building. One stack image needs about 6 GB at its peak.
- `./dev.sh "<cmd>"` rsyncs the tree to `~/hydo/` on the build machine and runs the command there; it is used for the Rust KVM monitor (Linux only, Rust 1.86; `kvm-ioctls = "0.17"` must pair with `kvm-bindings = "0.8"`).

### The Mac backend

`hvx-mac/` runs deterministic arm64 Linux on an Apple Silicon Mac (an M2 Pro, macOS 26.4.1, no root needed) through Hypervisor.framework: the VMM `hvm.c`, the explorer server `hvmx`, and guest kernels built with `build-guest.sh`. See `hvx-mac/PROBE.md`, `BOOT.md`, `VIRTIO.md` and `hvx-mac/explore/README.md` (M48, M51, M53, M57).

### Removed tooling

The nested-VM (L1) tooling was removed on 2026-10-20: `hvx/run-l1.sh`, `hvx/deploy-hvx.sh`, `hvx/deploy-resume.sh`, `hvx/cloud-init/`, `hvx/kafka-once.sh`, and the `hvx/run-*-demo.sh`/`run-fuzz*`/`run-spread*` runners. Milestones 1–32 in `hvx/HISTORY.md` describe that era. The tooling can be restored from git (`git show 2c04001:hvx/run-l1.sh`) if a nested VM is ever needed again, for example when no bare-metal test machine is available or to test a host without a usable PMU.

## Safety and coordination rules (still in force)

- **Never load a kernel module on the build machine,** and never attempt `sudo`, `insmod` or `rmmod` there. We have no root there, and the host is shared with other people's workloads. If something there needs root, stop and ask the user.
- **The main `hvx.ko` on the bare-metal test machine is shared by several sessions.** Reload it only through `~/bin/hvx-reload` (or `deploy-metal.sh load`), and coordinate with other sessions before doing so. Develop module changes on a side-by-side module with its own device name (for example `hvx/avx/build-hvxavx.sh` gives `/dev/hvxavx`; `race/side-metal.sh` runs the regression suite against a side module). Two modules can be loaded together because each run's VMXON…VMXOFF section is non-preemptible (M40). Merge into the main module only after the side module passes the full suite and the pinned reproducers.
- **After any change to `hvx_abi.h`, rebuild every userspace binary** against the new header. The snapshot struct grew in M68, so binaries built against an older header are wrong.
- **Never run `pkill -f <pattern>` or `pgrep -f <pattern>` through a remote shell when the pattern also matches the remote command line itself.** `pkill` then kills your own session, and a wait loop such as `until ! on_bare_metal 'pgrep -f "<pat>"'; do sleep; done` never finishes. Match with `pgrep -x`, a pid file or a done-marker file instead, or put the `pkill` into a script whose path does not match.
- **Every wait loop needs a timeout,** including the wait loops of agents you start.
- **Build guest images one at a time on the build machine,** and copy each to the bare-metal machine before building the next, because the disk fills up.
- **Delete only your own files.** The boxes are shared by several sessions. For anything else, ask the user and name the candidates. Safe-to-regenerate leftovers are `~/hydo/build/rootfs-*-root` trees and `*-ext4.img` / `*-data.img` intermediates.
- **Keep existing fingerprints stable.** A new module feature must be opt-in and inert when off, so that every recorded baseline is unchanged; check this with `deploy-metal.sh` and the pinned reproducers before and after.

## Current state

### Linux determinism, clocks and the module

- hvx boots real Linux 6.6.35 deterministically: two boots from scratch and every branch-from-snapshot reach a bit-identical full state (console, snapshot, guest RAM, disk). Current boot baselines: no-disk `7a5559028ffbf52c`, with-disk `36583d533d7c989b` (M34). Guest RAM can be up to 3 GiB, below the MMIO hole, and 3 GiB boots are identical; `hvx_bigram_test` checks every page of a 3 GiB guest (M28, M36).
- The default clock counts VM exits: the virtual TSC advances by a fixed step per exit. The **PMU branch clock** (`HVX_VM_PMU_CLOCK`) counts the guest's retired conditional branches in hardware and gives a tick every period (default 2^20 branches, 2 ms of guest time) at an exact instruction (M33). Since M65 the branch counter is frozen while the firmware handles a System Management Interrupt; before that fix, SMIs (which fwupd causes) shifted ticks and caused rare fingerprint differences. Without `tsc=reliable` on the guest command line or the PMU clock, a heavy guest can livelock.
- VM flags (`struct hvx_config.flags`), all off by default: bit 0 `IDLE_HEARTBEAT` (a synthetic tick when the guest idles with nothing armed, M27), bit 1 `X86_64_V2` (M31), bit 2 `PMU_CLOCK` (M33), bit 3 `PAUSE_EXIT` (every `PAUSE` exits and moves the clock, M34), bit 4 `PT_TRACE` (M40), bit 5 `SMP_SCHED` (M66), bit 6 `SMP_BOOT` (M69), bit 7 `X86_64_V3` (AVX/AVX2/FMA/BMI and XSAVE for x87, SSE and AVX only, M68).
- Timing controls for exploring thread interleavings: seeded tick jitter `HVX_SET_TJITTER` (M43), a shorter tick period and inserted extra ticks `HVX_SET_XTICK` (M49), a power cut at the k-th disk FLUSH `HVX_SET_FLUSHCUT` (M56), and the disk's volatile write cache saved and restored through `HVX_GET/SET_UNSYNC` (M43 addendum).
- Code coverage: Intel Processor Trace of the guest (`HVX_SET_PT`, tools in `hvx/pt/`, libipt) gives exact per-instruction coverage; tracing on or off gives identical fingerprints (M40).
- The Rust KVM monitor (`src/`) has its own baselines, `det64` = `ed882eafa4643e09` and `timer64 --timer 200 --insn-clock` = `0b3b7c95eadbcfae`. It cannot get past the `RDTSC` calibration in a Linux boot, because stock KVM does not expose RDTSC exiting to userspace; that is why hvx exists.

### Devices and fault injection

- Devices: a 16550 serial port with input (`HVX_PUT_INPUT`), an emulated x2APIC and IOAPIC, a virtio-blk PCI disk (device 3, MSI-X, a page-array image of up to 4 GiB mapped zero-copy into userspace) and a virtio-net PCI device (device 4, scripted receive with `HVX_PUT_RX`, captured transmit with `HVX_GET_TX`). Every device is inert unless attached (M18–M24, M28).
- Disk faults (`HVX_SET_FAULT`) are a pure function of a seed and the request index: delay, I/O error, lying-disk drop, reorder, torn write, and `DROP_UNFLUSHED`, a faithful power loss in which writes held in a volatile cache are lost unless flushed (M22, M26). Network faults (`HVX_SET_NET_FAULT`) are drop, delay, corrupt, duplicate and reorder (M24).
- Current hand-written test fingerprints (all in `deploy-metal.sh`): iso `7e586473f6e494d7`, pci `3f95ef833747f3ec`, fault `91170d4dbf101c5b` / `88f680fc22cc9bb0` / `9090286f3fc59682`, net `0e2b7acf2f4f94ee`, net-fault `883a00eb6a14a6f0` / `37490fb78da33064` / `40659aabb208aaa0`, bug_f0 `1083e400458728ff`, avx `3bb428a134ce542a`.

### Real-service flagships

Each flagship boots an unmodified service, runs a workload and an oracle, and passes four checks: without faults the result is correct; with the durable setting the data survives a power cut; with the relaxed setting a power cut loses an acknowledged write, and replay and branch reproduce the loss exactly; and different fault seeds give different, reproducible results. The values below are for the original images. The images rebuilt with the pinned toolchain (`*-pin-disk.img`) have their values in `hvx/REBASELINE.md` (M61), and new comparisons should use those.
- hydokv (M25): `62ae248b01bf3f70` / `7ade5f37d16a347c` / `832cc63372b1458d`. SQLite (M26): `1c072036545a7e87` / `7a4ab31fddabe4cc` / `dc04dbe3b49624df`, contrast `2d96a0c788c1fbba`.
- Redis (M27, values from M34): SANITY `634c64b7c583b6df`, F1 `40e98d280d28a492`, F2 `789580e15b179115`, F3 `2ec726ec7c0e50a7` / `c95cb2090b8db26c`. Two services (M30, values from M34): SANITY `e95beaac20a0f91e`, F1 `bd0308f054ca0122`, F2 `fc7d9925ff0997db`, F3 `aed9728c9633817f` / `5c8ec447620131cd`.
- ext4 read-only root (M29, value from M34): `77909a097a8eee08`. JVM with the `c1sync` JIT (M31, M32, value from M34): `025b276a0215a7a6`. The JIT is deterministic under hvx (M32).
- Kafka 3.5.0 (M35, `kafka-disk.img` `ba27661b84dd1e8c…`, PMU clock): SANITY `baae8a071a878505`, F1 `ab40506e09bd7d9b`, F2 `19a591dfde5f04cb`, F3 `d9211628c9dc354f` / `d005d3e6403f454a`. The two `tsc=reliable` configurations also pass; their values are in M35.
- MySQL 8.4.10 (M36, `mysql-disk.img` `404bf87d10ec5422…`, PMU clock): SANITY `e73f52739a634165`, F1 `8be5987266ccb7aa`, F2 `c2ef1c3a1bfd1b86`, F3 `5b5905ac30da7da9` / `8749bd93d306cb05`. The harness sets `HVX_VM_PAUSE_EXIT`.

### The four-service stack

- One guest runs unmodified MySQL 8.4.10, Kafka 3.5.0 and Redis 7.2.4 plus our gRPC order service (`hvx/stack/main.go`; older notes call it the "front door"). Each `Place(id)` writes MySQL, publishes to Kafka with an idempotent franz-go producer, writes Redis as a best-effort cache, and only then acknowledges. Only one `Place` per order id runs at a time, and the "send started" state lives in MySQL (`published` 0 / 2 / 1) (M35, M37, M39, M50). Built by `build-stack.sh`; verified by `hvx/hvx_stack_verify.c` (`mode=all`, `mode=fault`).
- MySQL runs with the binary log on and `sync_binlog=1`, and Kafka with `log.retention.ms=-1` (M63). These two settings fix the bugs found in M58 and M63.
- Current baselines (M77, image `stack-nq-disk.img` `161996bb…` = the M63 recipe plus the M71 netem drain, one vCPU): SANITY `2235f9d76a592a07`, F1 `815cc64346e728fc`, F2 `32024d0b245aff3c` (the expected VIOLATION from MySQL's relaxed window, order 8; durable `7323951e90d21b4f`), F3 `96e2e7ddb963ee66` / `e8c16382540eba2e`. The eight per-fault scenarios (`mode=fault`; kills and netem, all OK 30/30) are in `hvx/REBASELINE.md` §7.2.
- On 2 vCPUs (M79, same image `stack-nq`, `HVX_VCPUS=2`; supersedes the M69 values of `stack-binlog-ret`, which that image still reproduces): SANITY `d1a5ee3ccea87dac`, F1 `7fb7b85c69257f2b`, F2 `809c8292d95b638a` (order 8; durable `233e0afedb35fe4e`), F3 `3a074084d6fbeaa5` / `c008a3f9a3d961e3`; Kafka kill at order 10 (`mode=fault`) `33611b4de52bd93d`, OK 30/30. Details in `hvx/REBASELINE.md` §7.6.

### The explorer

- `hvx/hvx_explore.c` boots a guest once, snapshots it at "root mounted" (S0) and "provisioned" (S1), and starts each schedule in a fresh VM restored from S1; after a power cut, only the flushed data disk goes into a recovery VM restored from S0. `hvx/explore/explore.py` generates schedules from seeds, runs them on a worker pool, re-runs every failure (the fingerprints must match), shrinks one failure per signature, and writes `report.md`, `runs.jsonl` and `shrink.jsonl` (M38). Run it on the box as `cd ~/hydo/hvx/explore && python3 explore.py --seeds 1-200 --out /var/tmp/explore/run1`. Reproduce one schedule as `~/bin/hvx-run sudo ./hvx_explore one <bzImage> <initramfs-kafka> ~/hydo/build/<image> <outdir> "<schedule>"`.
- **Current explorer image:** `explore-binlog-ret-xr-nq-disk.img` (`4a6a8493…`, in `~/hydo/build` on the box, M71: the M67 image plus the netem drain). On it, 40 swarm schedules with `--new-faults --durable-only` give 0 failures. Its predecessor `explore-binlog-ret-xr-disk.img` (`b7975f6b…`, M67) gave 100 OK for `--strategy process --more-faults --durable-only`, seeds 1–100.
- **Pinned reproducers** (check them after every module change): `disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24` gives `2403499c9a3f8419` (a VIOLATION) on `explore-disk.img` (the old order service) and `f87130453609a5d6` (OK) on `explore-fix-disk.img` (M38, M39).
- Strategies: `random` (the default), `swarm` (M42), `swarm-pos` and `process` (a Jepsen-style fault process, the best so far, M56), `portfolio.py` (M44), `guided.py` and `richguided.py` (coverage-guided, negative results, M41, M44), `abranch.py` (negative, M45), and `--sweep kill|cut|net|killcut|pause|clock|oneway|frontkill|tail|depth1|freeze` (`freeze`: every packet point of a traced run as one `F=` schedule, options `--fc-*`, `--sweep-every`, `--sweep-with`, M76). Options: `--timing` (tick jitter, M43), `--new-faults` (pause, clock jump and one-way loss, M46), `--durable-only`, `--concurrency` (concurrent and duplicate clients, M50), `--more-faults` (timed kills `X=` and k-th-flush cuts `cf=`, M67), `--preempt` (M49; on the Mac it gives PCT points, M57), `--buggify` (M56), `--vcpus N --turns` (M69), `--cpu v3` (M68), `--ram-mib` (M59), `--manifest` (M52), `--backend hvm` (the Mac, M51), `--knob-rule` (M56) and `--branch` (M43).
- Schedule tokens. Guest side (after `|`): `K=` kill, `P=` pause, `N=` netem, `O=` one-way loss, `C=` power cut, `T=` clock jump, `Q=` concurrency, `X=` timed kill, `B=` BUGGIFY, and `M=` / `R=` durability knobs. Host side (before `|`): `disk=` and `dseed=`, `tj*` tick jitter, `pp=` / `xt*` preemption, `cf=` flush cut, `rb=` run budget, `q=` / `qs=` turn lengths, and `pct*` (Mac only).
- The main lesson of M41–M57: where and in what combination faults land matters more than how the next schedule is chosen. The fault process, BUGGIFY, and timed kills with flush cuts find what the guided strategies did not.
- Assertions with `always`, `sometimes`, `reachable` and `unreachable` properties (`hvx/ASSERTIONS.md`, the Go SDK in `hvx/stack/hydoassert`, `hydo_assert.sh` for shell, and the report tool `hydo_assert_report`) include `eventually` liveness properties (M38, M59).
- The bug benchmark `hvx/bench/` (M54, M56, M62, M67) has defects d0–d12, each patched into a copy of the order service and each with a confirmed trigger. The process strategy with `--more-faults` finds every defect except d11 in most repetitions; d11 (the M58 window) is found rarely.

### Onboarding projects

- A project is described by one `hydo.yaml` (`hvx/onboard/MANIFEST.md`). `hvx/onboard/hydo-build` compiles it into a guest image with one generic init (`hydo-init.sh`) that starts the services, applies fault tokens by service name and runs the invariants (M47). The `hydo-onboard` skill writes a manifest for a new project.
- Features: services with readiness checks and crash recovery; faults that reach a service's whole process tree (M52); `params:` (M52); `eventually` liveness invariants (M59); Docker/OCI images by digest with `image:` (M59); `guest.ram_mib`, 256–3072 MiB on Linux and 256–4096 MiB on the Mac (M59); per-service `addr:` with link faults between services (M60, x86_64 only); `guest.cpu: v3` (M68, x86_64 only); and `hydo-build --arch arm64` for the Mac (M51).
- Current images of the examples with network or link faults (the M71 netem drain, pinned tools; M77): `explore-ob-stack-{fix,old}-nq`, `explore-ob-miniflux-nq`, `explore-ob-etcd-{3.5.5,3.5.6,3.6.8,3.6.9,3.6.15}-nq`; their fingerprints are in `hvx/REBASELINE.md` §7 and the examples' `RESULTS.md`. On etcd 3.5.5-nq the 14685 bug is still found (17 of 100 explorer schedules); M60's second reproducer no longer fires, but `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1` shows the second symptom (a client's revision going back, `0470512bb297c87f`) and passes on 3.5.6-nq (`63e112769072c0c1`) (M78).

### The Linux release

- `hvx/dist/` packages the bare-metal backend like the Mac release: `build-release.sh` (assets pinned in `assets.sha256`, fetched once from the build machine) gives `hydo-linux-0.1.0.tar.gz` (45 MB); `install.sh` compiles `hvx_explore` and the module on the user's machine; `hydo doctor|module|build|run|repro|explore|report` drive the existing tools, and runs/reloads share the box's `~/.hvx-module.lock` (M75).
- `hydo build` needs no Nix: bundled busybox, static helpers, guest tc/iptables closures and modules, and e2fsprogs + libfaketime run with their own glibc loader. Images are the same bytes on any machine: frozen build clock in UTC+4 (hydo-build's `FAKE_TS` is 1600000000 only there), no host `/etc/mke2fs.conf`, umask 022, root inodes owned by 0:0. Release quickstart on the box: Redis image `8c828d3e…`, clean OK `c513f10aee733aae`, `disk=cut | run C=12 R=everysec` VIOLATION k:2 `ecef6f8fe6c39ec0` (M75).

### Several virtual CPUs

- With `HVX_VM_SMP_SCHED` and the PMU clock, one `HVX_RUN` runs every vCPU in turns of exactly Q guest branches (or seeded lengths), on one shared clock, and stops only at clean points. `HVX_GET/SET_SCHED` carry the scheduler state (M66).
- With `HVX_VM_SMP_BOOT`, real Linux starts the other CPUs with INIT and STARTUP IPIs and boots on 2, 3 and 4 vCPUs, identically across boots and branches: 2 vCPUs `6584c954c2839491`, 3 `e1a2bb0dd744a9ab`, 4 `1dfafae6887e3a01` (`hvx/hvx_smp_boot_verify.c`, M69). Device interrupts are routed by destination.
- Tests in `deploy-metal.sh`: `hvx_sched_pmu_test` (snapshot `73a465516c4e0094`), `hvx_sched_race_test` (grid `8cb7c61a2393f44e`), `hvx_sipi_test` (`eef5c1ebd4df2e7d` / `c8d015ab3b57252d`), `hvx_sched_sweep_test 2000` (`39971eed9c2f35fe`) and the 2-vCPU boot.
- The explorer takes `--vcpus N` and `--turns`, and `hvx_stack_verify` takes `HVX_VCPUS`; so do `hvx/bench/run.py` and `hvx/knownbugs/sqlite/hunt.py`. Measured value so far (M74): on the benchmark's race defects and the SQLite WAL bug, several vCPUs found nothing that one vCPU with concurrent clients or tick jitter misses, at about 3× the cost per run; each turn end costs about 0.8 ms of host time. Write-ups: `hvx/SMP_PLAN.md`, `hvx/SMP_STAGE1.md`, `hvx/SMP_STAGE2.md`, `hvx/explore/TURNS.md`.

### The Mac backend

- Real arm64 Linux 6.6.35 boots bit-identically on the M2 (boot `81711ada76a4a236`), also under full host load and from branches (M48). The guest kernel needs `CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT` (`hvx-mac/linux/force-cntpct.patch`), LSE atomics and `CONFIG_ARM64_HVM_IDLE_HVC` (M48, M51).
- It has virtio-mmio disk and network devices with the same fault model as hvx (with delays counted in exits), runs Redis and miniflux with PostgreSQL, and serves the explorer (`hvmx`, `explore.py --backend hvm`). The Redis `disk=none | run` baseline is `ca13bcd1d77e2b4b` (M51, M53).
- Tick jitter and PCT-style forced preemption points are counted in VM exits (M53, M57), because the M2 has no exact guest instruction counter.
- The three M51 follow-ups are merged (git history): network faults `N=`/`O=` via tc + netem in the arm64 guest (`5fa2f06`), the spin observer in `hvm`/`hvmx` with automatic escalation to rewind (`3b2a0bb`), and the self-contained release (`hvx-mac/dist/`, `feffce6`). The release was rebuilt on 2026-10-23: `hvx-mac/dist/out/hydo-mac-0.1.0.tar.gz`, sha256 `9df88bc145cbefcf…` (rebuilt again the same day to ship all init extensions — `hydo-liveness.sh`, `hydo-init-clock.sh`, `hydo-init-links.sh`, `hydo-init-freeze.sh` — without which an arm64 build of a manifest with `eventually` invariants or `guest.clock_after_cut` failed; checked by building and running such a manifest from an installed release); every asset matched `assets.sha256`; its quickstart verdicts held, each schedule twice identical (fingerprints differ from `hvx-mac/explore/README.md`, which was measured on Linux-built images). Its cached inputs are in `hvx-mac/dist/assets/` (git-ignored), so a rebuild works offline.

### Bugs found so far

- In our own order service: a failed cache write failed an order that was already published (M38, fixed in M39); and a duplicate under overlapping retries, a panic on the shared Redis connection, and a crash that forgot which sends had an unknown outcome (M50, all fixed).
- In the stack configuration: with the MySQL binary log off, a commit was visible before its redo fsync finished, found only by the k-th-flush cut (M58, fixed by the binary log in M63); and Kafka's default 7-day retention deleted acknowledged orders after a large forward clock jump (M63, fixed with `log.retention.ms=-1`).
- Upstream: etcd 3.5.5 issue 14685 (member revisions diverge after an online defrag and a crash), found without being told and absent in 3.5.6 (M60); both symptoms reproduce on the `-nq` image (M77, M78).
- Not reproduced: the SQLite 3.51.2 WAL-reset corruption (M52, M49).

## Open items

Items marked **[user decision]** need the user before anyone acts on them.

2. **[user decision]** The single-Kafka image (`build-linux.sh` §2l) and `hvx/onboard/examples/stack/hydo.yaml` still use Kafka's default retention, and `build-mysql.sh` and the onboarding stack example still use `skip-log-bin`. Fixing them moves unrelated baselines, so they were left unchanged (M63).
3. **[user decision]** The build machine disk keeps filling up; cleaning other sessions' build outputs needs the user's approval.
4. Several vCPUs (M69): Linux marks the TSC unstable with 2 vCPUs (`tsc=reliable` removes the message). Each PAUSE exit moves the virtual clock, so guest time depends on the PAUSE setting. NMIs between CPUs, logical-destination interrupt routing, virtio-net routing beyond the first deliverable queue, paravirtual lock help and more than 4 vCPUs are not done. `hvx/explore/TURNS.md` lists module wishes: a per-turn length list, a short/long mix and a starve mode.
5. Guest AVX (M68): there is no AVX-512, XSAVEOPT, XSAVEC, XSAVES or PKRU; the pending #GP of an invalid XSETBV is not in the snapshot; and with the flag off, a guest that sets CR4.OSXSAVE anyway reads the host's XCR0.
6. The d11 defect (an ACK on an unflushed flag) has a window of about 3 of ~310 flush-cut points and is rarely found by strategies (M67, M72: 3/6 then 0/6 over 3-repetition cells); measuring its detection rate needs many more repetitions or a cell that always draws `cf=`. The m59 bench set now defaults to `-pin-kr` (Kafka retention fixed, M72).
7. fwupd still runs on the bare-metal machine; it is harmless to the fixed module. Older single outliers (M34 MySQL, M39 I/O error) were not checked against fwupd times (M65).
8. etcd 3.5.5: 14685's second symptom (a client's revision going back) is reproduced again on the `-nq` image by `K=etcd1@10+1 P=etcd3@26+1` (M78), found by a hand sweep, not by the explorer. Why etcd1's one-revision lead appears only together with the later pause of another member was not investigated.
9. Explorer and tools: the oracle reads Kafka before it restarts when Kafka is killed at the last order (M55); `pp=`/`xt=` are refused in branch mode, and the `xt` shrinker does not turn a seeded policy into a list (M49); the guest RAM size is not part of the schedule string, so a hand-run `hvx_explore one` on a non-default image needs `--ram-mib` (M59); the OK verdict shows only the first invariant's details (M47).
10. etcd 3.6.8 against 3.6.9 (stale reads with a paused leader) showed no difference; it needs timed pauses aimed at the leader. `addr:` is refused on arm64 (M60).
11. Images built before M71 drop the packets queued in netem when a delay fault is lifted. The onboarding examples and the default stack were rebuilt as `-nq` images in M77; any other image with network faults that predates M71 keeps the old behaviour.
12. Processor Trace limits: guest kernel modules are not traced, very early boot needs tracing started after the kernel's self-patching, JIT-compiled Java is out of scope, buffers of about 1 GiB can fail to allocate, and host PT use is suspended during traced runs (M40). Whole-system coverage runs often filled the 256 MiB buffer (M44).
13. Mac backend: a pure memory-flag spin in user space with no exits cannot be interrupted deterministically, and windows of plain work with no exits are out of reach of jitter and PCT points (M48, M53, M57).
14. Older limits that still hold: the PMU clock is a tick source on top of the exit-count clock, not an instruction-exact clock, so instruction-exact bisection of a Linux boot is future work (M33); host performance counters are off while a PMU-clock guest runs (M34); RAM above about 3.9 GiB needs a PCI-hole split (M28); a read-write ext4 root is not done (M29); and in the module-built default guest page tables, the page that maps 512 MiB–1 GiB shares a page with the built-in interrupt handler, which only hand-written test guests use, left as is because fixing it moves every hand-written fingerprint (M36).
15. Images listed but not rebuilt with the pinned tools: `explore-cc0/1/cc` (M50; rebuild from `029e4b3`/`b9233d5`) and `explore-dx`, an undocumented diagnostic (M61). The build machine copy of `richcov-disk.img` was built with a substitute gcc and must be rebuilt before use (M44).
16. Linux release (M75): runs need `sudo` (no udev rule for `/dev/hvx`); the Mac release copies only `hydo-init.sh` of the init extensions, so an arm64 build of a manifest with `eventually` invariants or `guest.clock_after_cut` probably fails; plain `hydo-build` on the Nix build box gives different image bytes when run in a time zone other than UTC+4 (its `FAKE_TS` is local time).

## Milestone index (details in `hvx/HISTORY.md`)

Milestones 1–32 ran inside the nested L1 VM on the build machine, before 2026-10-13 (M17 is dated 2026-08-12).

- M1 — VMXON and VMLAUNCH of a trivial guest in our own module
- M2 — EPT: the guest in its own physical address space
- M3 — `/dev/hvx`, a userspace mini-KVM interface
- M4 — deterministic RDTSC interception
- M5 — the MTF single-step instruction clock
- M6 — RDTSC tied to the instruction clock
- M7 — snapshot and branch-from-snapshot
- M8 — instruction-timed interrupt injection
- M9 — canonical fingerprint and verify harness
- M10 — I/O-port trap and serial console
- M11 — guest RAM beyond 2 MiB
- M12 — deterministic CPUID and MSR emulation
- M13 — privileged-state persistence and `HVX_SET_SREGS`
- M14 — first real Linux serial output as L2
- M15 — real Linux boots to the initramfs shell
- M16 — whole-boot determinism and branch on real Linux
- M17 — PMU clock blocked on the hybrid build machine CPU (2026-08-12; resolved in M33)
- M18 — serial input and an interactive shell
- M19 — deterministic multi-vCPU foundation on hand-written guests
- M20 — deterministic virtio-PCI block device
- M21 — real Linux uses the disk over MSI-X
- M22 — deterministic disk fault injection
- M23 — deterministic virtio-net device
- M24 — deterministic network fault injection
- M25 — flagship: a durability bug in hydokv found by a seeded fault
- M26 — flagship on unmodified SQLite
- M27 — flagship on Redis: the AOF durability window
- (no number) — fs-image byte-reproducibility hardening
- M28 — page-array disk, disk mmap, RAM to 3 GiB
- M29 — ext4 read-only root via switch_root, snapshot at scale
- M30 — a two-service system over loopback
- M31 — deterministic OpenJDK; Kafka hit a wall
- M32 — the JIT is deterministic; file-backed snapshot
- M33 — bare metal, Kafka flagship, PMU branch clock (2026-10-13)
- M34 — MySQL flagship, pinned Kafka image, seven hvx fixes (2026-10-13)
- M35 — the four-service stack with one cross-service oracle; `deploy-metal.sh` (2026-10-14)
- M36 — service crashes and network faults on the stack; RAM to 3 GiB verified (2026-10-14)
- M37 — the order service made retry-safe and exactly-once (2026-10-14)
- M38 — automatic fault explorer and assertions; first bug found without a hand-picked scenario (2026-10-14)
- M39 — the explorer's bug fixed and the fix proven by the explorer (2026-10-15)
- M40 — exact guest code coverage with Intel Processor Trace (2026-10-15)
- M41 — coverage-guided exploration, negative (2026-10-16)
- M42 — swarm testing and exhaustive sweeps (2026-10-16)
- M43 — timing exploration: tick jitter and mid-run branching (2026-10-16)
- M44 — coverage growth, richer signal, portfolio scheduler, mostly negative (2026-10-17)
- M45 — branch points from assertions, negative (2026-10-17)
- M46 — pause, clock-jump and one-way-loss faults, no new bug (2026-10-17)
- M47 — onboarding v1: one `hydo.yaml` per project (2026-10-18)
- M48 — deterministic arm64 Linux on the Mac (2026-10-18)
- M49 — finer preemption: shorter tick period and inserted ticks (2026-10-18)
- M50 — concurrent clients find three bugs in the order service (2026-10-18)
- M51 — the Mac backend explores real projects (2026-10-18)
- M52 — onboarding follow-ups; the SQLite known bug not reproduced (2026-10-18)
- M53 — tick jitter on the Mac, counted in VM exits (2026-10-18)
- M54 — an 8-defect bug benchmark; five strategies compared (2026-10-19)
- M55 — why d7 was missed; end-of-workload sweep; fault-search literature (2026-10-20)
- M56 — fault process, BUGGIFY, depth-1 sweep, cut at the k-th flush (2026-10-21)
- M57 — PCT-style forced preemption points on the Mac (2026-10-21)
- M58 — a real bug found by the flush cut; a harness stop-point bug fixed (2026-10-21)
- M59 — onboarding: guest RAM, liveness properties, Docker/OCI images (2026-10-21)
- M60 — three-node etcd; a known upstream bug found without being told (2026-10-22)
- M61 — re-baseline on the pinned toolchain (2026-10-22)
- M62 — benchmark defects d8–d12; an explorer gap (2026-10-22)
- M63 — MySQL binary log on; Kafka retention after a clock jump fixed (2026-10-22)
- M65 — a rare fingerprint gap (a clean schedule of one example gave a different fingerprint in 1 of 5–10 runs) was SMIs counted on the branch clock; fixed (2026-10-22)
- M66 — several vCPUs, stage 1: exact branch-counted turns (2026-10-22)
- M67 — strategies draw timed kills and k-th-flush cuts; d10 and d11 found (2026-10-22)
- M68 — opt-in guest AVX/AVX2, x86-64-v3 (2026-10-22)
- M69 — several vCPUs, stages 2 and 3: Linux on 2–4 vCPUs, the stack and the explorer on 2 vCPUs (2026-10-22)
- M70 — freeze a service at an exact packet of its traffic (`F=`) (2026-10-23)
- M71 — lifting a network fault drains the netem queue instead of dropping it (2026-10-23)
- M72 — benchmark images with the Kafka retention fix (`-pin-kr`), now the default m59 set (2026-10-23)
- M73 — M70 follow-up: the recovery boot continues the first boot's clock; freeze measurement (2026-10-23)
- M74 — negative: several vCPUs found no race that one vCPU misses (benchmark d8/d9/d12; SQLite WAL bug not found on 2 or 4 vCPUs) (2026-10-23)
- M75 — the Linux release: one `hydo` command (doctor, module, build, run, repro, explore, report) and a self-contained tarball for x86_64 + VT-x, no Nix needed (2026-10-23)
- M76 — the freeze sweep: every packet point of a run as one `F=` schedule (2026-10-24)
- M77 — onboarding and default stack images rebuilt with the netem drain (`-nq`); stack baselines and the eight fault scenarios re-measured (2026-10-23)
- M78 — etcd 14685's second symptom (a client's revision going back) reproduced again on the 3.5.5-nq image; passes on 3.5.6-nq (2026-10-24)
- M79 — the 2-vCPU stack baselines re-measured on `stack-nq` (2026-10-24)

## Where to look

- `hvx/hvx.c` (the module), `hvx/hvx_abi.h` (the ioctl ABI shared with userspace), `hvx/hvx_boot.h` (the bzImage loader recipe), and the `hvx/hvx_*_test.c` and `hvx/hvx_*_verify.c` tests and harnesses.
- `hvx/explore/` (the explorer), `hvx/stack/` (the order service and the assertion SDK), `hvx/bench/` (the bug benchmark), `hvx/onboard/` (manifests), `hvx/pt/` (Processor Trace tools), `hvx/race/` (race calibration and side-module scripts), `hvx/cutflush/` (flush-cut findings), `hvx/tdet/` (SMI tools).
- `build-linux.sh`, `build-stack.sh`, `build-kafka.sh`, `build-mysql.sh` (guest images), `hvx/BUILD-PINS.md`, `hvx/REBASELINE.md`.
- `src/` (the Rust KVM monitor: `vmm.rs`, `state.rs`, `pmu.rs`, `detlinux.rs`, `boot.rs`).

## Suggested skills for the next session

- `run` — to launch/re-verify hydo on the remote after changes.
- `code-review` / `simplify` — for reviewing Rust changes (the determinism engine is subtle; keep `verify` + the baseline fingerprints above passing).
- No special skill exists for kernel/VMX work — use Intel SDM Vol 3C (VMX chapters), the KVM source, and "Hypervisor From Scratch" as references.

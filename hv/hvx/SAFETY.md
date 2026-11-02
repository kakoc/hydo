# The safety net of the hvx kernel module (Milestone 97)

`hv/hvx/hvx.c` stays in C (language policy in `AGENTS.md`). This is what checks it instead of a
memory-safe language: a debug test kernel that turns memory errors, undefined behaviour, lock
misuse and leaks into loud reports, and a coverage-guided fuzzer (syzkaller) of the `/dev/hvx`
ioctl interface and of the device models a guest program drives. Everything lives in
`hv/hvx/safety/`; `hv/hvx/safety/safety.sh` drives it from the Mac.

## Where things run

| step | machine | why |
|---|---|---|
| build: debug kernel, `hvx.ko` for it, tests, syzkaller, fuzzing image | the bare-metal machine (`~/hydo-safety`, nice 19, 6 jobs, paused while `/var/tmp/hydo-QUIET` exists) or the build machine | building is safe anywhere |
| run: the debug VM, the syzkaller VMs | the build machine only (`~/hydo-safety`) | needs KVM with nested VMX; on the bare-metal machine KVM would turn VMX on next to the shared `hvx.ko` |
| prove a fix inert | the bare-metal machine, side module `/dev/hvxsafe` | real hardware, recorded fingerprints |

Never start a KVM guest on the bare-metal machine. Boot it into a debug kernel only with the
user's approval, and only by the on-box method below (Milestone 140). On the
build machine, check `free -m` first: the VM steps refuse to start unless enough memory is
available (`campaign.sh` waits up to `WAIT_MIN` minutes and stops its VMs if available memory
falls below `MEM_FLOOR`), because other people's jobs share that host.

## The debug kernel

Linux 6.8.12 (the bare-metal machine runs Ubuntu's 6.8), `defconfig` + `kvm_guest.config` +
`hv/hvx/safety/kernel/hvx-debug.config`: KASAN (generic, inline, vmalloc), kmemleak, UBSAN
(bounds, shift, bool, enum), PROVE_LOCKING, DEBUG_ATOMIC_SLEEP, DEBUG_PREEMPT with full
preemption, DEBUG_LIST, DEBUG_VM, HARDENED_USERCOPY, FORTIFY_SOURCE, the soft-lockup, hung-task
and RCU-stall detectors, KCOV with comparisons (for the fuzzer), and fault injection (failslab,
fail_page_alloc, usercopy). KVM is off, so `hvx.ko` owns VMX in the VM. `build-kernel.sh`
pins the tarball by sha256 and fixes the build identity (`KBUILD_BUILD_*`), and it fails if
any option of the fragment did not survive Kconfig. A rebuild with the same compiler gives the
same `bzImage`; the bare-metal machine (Ubuntu gcc 13.3) and the build machine (nixpkgs gcc 13.2) give different
bytes.

```
safety.sh build          # on the bare-metal machine: kernel, bundles "debug" (this tree's hvx.c)
                         # and "debug-orig" (main's hvx.c), syzkaller, fuzzing image
safety.sh push           # box -> Mac -> build machine
safety.sh debug          # boot it, run every test, print PASS/SKIP/FAIL and every splat
safety.sh debug hvx_repro_txlen,hvx_virtio_pci_test     # a subset
```

`run-debug.sh` boots the kernel with an initramfs (`debug-init.sh` is its `/init`), loads
`hvx.ko`, runs the deploy-metal suite (hand-written tests, several-vCPU tests), a real Linux
boot under hvx (`hvx_linux`, `hvx_boot_verify`, `hvx_disk_boot_verify`) and the reproducers
(`hvx_repro_*`), unloads the module, scans kmemleak, and attributes each splat to the test that
was running. Exit status 0 means no failure and no splat. With `DEBUG_OUT=debug-orig` it boots
the unfixed module instead, which is how each reproducer was shown to fail before its fix.
`hvx_repro_pageleak` checks that 200 VM lifecycles give their pages back (kmemleak does not
see pages from `__get_free_page`). `hvx_repro_bigalloc` (a 3 GiB VM and a 4 GiB disk) is not in
the default list because it needs a VM smaller than 3 GiB:
`VM_MEM=2048 run-debug.sh hvx_repro_bigalloc`.

What the nested VM cannot cover: the build machine CPU is hybrid and KVM gives a nested guest no PMU
(M17), so `HVX_VM_PMU_CLOCK` and everything on it (the several-vCPU scheduler, tick jitter,
extra ticks, turn control, the 2-vCPU boot) fails with ENODEV there and is reported as SKIP;
Intel PT is not available nested either. Those paths are checked only by the bare-metal suite.

## The fuzzer

syzkaller (pinned commit in `syz/build-syzkaller.sh`, Go in `~/hydo-safety/go`) with
`syz/dev_hvx.txt`: every `/dev/hvx` ioctl with structured arguments (the full snapshot, the
scheduler, unsync, fault and timing policies, disk and network attach), edge-size variants,
the RAM and disk mmap windows, and the pseudo-syscall `syz_ext_hvx_guest`
(`syz/common_ext.h`, syzkaller's extension point) that assembles a guest program from
structured operations (port I/O, MMIO at the virtio BARs and the IOAPIC, MSRs, CPUID, PCI
config cycles, string I/O, control registers, single instructions, raw x86-64 text from
syzkaller's generator) plus virtqueue rings and descriptors in guest RAM, so `HVX_RUN` reaches
the exit handlers and device models. The constants come from `hvx_abi.h` (`syz/gen-const.c`),
and `syz/check-sizes.go` fails the build if a described struct differs in size from the
header's.

```
safety.sh smoke [MIN]        # bounded smoke run, 2 VMs (default 20 min); after every module change
safety.sh campaign HOURS [N] # long run in the background, N VMs (default 4), pid file
safety.sh status | stop      # crash titles, corpus and coverage; stop by pid file
```

Each fuzzing VM has 2 vCPUs and 3 GiB (`VM_MEM`; KASAN reserves about 430 MiB of it), and the
fuzzer keeps guest RAM at 8 MiB or less, because a program may hold many VMs at once; with
2 GiB VMs and 32 MiB guests the VMs ran out of memory every few minutes. `BUNDLE=debug-orig`
fuzzes the unfixed module of `main` instead (it finds the `HVX_GET_PMU` crash within a minute,
which is a quick check that the fuzzer works). Before a campaign starts, `campaign.sh` waits
for `VM count × (VM_MEM + 512) + MEM_RESERVE` MiB to be available, and its guard stops the
campaign when less than `MEM_FLOOR` MiB remain.

Results are in `~/hydo-safety/campaign/` on the build machine (`long/` or `smoke-*/`, each with
`manager.log`, `crashes/<id>/{description,log*,report*,repro.*}`); the web UI listens on
`127.0.0.1:56741` there (reach it through a forwarded port).

Why syzkaller and not a fuzzer of our own: it already does KCOV coverage feedback, VM
management, crash detection and deduplication, reproduction and minimisation to a C program,
and it handles races between threads; a structure-aware fuzzer of our own would have to rebuild
all of that. The one thing it lacks for a hypervisor — guest programs that reach the device
models — is the `syz_ext_hvx_guest` extension above.

## Proving a fix inert

`hv/hvx/safety/side-check.sh` (or `safety.sh side`) builds this tree's `hvx.c` as `hvxsafe.ko`
(`/dev/hvxsafe`) on the bare-metal machine, loads it next to the shared `hvx.ko`, and runs the whole
deploy-metal suite (31 checks) and the two pinned explorer reproducers against it, one VM at a
time through `~/bin/hvx-run`, with the devshim LD_PRELOAD. `side-check.sh unload` removes it.
Do not run `hvx_repro` on the bare-metal machine: against an unfixed module several cases oops the kernel or
hold a CPU with interrupts off.

## The debug kernel on the bare-metal machine itself (Milestone 140)

The nested VM above cannot reach the PMU clock, several vCPUs, Intel PT or the real CPU's EPT
accessed/dirty flags, and it needs the build machine. `hv/hvx/safety/onbox/` instead builds a debug
kernel from the bare-metal machine's own running configuration and boots the bare-metal machine into it once, so `hvx.ko` and the
whole suite run under KASAN, kmemleak, UBSAN and lockdep on the real hardware. Each such boot needs
the user's approval.

- **The kernel.** kernel.org Linux 6.8.12 (the tarball pinned above), configured from
  `/boot/config-6.8.0-146-generic`, trimmed to the loaded modules by `make localmodconfig` (it keeps
  ahci, md raid1, igb and ext4, and `onbox.sh build` fails if one is missing), plus
  `onbox/debug.config`: KASAN (generic, inline, vmalloc), kmemleak (scanned on demand), UBSAN
  (bounds, shift, bool, enum; no trap), PROVE_LOCKING with larger lockdep tables (a whole Ubuntu
  system runs on it), DEBUG_ATOMIC_SLEEP, full preemption with DEBUG_PREEMPT, DEBUG_LIST, DEBUG_VM,
  hardened usercopy, the lockup, hung-task and workqueue detectors, EFI pstore (a panic's log
  survives the reboot), no KVM, `LOCALVERSION=-hydodbg`. Module signing stays on (the lockdown LSM
  selects it; the build makes its own key; signatures are not enforced).
- **Where it lives.** Everything is in your own tree (`~/hydo-dbg/kbuild`, `~/hydo-dbg/boot`) except
  the modules, which `modules_install` puts in `/lib/modules/6.8.12-hydodbg`. Nothing goes into
  `/boot`, so `update-grub` never lists the debug kernel and the next Ubuntu kernel update cannot make
  it the default.
- **Why kexec and not `grub-reboot`.** `/boot` is on md RAID (md2), and GRUB cannot write its
  environment block there (the generated `grub.cfg` says "GRUB lacks write support for diskfilter").
  `grub-reboot` relies on GRUB clearing `next_entry` with `save_env` at boot; on this box that write
  fails, so the entry would be booted on every reboot, and a hang would never fall back. `kexec`
  starts the debug kernel from the running one without GRUB; every later reboot (a plain one, a
  panic, which reboots after 30 s because of `panic=30`, a soft lockup through `softlockup_panic=1`,
  or the provider's panel after a hang) goes through the firmware and GRUB's saved default. Ubuntu
  24.04's kexec-tools does not take over normal reboots (no `LOAD_KEXEC`, no init script).
- **GRUB's default.** The bare-metal machine had a newer Ubuntu kernel installed but never booted (6.8.0-147, which
  `/boot/vmlinuz` and GRUB's entry 0 pointed at), so a plain reboot would not have returned to the
  kernel the bare-metal machine runs. `onbox.sh grub` sets `GRUB_DEFAULT=saved`, runs `update-grub` and
  `grub-set-default 'gnulinux-advanced-<uuid>>gnulinux-6.8.0-146-generic-advanced-<uuid>'`
  (written from Linux, which can write the RAID); `grub.cfg` then selects `${saved_entry}`. Moving
  the bare-metal machine to a newer kernel is a separate decision: `grub-set-default` with that kernel's entry.

```
onbox.sh build       # on the bare-metal machine, in ~/hydo-dbg: about 30 minutes at nice 19 with 6 jobs
onbox.sh install     # modules, bzImage and initramfs; checks md raid1, mdadm, ahci, igb, ext4
onbox.sh grub        # GRUB_DEFAULT=saved, saved_entry = 6.8.0-146 (prints the checks)
onbox.sh load        # kexec -l with the running command line + panic=30 softlockup_panic=1 kasan_multi_shot
                     # write ~/m140-reboot-pending, give the other sessions 15 minutes, then:
GO=1 onbox.sh go     # exclusive module lock, /dev/hvx closed, systemctl kexec
onbox.sh module      # under the debug kernel: build this tree's hvx.ko for it and load it as hvx
debug-chain.sh       # the checks (deploy-metal runs from the Mac first: HVX_METAL_TREE=hydo-dbg)
onbox.sh scan <tag>  # kmemleak scan and every report line of dmesg, in /var/tmp/m140a
sudo reboot          # back to GRUB's saved default
```

`onbox/hvx_m138_edge_test.c` drives the edges of Milestone 138's code (the host-call port's
records, overflow, bad commits and staged bytes, the six new ioctls' refusals, the dirty log and
`HVX_RESET_VM`, 50 VM lifecycles) for the debug kernel to watch; syzkaller does not know those
ioctls yet.

# The safety net of the hvx kernel module (Milestone 97)

`hvx/hvx.c` stays in C (language policy in `AGENTS.md`). This is what checks it instead of a
memory-safe language: a debug test kernel that turns memory errors, undefined behaviour, lock
misuse and leaks into loud reports, and a coverage-guided fuzzer (syzkaller) of the `/dev/hvx`
ioctl interface and of the device models a guest program drives. Everything lives in
`hvx/safety/`; `hvx/safety/safety.sh` drives it from the Mac.

## Where things run

| step | machine | why |
|---|---|---|
| build: debug kernel, `hvx.ko` for it, tests, syzkaller, fuzzing image | the bare-metal machine (`~/hydo-safety`, nice 19, 6 jobs, paused while `/var/tmp/hydo-QUIET` exists) or the build machine | building is safe anywhere |
| run: the debug VM, the syzkaller VMs | the build machine only (`~/hydo-safety`) | needs KVM with nested VMX; on the bare-metal machine KVM would turn VMX on next to the shared `hvx.ko` |
| prove a fix inert | the bare-metal machine, side module `/dev/hvxsafe` | real hardware, recorded fingerprints |

Never boot the bare-metal test machine into the debug kernel, and never start a KVM guest there. On the
build machine, check `free -m` first: the VM steps refuse to start unless enough memory is
available (`campaign.sh` waits up to `WAIT_MIN` minutes and stops its VMs if available memory
falls below `MEM_FLOOR`), because other people's jobs share that host.

## The debug kernel

Linux 6.8.12 (the bare-metal machine runs Ubuntu's 6.8), `defconfig` + `kvm_guest.config` +
`hvx/safety/kernel/hvx-debug.config`: KASAN (generic, inline, vmalloc), kmemleak, UBSAN
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

Results are in `~/hydo-safety/campaign/` on the build machine (`long/` or `smoke-*/`, each with
`manager.log`, `crashes/<id>/{description,log*,report*,repro.*}`); the web UI listens on
`127.0.0.1:56741` there (reach it through a forwarded port).

Why syzkaller and not a fuzzer of our own: it already does KCOV coverage feedback, VM
management, crash detection and deduplication, reproduction and minimisation to a C program,
and it handles races between threads; a structure-aware fuzzer of our own would have to rebuild
all of that. The one thing it lacks for a hypervisor — guest programs that reach the device
models — is the `syz_ext_hvx_guest` extension above.

## Proving a fix inert

`hvx/safety/side-check.sh` (or `safety.sh side`) builds this tree's `hvx.c` as `hvxsafe.ko`
(`/dev/hvxsafe`) on the bare-metal machine, loads it next to the shared `hvx.ko`, and runs the whole
deploy-metal suite (31 checks) and the two pinned explorer reproducers against it, one VM at a
time through `~/bin/hvx-run`, with the devshim LD_PRELOAD. `side-check.sh unload` removes it.
Do not run `hvx_repro` on the bare-metal machine: against an unfixed module several cases oops the kernel or
hold a CPU with interrupts off.

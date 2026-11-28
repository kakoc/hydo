# Every guest thread's stack at a stop (Milestone 383)

Status: a first version on hvx (Milestone 383, tentative number). The Mac and KVM engines answer
the new engine command with a refusal. [What each engine needs](#what-each-engine-needs) says
what they lack.

A service hangs under one schedule. Its processes stop with no error, and its log just stops.
The run replays the same way each time, but nobody can say which lock the threads wait on. The
guest has no debugger, and a debugger in the image would change the image and its timing. The
question a maintainer asks is: which lock, held by which function?

hydo is the hypervisor. At every stop it holds every vCPU's registers and all of guest memory.
`hydo inspect stacks` answers the question from those alone, without a change of the guest:

```sh
hydo inspect start <run dir> --at end          # a session at the stop where the verdict was given
hydo inspect stacks <session> --text           # every thread's stack, the locks, the cycles
hydo inspect stacks <run dir> --at run:400     # one call: replay to the stop, read it, end
hydo inspect stacks <session> --follow --steps 5 --ms 500   # the stacks, then advance, 5 times
hydo inspect stacks <run dir> --check-run --every 25        # the proof that reading moves nothing
```

## Decisions

1. **The engine only writes the stop's state; the host reads it.** A new command of the
   engines' lock-step protocol, `DUMP <path>`, writes every vCPU's registers and the guest's RAM
   to a file (the format is in [The dump](#the-dump)). The engine reads its own state and writes
   nothing into the guest, so the guest, its clock and the run's stops are as they were. All the
   rest runs on the host, in `hydo_contracts::stacks`, as pure functions of the file: they are
   tested with memory images made in the tests. A new engine needs only the dump.
2. **The kernel's symbols come from a file kept beside the pinned kernel; its layouts come from
   the kernel itself.** The task list starts at `init_task`, and its offsets are those of the
   running kernel's `struct task_struct`. hvx's guest kernel is nixpkgs's `linux-6.6.35` with
   `CONFIG_DEBUG_INFO_BTF`, so the kernel keeps its own type information (BTF, 4.6 MB) in its
   image, between `__start_BTF` and `__stop_BTF`. hydo reads it from guest memory, so no
   offset is written down by hand and a wrong table cannot be used. The symbols are nixpkgs's
   `System.map` of the same build (161,287 lines), kept as `<kernel>.System.map` beside the
   kernel. Reading `kallsyms` from guest memory was the other way: it needs no file, but finding
   the kernel's compressed symbol tables without a symbol is a search of the whole image by
   pattern, which a new kernel version can break. The map is checked against the guest: the name
   of `init_task` must read `swapper/0`.
3. **Threads come from the kernel's task list.** hydo walks `init_task.tasks` (the thread
   groups) and each group's `signal->thread_head` (its threads). For each thread it reads the
   pid, the tid, `comm`, `__state`, the program (`mm->exe_file`'s path, from its dentries), the
   user registers, and the system call the thread is in. A thread that a vCPU runs (the vCPU's
   `pcpu_hot.current_task`) has the vCPU's registers. Any other thread has the registers the
   kernel saved at the top of its kernel stack (`struct pt_regs`), and its kernel stack starts at
   the frame `__switch_to_asm` left (`thread.sp`, `struct inactive_task_frame`).
4. **Addresses are translated by each process's own page tables.** A process's top table is
   `mm->pgd`. The kernel's image and its direct map of RAM are translated by their fixed offsets
   (`phys_base`, `page_offset_base`, read from the kernel), and the rest, such as the
   virtually mapped kernel stacks, by `init_top_pgt`. The walk handles x86_64's four levels with
   2 MiB and 1 GiB pages, and arm64's four levels with 4 KiB pages and blocks, tested on tables
   made in the tests.
5. **A stack is unwound from what the program keeps.** In this order: Go's table of frame sizes
   (`.gopclntab`, which a stripped Go program keeps too); the call frame information
   (`.eh_frame`, which Rust and C programs have); frame pointers. The kernel's stacks use the
   kernel's ORC tables (`CONFIG_UNWINDER_ORC`, in its image). A kernel without them gets the
   kernel text addresses found on the stack, marked `scan`. Each frame says how it was found
   (`regs`, `go`, `cfi`, `fp`, `orc`, `scan`).
6. **Frames are named from the image's own files.** `hydo build` keeps the image's root tree
   beside the image (`<name>-root`), so the guest path of a process's program is a file on the
   host. Its symbols name the function (Rust's names demangled), its DWARF line tables or Go's
   table give the file and the line. `--program GUEST=HOST` names another copy.
7. **A Go program's goroutines come from its runtime.** The runtime's own structures say what
   each goroutine waits for: `runtime.allgs`, each `runtime.g`'s status, wait reason
   (`runtime.waitReasonStrings`) and saved registers (`sched`). For a goroutine parked on a
   semaphore it is the `sudog`'s `elem`, the semaphore's address. The offsets come from the
   program's DWARF. A Go program's threads park on the runtime's own futexes, so for Go the
   locks are the goroutines'.
8. **A lock is an address that someone waits on.** A thread in `futex` with a wait operation
   waits on its first argument. A goroutine in `sync.Mutex.Lock` waits on the mutex whose
   semaphore its `sudog` names. hydo names the address by the program's global variables
   (`main.accountsMu`, `bank::ACCOUNTS+0x20`).
9. **The holder is a likely one, and the answer says why.** Neither a futex nor Go's mutex
   records its owner. hydo takes as a likely holder another thread or goroutine with a frame
   whose function refers to the lock's variable (on x86_64, a 32-bit `rip`-relative
   displacement in the function's code that points into the variable) and has not returned. When
   no function refers to it, a stack slot that keeps the lock's address counts. The waits and the
   likely holders make a graph; a cycle in it is a deadlock, and the answer lists each wait of the
   cycle.
10. **Reading a stop moves nothing, and a command proves it.** `hydo inspect stacks <run dir>
    --check-run --every N` replays the whole recorded run with a dump at every N-th stop and
    compares the replay's fingerprint with the recorded run's. Equal values mean that the guest's
    RAM, disk, registers and console ended the same with the reads and without them.

## The dump

```text
HYDODUMP 1
{"arch":"x86_64","engine":"hvx","vcpus":[{"rip":...,"rsp":...,"cr3":...,"cs":...,...}],"ram":[{"gpa":0,"len":...,"off":4096}]}
(zero bytes up to the first range's offset)
the guest's RAM, range after range, each at its offset
```

A page of zeros is a hole of the file, so a dump takes the disk space of the RAM the guest uses.
hvx's RAM above 3 GiB is a second range at 4 GiB (Milestone 179). The engine answers `OK dump
at=<n> bytes=<n>`; an engine without the command answers `ERR dump ...`. In a session the file
is in the session's folder and is removed after the reading.

## What each engine needs

| Engine | The dump | The stacks' reading |
|---|---|---|
| hvx | Built. The engine program reads `HVX_GET_SNAPSHOT` per vCPU and the guest RAM it maps. No module change, no new ioctl. | Built for its guest kernel, nixpkgs's 6.6.35 with BTF and ORC. |
| KVM | Not built: `KVM_GET_REGS` and `KVM_GET_SREGS` per vCPU and the engine's guest RAM give the same file. | Its kernels (`bzImage-kvm` and the others) are `allnoconfig` builds without BTF. They need a list of offsets for each pinned kernel, made once from a build of the same configuration with `CONFIG_DEBUG_INFO_BTF`, which does not change the layouts, and kept beside the kernel with its `System.map`. Their unwinder is frame pointers or none, so the kernel stacks are found by `scan` until a kernel with frame pointers is pinned. |
| Mac (hvm) | Not built: the engine's vCPU registers (including `TTBR0_EL1`, `TTBR1_EL1` and `SP_EL0`) and its guest RAM. | The arm64 kernels need the same list of offsets. The page walk is built and tested. arm64's `pt_regs` and `cpu_context` replace x86_64's, and the frame records (x29, x30) replace ORC. |

## The proof

The example `hydo/onboard/examples/deadlock` (its RESULTS.md has the whole answers) runs a Go and
a Rust service whose worker and watchdog take two locks in opposite orders after a pause. On hvx:

| Schedule | Verdict | Fingerprint | `hydo inspect stacks --at end` |
|---|---|---|---|
| `disk=none \| run` | OK | `79ada02759c5cdbb` | no lock waited on but the Rust main thread's `join` |
| `P=bank_go@3+2` | VIOLATION `[bank_go.progress]` | `c81b3f5264c80d56` | goroutine 6 waits on `main.journalMu`, held by goroutine 7 in `main.checkJournal`; goroutine 7 waits on `main.accountsMu`, held by goroutine 6 in `main.transfer (main.go:35)` |
| `P=bank_rs@3+2` | VIOLATION `[bank_rs.progress]` | `c315abec6fffde94` | tid 85 (`worker`) waits on `bank::JOURNAL`, held by tid 86 (`watchdog`) in `bank::check_journal`; tid 86 waits on `bank::ACCOUNTS`, held by tid 85 in `bank::transfer (main.rs:23)` |

Each schedule ran twice with one fingerprint. At a stop before the pause no lock is waited on.
A session right after the pause, read with `--follow --steps 4 --ms 20`, shows the cycle form
within 20 ms of guest time. `--check-run --every 2000` replayed both failing runs with 93 and 94
dumps and ended with the recorded fingerprints, `c81b3f5264c80d56` and `c315abec6fffde94`.

Three cases of `docs/design/debug-benchmark.md` were tried:

| Case | Engine | What the answer gave |
|---|---|---|
| ClickHouse Keeper's loop (M325, C8), known | hvx | At M325's stop, the two `KeeperHandler` threads 333 and 338 of `keeper2` are the only runnable threads of the Keeper, with `exit_to_user_mode_prepare` under `schedule`, as M325's 100 samples had it. Its other handlers sleep in `futex`. |
| Typesense's crash at start (M373, C9), known | KVM | Not handled: KVM refuses `DUMP`, and a crash needs a stop at the signal. |
| Garnet's replica that answers nothing (M371, A), open | hvx | The replica `g4` has 90 threads against 16 to 19 in each other node: 78 thread-pool workers, each asleep in `futex` on its own address. Its .NET frames are not named. |

The two recorded runs were made by other versions of hydo (the 0.4 era and 0.6.0), and the
replays followed them, so a run recorded before this milestone can be read.

## Left for later

- The Mac and KVM engines (the table before), and several vCPUs on hvx: the code reads each vCPU's
  registers and its current task, but no run with `--vcpus 2` was made.
- A shared library's frames are not named: hydo reads the program's own file only, and a
  dynamically linked program's `libc.so` frames show as addresses. The process's mappings
  (`mm->mm_mt`, a maple tree) are not read, so no library's load address is known, and an
  unwind through a library without frame pointers stops there.
- A runtime with a JIT (.NET, the JVM) keeps its frames in memory that no file describes. The
  Garnet case shows its threads and waits, not its methods.
- A thread that was woken but has not run yet reads `running`, with its old kernel stack.
- The likely holder is a guess. A lock that a function takes through a pointer it was handed
  (a mutex in a structure on the heap) is not found by the code search; then only a stack slot
  with its address names a holder. A lock in a heap structure is named by its address, not by the
  structure that holds it.
- The goroutines need the program's DWARF; a Go program built with `-ldflags=-w` shows its
  threads only.
- A frame's file and line are the innermost inlined function's. A `Lock` that the compiler
  inlined into `main.checkJournal` names `mutex.go:70`. Go's inline tree (`FUNCDATA_InlTree`) and
  DWARF's inlined calls would give the caller's line.
- A dump writes the guest's whole RAM, holes aside. The proofs' guests had 512 MiB; a guest of
  several GiB writes that much to the session's folder for each reading.
- The release does not carry the kernel's `System.map` yet: `hydo inspect stacks` looks for it
  beside the kernel, and `--kernel-map` names it.

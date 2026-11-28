# One service's own wall clock (design)

Status: a prototype is built in Milestone 372 (tentative number) for x86_64 images on hvx, and
runs on KVM since Milestone 379 (tentative number) for an image of `guest.kernel: containers`. The
authority on the token text is `hydo-schedule` (`ServiceClockChange` in `src/clock.rs`, and the
test `service_clocks` in `tests/language.rs`). The program that gives a service its own clock is
`hydo/crates/hydo-clock`. The controller's side is
`hydo/crates/hydo-control/src/features/clock.rs`. The example is `hydo/onboard/examples/service-clocks`. This page explains the problem, the
options, the choice, the schedule syntax, the explorer, the runs on real systems, and what is left.

## The problem

Every service of a hydo guest runs on one Linux kernel, so every service reads one wall clock
(`CLOCK_REALTIME`). The clock step `T=<at>:<delta>` moves that clock for every service at once.
Real clusters have clocks that differ between nodes, and that difference is a classic cause of
lease and timestamp bugs. Three examples needed it:

- TiKV (Milestone 287): a PD leader whose clock runs ahead of the stores'
  (`hydo/onboard/examples/tikv/RESULTS.md`, and open item 147).
- Prometheus (Milestone 342): a server whose clock alone runs ahead of its senders, the field
  report of prometheus#8312 and #10169 (open item 170).
- A Raft node with a slow clock, such as the braft election timer of the Typesense example
  (Milestone 366), which runs at wall-clock times.

The clock audit (open item 117) has the same gap: its schedules can step every node's clock, but
not one node's clock alone.

## The options

Each option was checked against the hvx guest kernel's own configuration, extracted from
`guest/bzImage` (`0a9ede35…`) with the kernel's `scripts/extract-ikconfig`. It has
`CONFIG_TIME_NS=y`, `CONFIG_SECCOMP_FILTER=y` and `CONFIG_CROSS_MEMORY_ATTACH=y`. The KVM and Mac
guest kernels are built from `allnoconfig` plus a list. Their lists have no `CONFIG_TIME_NS`, and
only their `containers` variants (Milestone 256) have `CONFIG_SECCOMP_FILTER`.

| Option | Programs it covers | Cost per read of the wall clock | Image bytes and fingerprints | Engines |
|---|---|---|---|---|
| Linux time namespaces | none for the wall clock (below) | none | none | hvx only; the KVM and Mac kernels lack `CONFIG_TIME_NS` |
| `LD_PRELOAD` of a library that wraps `clock_gettime`, such as libfaketime | dynamically linked programs that read the clock through libc; no static program (below) | about 0.1 µs | a library in images that use it | every engine |
| seccomp user notification | programs whose reads are system calls; hiding the vDSO needs a hook at `execve`, which seccomp does not give | two process switches | a supervisor program | hvx; KVM and Mac with the `containers` kernel |
| seccomp and ptrace, with the vDSO hidden (the choice) | programs that read the clock through glibc, musl or Go's runtime, also static ones | 40.5 µs of guest time on hvx, against 4.4 µs without it | `/bin/hydo-clock` only in images with `clock: own` | hvx; KVM and Mac with the `containers` kernel, and an arm64 port for the Mac |
| A guest kernel patch: a realtime offset per time namespace | every program, also through the vDSO, with no tracer | none | an opt-in kernel variant for each of the four kernels | every engine, after four kernel builds |

Linux 6.6's time namespaces offset only `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME`. The kernel's
`struct timens_offsets` has those two fields, and `proc_timens_set_offset` refuses every other
clock with `EINVAL`. The offsets are also fixed once a process entered the namespace
(`frozen_offsets`). So a namespace can give a service another uptime, but not another wall clock.

A preloaded library reaches C and C++, the JVM, Python, Node.js, Erlang, Ruby, and Rust on glibc.
It does not reach a static program. Every Go program is one case, because Go's runtime calls the
vDSO itself, also with cgo. Static Rust and C programs on musl and the guest's static busybox are
others.

The seccomp and ptrace option covers every program that reads the clock through glibc, musl or
Go's runtime. All three make the system call when the auxiliary vector names no vDSO.
The cost is measured in "What the prototype leaves" below.

The kernel patch is the item "Per-service clocks" of `docs/design/jepsen-gaps.md`. It needs four
parts:

1. a `CLOCK_REALTIME` offset in `struct timens_offsets` and in the namespace's vvar page;
2. the system-call paths of the realtime clocks;
3. the absolute realtime timers translated back: `clock_nanosleep` with `TIMER_ABSTIME`,
   `timerfd`, POSIX timers, and futexes with `FUTEX_CLOCK_REALTIME`;
4. offsets that change while the namespace has processes.

It touches the timekeeping core of the hvx kernel, the KVM kernel, and the two Mac kernels. The
image record would name the variant, as it names `guest.kernel: containers`. The kernel's own
times (file times, socket time stamps) would stay the guest's. It is the better second step when
the tracer's cost matters.

## The choice

`hydo-clock <offset file> <program> [args...]` runs the program under four mechanisms:

1. A seccomp filter, inherited by every process the program starts, sends the reads of the wall
   clock to hydo-clock. The reads are `clock_gettime` of `CLOCK_REALTIME`,
   `CLOCK_REALTIME_COARSE` and `CLOCK_TAI`, and `gettimeofday` and `time`. The filter also sends
   the waits until a wall-clock time: `clock_nanosleep` with `TIMER_ABSTIME`, and the futex
   operations with an absolute realtime deadline (`pthread_cond_timedwait` uses one by default).
   Every other system call runs without a stop.
2. hydo-clock traces every process of the tree with ptrace (`PTRACE_SEIZE`, with forks, clones
   and `execve` followed). At a read it writes the guest's time plus the offset into the
   process's memory and skips the call. At a wait it gives the kernel the deadline minus the
   offset, in a copy below the stack pointer.
3. At every `execve` it turns the `AT_SYSINFO_EHDR` entry of the new program's auxiliary vector
   into `AT_IGNORE`. glibc, musl and Go then find no vDSO and make the system call.
4. The offset is in a file of the run directory, `/tmp/hydo/clock.<svc>`: one line
   `<offset ns> <drift ppm>`. hydo-clock reads it at every read of the clock, so a step takes
   effect at the next read. A drift counts from the first read after its line was written.

The guest agent, the guest kernels and every image without `clock: own` stay as they are. The
service's setup, its readiness check, the workload and the checks keep the guest's clock, as an
outside observer does.

Signals pass through. hydo signals a service's whole process tree, so hydo-clock ignores the
signals that end a process by default (`SIGTERM`, `SIGINT`, `SIGHUP`, `SIGQUIT`, `SIGUSR1`,
`SIGUSR2`, `SIGPIPE`). When the program ends, hydo-clock ends with its status, and any process left
in the tree dies with it (`PTRACE_O_EXITKILL`).

## The schedule

`T=` gains a target before its step, in the style of `L=<svc>@<at>+<down>`. The parser is the
reference; these forms are its tests:

| Token | Meaning |
|---|---|
| `T=n2@5:-30` | before step 5 the clock of `n2` alone jumps back by 30 s |
| `T=n2@5:1500ms` | the same, forward by 1.5 s |
| `T=n2@0:200ppm` | from step 0 the clock of `n2` runs fast by 200 millionths |
| `T=n2@9:0ppm` | the drift of `n2` stops; the offset it built up stays |
| `T=n1@0:2 T=n2@0:-2 T=4:-300` | two nodes 4 s apart, and later a step of the guest's clock for both |

Every service token applies, in token order. The guest's own `T=<at>:<delta>` still replaces an
earlier one, as before. A whole number of seconds is written without `ms`, so `2000ms` is refused
with `2` in the message: the guest logs the token as written, and one spelling keeps one
fingerprint. The controller refuses the token before any VM when the service has no `clock: own`
(`hydo_control::names`). It logs `CLOCK of n2 5 before step 3 (offset 0 ms -> 5000 ms, drift 0
ppm -> 0 ppm)`, and a step gives the same `fault.clock_back` or `fault.clock_fwd` record as a step
of the guest's clock.

## The explorer

Built in Milestone 375 (tentative number). `hydo explore --service-clocks` draws a service's
own clock for a manifest that has services with `clock: own` and `clock` in `faults:`. The draws
come from a stream of their own (`SERVICE_CLOCK_SALT` in `hydo-explore`'s `gen.rs`), so every
other field of a seed's schedule stays as it was. Without the option, and for every manifest
without `clock: own`, the explorer draws the schedules it drew before: the corpus goldens of
`hydo-explore` did not change. The option is opt-in, not a default, because a default would
change the draws of the manifests that already have `clock: own`.

Most drawn schedules get one service's own clock change:

- A schedule with a step of the guest's clock (`T=<at>:<delta>`) gives it up in half of the
  schedules for one service's change before the same step.
- A schedule without one gets a service's change in 60 % of the schedules.
- The service is one of the services of `clock: own`, at random. The change is a step of
  `±250ms`, `±1500ms`, `±5`, `±30` or `±300`, or a drift of `±200ppm` or `±50000ppm`.
- With two or more such services, a quarter of the changes put two of them apart: a second
  service gets the opposite change before the same step.

The shrinker removes each token. It also halves a step longer than 250 ms and a drift larger
than 200 ppm, towards 0. A schedule with a service's clock is in the failure class of the clock
faults (`clock`, or `clock` with the other families). The explorer refuses `--service-clocks` before
any run when the image's manifest has no service with `clock: own`, and when the manifest's
`faults:` does not allow `clock`.

The clock audit (`hydo rules scan --kind clock`) proposes two more schedules for a place whose
code a service of `clock: own` runs. In `own-ahead` that service's clock alone goes forward by
more than the place's limit, and in `own-behind` it goes back by as much. A project without
`clock: own` gets the audit's report of before.

## What moves

Nothing that is recorded. An image without `clock: own` has the bytes it had, because
hydo-build stages `/bin/hydo-clock` only for such an image, and a schedule without the new form
gives the controller's commands of before. So the fingerprint epoch stays 6. The pinned
reproducers gave their epoch-6 values with this tree (`docs/HISTORY.md`, Milestone 372).
Milestone 379's fallback for KVM is in a second build of hydo-clock, staged only for an image of
`guest.kernel: containers`, so an image of `clock: own` built before keeps its bytes too.

A kernel variant would not change that rule either. It would be opt-in, keyed on a manifest key,
and the image record would name the kernel by its sha256, as `guest.kernel: containers` does.

## What the prototype leaves

- The cost: on hvx one read of the wall clock took 40.5 µs of guest time under hydo-clock. It
  took 4.4 µs without it (20,000 reads of a static Go program). A program that reads the clock in a
  tight loop runs slower in guest time. The monotonic clock's reads become system calls without a
  stop.
- Not covered:
  - a program that reads the TSC itself for the wall clock, or finds the vDSO in
    `/proc/self/maps`;
  - a 32-bit program;
  - a program that uses ptrace itself, such as a debugger in the service. The kernel's own times stay the guest's: file modification times, socket time stamps,
  `/proc/uptime`, and the kernel log.
- `timerfd` and POSIX timers with an absolute realtime deadline are not translated.
- A wait that already started keeps its deadline when the offset changes; Linux moves such a
  wait when the real clock is set.
- The offset file is in the run directory, so a recovery boot after a power cut and a reboot
  start every own clock at the guest's clock. A drift restarts from 0 when hydo-clock starts
  again after a kill.
- x86_64 only. hvx runs it, and KVM since Milestone 379 for an image of `guest.kernel:
  containers`. The Mac engine does not: the section "The KVM and Mac engines" says what it
  needs.

## The KVM and Mac engines

Milestone 375 (tentative number) read what the two other engines lack. Both engines' guest
kernels start from `allnoconfig` and a list of options, so an option that no list names is off.
Milestone 379 (tentative number) built what KVM needs without a change of a kernel.

### KVM (built in Milestone 379)

Two options were missing:

- **seccomp.** Only the `containers` lists have `CONFIG_SECCOMP_FILTER`
  (`hv/kvm/linux/hydo-kvm-containers.config` and `hv/hvm/explore/hvm-containers.config`). So a
  service of `clock: own` needs `guest.kernel: containers` on KVM. The KVM engine refuses an
  image whose record names no such kernel before any VM: `hydo_control::agent::
  kvm_own_clock_refusal` names the services and the key. hvx boots its own kernel, which has
  seccomp, whatever `guest.kernel` says.
- **The traced process's memory.** hydo-clock reads and writes the traced process's memory with
  `process_vm_readv` and `process_vm_writev`. They need `CONFIG_CROSS_MEMORY_ATTACH`, which no
  KVM or Mac list names, so every KVM kernel answers `ENOSYS`.

hydo-clock's cargo feature `words` (`src/words.rs`) adds the fallback. When either call answers
`ENOSYS`, `peek` and `poke` reach the memory with `PTRACE_PEEKDATA` and `PTRACE_POKEDATA`, one
aligned 8-byte word per call. An aligned word never crosses a page, and a write of part of a word
reads the word first, so the bytes around a range stay. Where the calls work, the feature changes
nothing.

The feature is off in the default build, and the compiler removes the two calls into the empty
module, so that build keeps Milestone 372's bytes (`de24fd93…`). `build.sh` makes both builds,
`hydo-clock` and `hydo-clock-words`, each to the same bytes on every build. `hydo build` stages the
build with the feature as `/bin/hydo-clock` for an image of `guest.kernel: containers`
(`HYDO_CLOCKBIN_WORDS`, default `<build dir>/hydo-clock-words`), and Milestone 372's build for
every other image. So every image built before keeps its bytes and its fingerprints on hvx.

The example's `hydo-kvm.yaml` is its `hydo.yaml` with `guest.kernel: containers` and another
name. The runs are in `docs/HISTORY.md`, Milestone 379.

### The Mac (not built)

The Mac engine needs the same two things and one more:

- `guest.kernel: containers`, as on KVM: `Image-explore-containers` has seccomp, the other Mac
  kernels do not. The build with the feature `words`, because no Mac kernel has
  `CONFIG_CROSS_MEMORY_ATTACH`.
- An arm64 build of hydo-clock. `clock: own` is refused for `--arch arm64` until it exists
  (`hydo-manifest`'s validator). The port changes these parts of `main.rs`:
  - the registers: `struct user_pt_regs` (`regs[31]`, `sp`, `pc`, `pstate`) through
    `PTRACE_GETREGSET` and `PTRACE_SETREGSET` with `NT_PRSTATUS`, because arm64 has no
    `PTRACE_GETREGS`. The system call number is in `x8`, the arguments in `x0` to `x5`, and the
    result goes into `x0`;
  - skipping a call: the number -1 through `PTRACE_SETREGSET` with `NT_ARM_SYSTEM_CALL`, then
    the result in `x0` (x86_64 writes `orig_rax` instead);
  - the numbers: arm64 has no `time` system call, and `clock_gettime`, `gettimeofday`,
    `clock_nanosleep` and `futex` are 113, 169, 115 and 98;
  - the seccomp filter's architecture, `AUDIT_ARCH_AARCH64` (`0xc00000b7`);
  - the stack below the stack pointer, for the moved deadline: arm64 has no red zone, so
    `sp - 512` is free as on x86_64;
  - hiding the vDSO, which is the same: `AT_SYSINFO_EHDR` becomes `AT_IGNORE`;
  - the check for a 32-bit program: an AArch32 task's `pstate` has bit 4 set, where x86_64 reads
    `cs`.
- A build recipe: `build.sh` with the `aarch64-unknown-linux-musl` target, as the arm64 builds of
  hydo-agent and hydo-world are made, and `HYDO_CLOCKBIN_ARM64`.
- **The engine's clock.** The Mac engine counts exits, as hvx and KVM do, so a read under
  hydo-clock costs guest time there too.

A kernel variant with a realtime offset per time namespace would replace all of it, at the cost
of four kernel builds (the section "The options").

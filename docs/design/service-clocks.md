# One service's own wall clock (design)

Status: a prototype is built in Milestone 372 (tentative number) for x86_64 images on hvx. The
authority on the token text is `hydo-schedule` (`ServiceClockChange` in `src/clock.rs`, and the
test `service_clocks` in `tests/language.rs`). The program that gives a service its own clock is
`hydo/crates/hydo-clock`. The controller's side is
`hydo/crates/hydo-control/src/features/clock.rs`. The example is `hydo/onboard/examples/service-clocks`. This page explains the problem, the
options, the choice, the schedule syntax, the explorer, and what is left.

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

## The explorer (designed, not built)

`hydo explore` would draw a service's clock only for a manifest with `clock: own` and `clock` in
`faults:`, from a stream of its own, so every other manifest draws the schedules it draws today.
For such a manifest one clock fault in two would be a service's own: one service of `clock: own`
at random, a step from `±250ms`, `±1500ms`, `±5`, `±30` and `±300`, or a drift of `±200ppm` and
`±50000ppm`. The shrinker would remove the token, then halve the step towards 0. The clock audit
would propose one service's step for a place that a single service runs.

## What moves

Nothing that is recorded. An image without `clock: own` has the bytes it had, because
hydo-build stages `/bin/hydo-clock` only for such an image, and a schedule without the new form
gives the controller's commands of before. So the fingerprint epoch stays 6. The pinned
reproducers gave their epoch-6 values with this tree (`docs/HISTORY.md`, Milestone 372).

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
- x86_64 only: the Mac engine needs an arm64 build of hydo-clock (other registers and system call
  numbers). On the KVM and Mac engines the service needs the `containers` kernel for seccomp.
  Without it hydo-clock stops with `hydo-clock: cannot install the clock filter` and the status
  126, and the service does not start. Neither engine ran it.
- The explorer does not draw it, and the clock audit does not propose it.

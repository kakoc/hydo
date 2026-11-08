# Determinism and environment hazards

The guest is one Linux virtual machine. It has:

- one virtual CPU by default, and `guest.ram_mib` of RAM (default 1024 MiB);
- no network outside the guest: loopback, or with `network: bridge` one bridge between the
  services' own namespaces;
- a read-only root (an image service can have a writable one, `writable: true`), and one data
  disk on `/data`;
- a clock and a random generator that hydo makes deterministic.

Check each item and report what you found.

## Handled by hydo (nothing to do)

- The random generator is seeded deterministically, so `getrandom()` and `/dev/urandom` do not
  block.
- Loopback is up; the realtime clock starts at `guest.clock_start`. The format's default is
  2026-01-01; `hydo init` and `hydo compose` write the day they ran. Software built after the
  clock start sees its own files and catalogs in the future, so keep `clock_start` after the
  newest release in the image.
- Services start in dependency order with readiness waits (`start_after: started` for a cluster
  whose readiness needs its peers, `hydo guide services`); killed services are restarted; the
  recovery boot after a power cut is automatic. A service that crashes by itself is started again
  only at the heal, unless it has a `restart:` policy such as `always` or `on-failure`
  (MANIFEST.md, section `Restart policies`). A service that does not come back at the heal ends the run in ERROR,
  unless it has `restart_may_fail: true` (MANIFEST.md "A restart after the faults that may fail").
- Faults (kill, pause, the freeze of a power cut) reach a service's whole process tree.

## Must be checked per project

- **Network access.** Anything that downloads, resolves external names, sends telemetry, checks
  for updates or contacts a license server at start fails. Turn it off in the configuration, or
  report it as a blocker. Outside services that the system calls become fakes
  (`hydo guide outside-services`).
- **Listening address.** On one shared loopback network, bind every service to 127.0.0.1 (or to
  its `addr:`). With `network: bridge`, a service that others call must listen on `0.0.0.0` or on
  its `addr:`. Inside its namespace, 127.0.0.1 is reachable only from the service itself, as in a
  container. Its peers, the workload, and the checks connect from outside it; the workload and
  the checks connect from 10.89.0.1. External addresses do not exist.
- **Writable paths.** Only `/data` (persistent, the fault target) and `/tmp` (lost at a power
  cut) are writable. Point data, logs, sockets, and PID files there. An image service also gets
  the writable paths of its `mounts:`.
- **CPU features.** The guest CPU is x86-64-v2 by default (SSE4.2, POPCNT). It has no AVX and
  no AVX2. Binaries built for x86-64-v3, or that need AVX at start, stop with one of these
  messages: `illegal instruction`, `invalid opcode`, `CPU ISA level is lower than required`. Set `guest.cpu: v3` (x86_64 only;
  AVX/AVX2, FMA, BMI1/2; no AVX-512), or use a build for the baseline. On the Mac the guest is
  arm64: images and binaries must be built for linux/arm64.
- **CPU caches.** By default the guest's CPUID describes no caches. The glibc 2.28 of Red Hat
  Enterprise Linux 8 and its rebuilds (AlmaLinux 8, Rocky Linux 8, UBI 8) then crashes in
  `memcpy` at `guest.cpu: v3`. The programs of such an image end with a segfault in
  `libc-2.28.so`. Set `guest.cpu_cache: true` (x86_64 only). `hydo build` warns when an image
  needs it.
- **Memory.** Every service together must fit in `guest.ram_mib` (at most 8192 MiB). Cap heaps
  and caches explicitly (JVM `-Xmx`, database buffer pools, Redis `maxmemory`). The host needs
  that much free memory for every guest that runs at once. `hydo run` refuses a guest that does
  not fit, and `hydo explore` sizes its pool from the guest's RAM: one 8 GiB worker on a 16 GiB
  machine.
- **One CPU.** Set worker and thread counts to 1 or 2 where the program sizes them from the CPU
  count (Go: `GOMAXPROCS=1`; JVM: `-XX:ActiveProcessorCount=1`; database I/O threads).
- **JVM services.** Use deterministic-friendly flags: one synchronous C1 compiler
  (`-XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation`), Serial GC
  (`-XX:+UseSerialGC`), a fixed heap (`-Xms` = `-Xmx`), `-XX:-UsePerfData`,
  `-XX:-UseContainerSupport`, `-Xshare:off`, and a fixed time zone and locale
  (`-Duser.timezone=UTC`).
- **Python services and tests.** An image gives every file one fixed time, so Python's bytecode
  caches (`__pycache__/*.pyc`) look stale and Python compiles every module again in every run.
  That costs guest time and can print a warning, which a test suite with warnings as errors
  turns into failures. Set `root.fix_pyc: true` (`hydo guide manifest`, section
  `Python bytecode`).
- **Wall clock in data.** Timestamps in payloads or keys make runs differ when the clock is
  stepped; derive values from `$HYDO_CMD_ID` or `$HYDO_SEED` instead. Expiry, TTL, or lease
  logic is affected by clock-jump faults on purpose: note it as a target, not as a hazard. If a
  check compares times from before and after a power cut, set `guest.clock_after_cut`. Set it
  too when the system stores times and checks them when it starts (hybrid clocks, leases).
  Without it the recovery boot's clock starts at `clock_start` again (`hydo guide faults`).
- **Background timers.** Periodic timers of 100 ms or less (a server `hz` of 10 or more) cost
  guest time; lower them where the project allows (`hz 1` for Redis).
- **Services that refuse to run as root** (PostgreSQL and others). Everything in the guest runs
  as root. Add the user to `/etc/passwd` and `/etc/group` under `root.text`, `chown` its data
  directory in `setup`, and start it with `su <user> -s /bin/sh -c "exec ..."` so that the
  service keeps the process id that kills and pauses target. Image services take `user:`.
- **Processes that leave the tree.** A process that double-forks or calls `setsid` into a new
  parent is not reached by kills and pauses. Check `ps` in the guest for such services.
- **Daemonizing and PID files.** Services must stay in the foreground.
- **Containers inside the guest.** Docker, cgroup sandboxes and overlay filesystems are not
  available; run the binary directly or as an image service.
- **Kernel modules.** The guest kernel is a fixed Linux 6.6 build; a program that needs a module
  the image does not load (FUSE, special filesystems) is a blocker to report.
- **busybox tools in checks.** Checks run busybox: `grep -v -x -F -f FILE` with an EMPTY `FILE`
  matches every line (GNU grep matches none), so "acknowledged ids that were not read back" comes
  out empty exactly when nothing was read back. Compare sets with `awk` keyed on `FILENAME`, or
  with `hydo-check`.
- **Console size.** The guest console holds 64 KiB per VM on hvx (`hydo run` warns when a run
  wrote more) and 4 MiB on the KVM engine and the Mac. Print short lines; do not print every
  acknowledged id. A long history (thousands of operations) fits on the KVM engine or the Mac.

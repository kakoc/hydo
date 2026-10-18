# Determinism and environment hazards

The guest is one virtual CPU, 1 GiB of RAM, loopback networking only, a read-only root and
one data disk, and a clock and random generator that hvx makes deterministic. Check each
item and report what you found.

## Handled by hydo-init (nothing to do)

- The random generator is seeded deterministically, so `getrandom()` and `/dev/urandom` do
  not block.
- Loopback is up; the realtime clock starts at `guest.clock_start`.
- Services start in order with readiness waits; killed services are restarted; the power-cut
  recovery boot is automatic.

## Must be checked per project

- **Network access.** Anything that downloads, resolves DNS, sends telemetry, checks for
  updates or contacts a license server at start fails. Disable it in the configuration, or
  report it as a blocker.
- **Listening address.** Bind every service to 127.0.0.1; external addresses do not exist.
- **Writable paths.** Only `/data` (persistent, the fault target) and `/tmp` (lost on a power
  cut) are writable. Point data, logs, sockets and PID files there.
- **CPU features.** The guest CPU offers x86-64-v2 (SSE4.2, POPCNT), not AVX or AVX2.
  Binaries built for x86-64-v3 or that require AVX at start fail with an illegal instruction
  or "CPU ISA level is lower than required". Prefer distribution builds (nixpkgs), which
  target the baseline.
- **Memory.** Everything together must fit in 1 GiB. Cap heaps and caches explicitly (JVM
  `-Xmx`, database buffer pools, Redis `maxmemory`).
- **One CPU.** Set worker and thread counts to 1 or 2 where the program sizes them from the
  CPU count (Go: `GOMAXPROCS=1`; JVM: `-XX:ActiveProcessorCount=1`; database I/O threads).
- **JVM services.** Use the flags the stack example uses (`JCOMMON` in
  `examples/stack/hydo.yaml`): one synchronous C1 compiler, Serial GC, a fixed heap,
  `-XX:-UsePerfData`, `-XX:-UseContainerSupport`, `-Xshare:off`, a fixed time zone and locale.
  CLAUDE.md Milestones 31–32 explain why.
- **Wall clock in data.** Timestamps in payloads or keys make runs differ when the clock is
  stepped; derive values from the step number instead. Expiry, TTL or lease logic is affected
  by clock-jump faults on purpose; note it as a target, not a hazard.
- **Background timers.** Very short periodic timers (e.g. a server `hz` of 10+) cost guest
  time; lower them when the project allows (`hz 1` in the Redis examples).
- **Services that refuse to run as root** (PostgreSQL, some others). Everything in the guest
  runs as root. Add the user to `/etc/passwd` and `/etc/group` under `root.text`, `chown`
  its data directory in `setup`, and start it with `su <user> -s /bin/sh -c "exec …"` so the
  service keeps the PID that kills and pauses target. Say that this PID behaviour is
  unverified until a kill schedule has been run.
- **Daemonizing and PID files.** Services must stay in the foreground, or kills and pauses
  hit the wrong process.
- **Containers inside the guest.** Docker, cgroups-based sandboxes and overlay filesystems
  are not available; run the binary directly.
- **Kernel modules.** The guest kernel is a fixed NixOS 6.6 build; anything that needs a
  module the image does not load (FUSE, special filesystems) is a blocker to report.

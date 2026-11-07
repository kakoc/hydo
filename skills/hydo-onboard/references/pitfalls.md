# Pitfalls to look for in the project

hydo runs the whole system in one small virtual machine with no outside network and a
deterministic clock. Check the project for each item below and report what you found and what you
did. The exact limits of the installed hydo (memory, CPU level, writable paths, the guest kernel)
are in `hydo guide hazards`; read it once.

- **Network at start**: downloads, outside name lookups, telemetry, update checks, license
  servers. Turn them off in the configuration, make the outside service a fake, or report a
  blocker.
- **Listening addresses**: a service that others call must listen where they can reach it (see
  `hydo guide hazards` for loopback and per-service networks).
- **Writable paths**: data, logs, sockets and PID files must go where the guest allows writes;
  everything else is read-only.
- **CPU level and architecture**: programs built for a newer CPU level, or for another
  architecture, stop at start with "illegal instruction" or "exec format error".
- **Memory**: all services together must fit in the guest; cap heaps, buffer pools and caches
  explicitly.
- **Threads sized from the CPU count**: set worker counts to 1 or 2 where the program sizes them
  from the CPUs (Go's `GOMAXPROCS`, the JVM's processor count, database I/O threads).
- **JVM services**: a single synchronous JIT compiler, a serial collector and a fixed heap keep
  runs fast and repeatable; the exact flags are in `hydo guide hazards`.
- **Wall-clock values in data**: timestamps in payloads or keys make runs differ when the clock is
  stepped; derive values from hydo's seed and command id. Expiry and lease logic is a target for
  clock faults, not a hazard.
- **Short background timers**: very frequent timers cost guest time; lower them where allowed.
- **Services that refuse to run as root**: everything in the guest runs as root; such a service
  needs a user and a start command that keeps its process id.
- **Processes that leave the process tree** (double fork, `setsid`): faults do not reach them.
- **Daemonizing and PID files**: every service stays in the foreground.
- **Containers, cgroups and special kernel modules inside the guest**: not available; run the
  program directly or as an image service, or report a blocker.
- **Shell tools in checks**: checks run small busybox tools whose edge cases differ from GNU's;
  compare sets with the ready-made checkers or `awk`.
- **Console size**: print short lines; never print every id.

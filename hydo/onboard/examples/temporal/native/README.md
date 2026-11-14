# Temporal without hydo: no database after a backward clock step

This folder reproduces finding 2 of [`../RESULTS.md`](../RESULTS.md) without hydo, on Temporal
1.32.2 and PostgreSQL 16 in Docker. When the clock has gone back and the server then loses
its database connections once, the server stops serving until the clock is back where it was.
Every start and every list of workflows answers `Namespace default is not found.`, and the
server logs `did not refresh database connection pool because the last refresh was too close`
thousands of times.

The cause is in `common/persistence/sql/sqlplugin/db_handle.go`. After a connection error, the
handle drops its connection pool and reconnects, at most once per second. It measures that
second with `timeSource.Now()`, and `clock.RealTimeSource.Now()` returns `time.Now().UTC()`,
which carries no monotonic clock reading. After a step back of D seconds, `now.Sub(lastRefresh)`
stays below one second for D seconds, and the handle stays without a pool for that long.

## Prerequisites

- A Linux machine or virtual machine of your own with Docker and Compose v2, where root may set
  the clock. The step changes the clock of every container on that machine.
- Its time synchronisation stopped, or it puts the clock back before the probes end.
- Go 1.26 or later, to build the example's worker: `sh ../build.sh` writes `../hydo/bin/tw`.
  Build it for the machine's architecture, for example `GOARCH=amd64 sh ../build.sh`.

On a Mac, a Colima virtual machine of its own serves. These commands make one and stop its two
clock services:

```sh
colima start hydo-tw --cpu 2 --memory 4 --disk 12 --runtime docker
export LIMA_HOME=$HOME/.colima/_lima
limactl shell colima-hydo-tw sudo systemctl stop systemd-timesyncd lima-guestagent
export DOCKER_HOST=unix://$HOME/.colima/hydo-tw/docker.sock
export CLOCK="limactl shell colima-hydo-tw sudo"
```

`colima start` makes the new profile Docker's current context; `docker context use colima`
puts the old one back. `colima delete hydo-tw` removes the machine afterwards.

## Steps

1. Run the control, the same crash of PostgreSQL without a clock step:

   ```sh
   CONTROL=1 PROBE=60 sh repro-clock-db.sh
   ```

2. Run the reproduction, a step back of one hour before the crash:

   ```sh
   PROBE=150 sh repro-clock-db.sh
   ```

Each run starts the services of `compose.yaml` as the Compose project `hydo-native-temporal`.
That file runs PostgreSQL in a loop inside its container, so that a crash leaves the container
and its address in place, as after a crash of the database server. The script then does these
steps:

1. It starts the order `before` and waits until it completes.
2. Without `CONTROL=1`, it steps the clock back `STEP` seconds (default 3600).
3. It kills PostgreSQL's server with `kill -9`. The loop starts the server again 5 s later.
4. For `PROBE` seconds, it starts one order every 10 s and reads the state of the order
   before it.
5. It counts the completed orders and two lines in the server's log.
6. It puts the clock forward again.

At the end it removes the project with its volumes. `KEEP=1` leaves it running.

## The expected outcome

The control of 2026-11-14 printed:

```text
== control: no clock step
== kill PostgreSQL's server; its container starts it again 5 s later
PostgreSQL answers again after 6 s
   7s  order probe-1: started; order probe-0: -
  17s  order probe-2: started; order probe-1: COMPLETED
  ...
== result: 6 of 6 orders started, 6 completed within 70 s of PostgreSQL's return
   server log: 56 x 'did not refresh database connection pool because the last refresh was too close'
   server log: 100 x 'no usable database connection found'
```

The reproduction of 2026-11-14 printed:

```text
== step the clock back 3600 s
clock now 2026-11-14T09:12:06Z
== kill PostgreSQL's server; its container starts it again 5 s later
PostgreSQL answers again after 5 s
   5s  order probe-1: not started; order probe-0: -
  16s  order probe-2: not started; order probe-1:
  ...
 138s  order probe-8: not started; order probe-7:
== put the clock forward again
clock now 2026-11-14T10:16:18Z
== result: 0 of 8 orders started, 0 completed within 160 s of PostgreSQL's return
   server log: 8119 x 'did not refresh database connection pool because the last refresh was too close'
   server log: 11933 x 'no usable database connection found'
```

During that run, `temporal workflow start` and `temporal workflow list` answered `Namespace
default is not found.` The failed operations in the server's log were, among others,
`ListNamespaces` (2,875 times), `GetCurrentExecution` (1,746), `GetHistoryTasks` (1,260),
`GetTimerTasks` (803), `GetTaskQueue` (248) and `UpsertClusterMembership` (234). The throttle
lines ran from the kill until the clock was put forward, 4 minutes 12 seconds. After the clock
was put forward, a new order completed within 8 s.

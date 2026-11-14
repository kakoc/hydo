# River without hydo: two leaders after a PostgreSQL crash

This folder reproduces finding 1 of [`../RESULTS.md`](../RESULTS.md) without hydo, on River
v0.51.2 and PostgreSQL 16.16.

River elects one leader among its clients, and only the leader runs the maintenance services,
among them the periodic job enqueuer. River keeps the leader in the table `river_leader`, which
its migration creates `UNLOGGED`. PostgreSQL empties an unlogged table when it recovers from a
crash. So after a crash of PostgreSQL another client can win the election at once, while the
old leader still trusts its term and goes on enqueuing. For a few seconds two clients act as
leader, and both insert the same run of a periodic job. A clean restart of PostgreSQL keeps the
table, and the leader stays.

## Prerequisites

- Docker. The scripts use the image `postgres:16.16-alpine3.24`.
- Go. `stack.sh` sets `GOTOOLCHAIN=go1.27.3`, so Go fetches that version once.
- The port 55433 on 127.0.0.1 free.

## Steps

Measure crashes against clean restarts, alternating, on a fresh stack:

```sh
N=30 DOWN=1 sh measure.sh
```

`measure.sh` runs `stack.sh up`. It starts one container, `hydo-native-river-pg`, and three
worker processes of `../app` on this machine, `w1`, `w2` and `w3`, with their logs in `work/`.
Each worker is a River client with the queue `default`, leader election, and one periodic job
that runs every 2 seconds. The job's arguments name the client that inserted it. At the end
`measure.sh` runs `stack.sh down`, which stops the workers and removes the container and its
volume.

To see one restart at a time:

```sh
sh stack.sh up
MODE=crash DOWN=1 sh probe-restart.sh       # SIGKILL PostgreSQL, start it again after 1 s
MODE=graceful DOWN=1 sh probe-restart.sh    # stop it with SIGTERM, start it again after 1 s
MODE=crash PAUSE=6 sh probe-pause.sh        # the same crash, with the leader stopped for 6 s
sh stack.sh down
```

`N=10 PAUSE=6 sh measure-pause.sh` measures the stopped leader against a clean restart and
against the stop alone.

## The expected outcome

`probe-restart.sh` prints the leader before the restart, each value of `river_leader` it saw
in the 25 seconds after it, and each run of the periodic job that two clients inserted:

```text
crash: leader before w3-76944; leader rows after: 09:08:20:? 09:08:21:w2-76927
crash: 1 periods with the periodic job inserted by two clients 1794643704000:w3-76944+w2-76927
```

After a crash the leader row is gone (`(none)`), and often another client takes the leadership.
In some crashes the old leader and the new one both insert one run of the periodic job. After a
clean restart the leader row is still there, the leader stays, and no run is inserted twice.

## The measured rates

All runs were on an Apple M2 Pro with macOS 26.4.1, PostgreSQL in Docker 28.4.0, on
2026-11-14. "Leader row gone" counts the restarts after which the probe saw `river_leader`
empty; it samples every 0.5 s, so it can miss a short gap.

| script | restarts | leader row gone | leader changed | a run inserted by two clients |
|---|---|---|---|---|
| `N=3 DOWN=0 sh measure.sh` | 3 crash | 3 | 1 | 0 |
| | 3 clean | 0 | 0 | 0 |
| `N=10 DOWN=3 sh measure.sh` | 10 crash | 6 | 7 | 1 |
| | 10 clean | 0 | 0 | 0 |
| `N=30 DOWN=1 sh measure.sh` | 30 crash | 18 | 21 | 5 |
| | 30 clean | 0 | 0 | 0 |

A double insert needs one more thing than the crash: another client's election attempt must
come before the old leader's next reelection attempt. So only some crashes show it.

`probe-pause.sh` and `measure-pause.sh` show the cause without that race. They stop the leader
with SIGSTOP for 6 s across the restart, well inside its 15 s term. With `N=10 PAUSE=6 sh
measure-pause.sh`:

| restart, leader stopped for 6 s | another client took the leadership during the term | a run inserted by two clients |
|---|---|---|
| crash | 10 of 10, 0 to 3 s after the restart | 0 of 10 |
| clean | 0 of 10 | 0 of 10 |
| none (only the stop) | 0 of 10 | 0 of 10 |

After a crash another client wins the election while the stopped leader's term still holds. No
run was inserted twice in this mode: the stopped leader inserts nothing while it is stopped. Why
it inserted nothing after it resumed, and before it learned that it lost, was not checked.

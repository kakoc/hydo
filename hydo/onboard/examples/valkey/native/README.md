# Finding 1 without hydo

These scripts reproduce finding 1 of [../RESULTS.md](../RESULTS.md) with plain Docker and the
official Valkey 9.1.3 image. Valkey is not changed. Nothing here is a fix or a patch.

The finding needs a step of the primary's wall clock, as an NTP step makes. A container cannot
step its clock alone: the clock belongs to the Docker machine and every container on it.
So the image of [Dockerfile](Dockerfile) adds libfaketime, a test tool that moves the wall clock
of one process. The primary runs under it with `FAKETIME_DONT_FAKE_MONOTONIC=1`, so only its
realtime clock moves, as in a real step, and its monotonic clock runs on. The replica runs
without a step. Both run with Valkey's default settings.

## What the scripts show

| script | what it does |
|---|---|
| `fullsync-clock-step.sh` | starts a primary with 1,000 keys and a fresh replica. One second after the replica connects, it steps the primary's clock back and measures how long the replica needs to sync |
| `min-replicas-clock-step.sh` | sets `min-replicas-to-write 1` and `min-replicas-max-lag 10`, freezes the only replica with `docker compose pause`, steps the primary's clock back, and counts the writes the primary still acknowledges |

Both scripts need Docker with Compose, build the image once, and remove their containers and
volumes at the end.

```sh
sh fullsync-clock-step.sh          # the steps +0, -60s and -120s
sh min-replicas-clock-step.sh      # the steps +0 and -120s
```

## The results on a Mac with Docker 28.4 (2026-11-14)

`fullsync-clock-step.sh` (with the replica's Docker address replaced by `<replica>`):

```text
step +0: 3 s later the primary shows: slave0:ip=<replica>,port=6379,state=wait_bgsave,offset=0,lag=0,type=replica
step +0: replica synced=yes after 6 s, replica DBSIZE 1000
step -60s: 3 s later the primary shows: slave0:ip=<replica>,port=6379,state=wait_bgsave,offset=0,lag=0,type=replica
step -60s: replica synced=yes after 65 s, replica DBSIZE 1000
step -120s: 3 s later the primary shows: slave0:ip=<replica>,port=6379,state=wait_bgsave,offset=0,lag=0,type=replica
step -120s: replica synced=yes after 124 s, replica DBSIZE 1000
```

Without a step the replica is in sync after 6 seconds: the 5 seconds of
`repl-diskless-sync-delay` and the transfer. Each backward step adds its own length to the wait.
During the wait the primary lists the replica as `wait_bgsave` and starts no transfer, and the
replica has none of the primary's data.

`min-replicas-clock-step.sh`:

```text
step +0: before the pause SET answers OK
step +0: the primary acknowledged 9 writes in the 11 s after its replica stopped; then: NOREPLICAS Not enough good replicas to write.
step -120s: before the pause SET answers OK
step -120s: the primary acknowledged 114 writes in the 133 s after its replica stopped; then: NOREPLICAS Not enough good replicas to write.
```

Without a step the primary refuses writes about 10 seconds after its replica stops answering, as
`min-replicas-max-lag 10` says. After a step of 120 seconds back, it acknowledges writes for
133 seconds. None of those writes reached a replica.

# The snapshot choice without hydo

[snapshot-by-mtime.sh](snapshot-by-mtime.sh) shows the snapshot choice of finding 1 with the
official image `ghcr.io/dragonflydb/dragonfly:v2.1.0` in Docker, with Dragonfly's default
settings and two threads.

A backward step of the host's clock would change the clock of every container on the machine, so
the script does not step a clock. It writes two snapshots and then gives the files of the first
snapshot a modification time one hour ahead with `touch`. After a step of one hour back, the
snapshots from before the step look the same to every later start.

```sh
sh snapshot-by-mtime.sh
```

The script does these steps:

1. It starts Dragonfly with an empty volume.
2. It sets `k` to `first` and runs `SAVE`.
3. It sets `k` to `second` and runs `SAVE` again, 2 s later.
4. It gives the files of the first snapshot a time one hour ahead.
5. It stops the container with SIGTERM. That is a clean shutdown, which saves a snapshot.
6. It starts the container again and reads `k`.

The end of a run on a Mac with Docker 28.4.0, on 2026-11-16:

```text
I20261116 19:52:19.858577     7 snapshot_storage.cc:318] Load snapshot: Searching for snapshot in directory: "/data"
I20261116 19:52:19.863207     7 server_family.cc:1775] Loading /data/dump-2026-11-16T19:52:17-summary.dfs
files after the restart:
-rw------- 1 dfly dfly 4096 20:52:19 dump-2026-11-16T19:52:17-summary.dfs
-rw------- 1 dfly dfly 4096 19:52:19 dump-2026-11-16T19:52:19-summary.dfs
after the restart: k=first
```

The shutdown's snapshot fell in the same second as the second `SAVE`, so it replaced that
snapshot under the same name. Dragonfly loaded the first snapshot, whose files had the later
time. The value `second`, which a `SAVE` and the shutdown both saved, is gone.

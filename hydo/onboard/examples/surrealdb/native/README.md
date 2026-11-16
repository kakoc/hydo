# Finding 1 without hydo

`vlog-tail-cut.sh` reproduces finding 1 of [../RESULTS.md](../RESULTS.md) with plain Docker and
SurrealDB's image `surrealdb/surrealdb:v4.0.1`. SurrealDB runs with its defaults: SurrealKV, with
every commit synced before its answer (`sync=every`). Nothing here is a fix or a patch.

The script damages one value of SurrealKV's value log with `dd`, as a disk that loses synced
bytes does. hydo's `G=` fault did the same in the guest.

```sh
sh vlog-tail-cut.sh                 # the damaged value is the newest large one: commits are dropped
LATER_BIG=1 sh vlog-tail-cut.sh     # one more large value follows: the open refuses
```

The script needs Docker and `curl`, and removes its container and volume at the end.

## The results on a Mac with Docker 28.4 (2026-11-16)

`sh vlog-tail-cut.sh`:

```text
acknowledged: big 30, small 40
read back:    big 30, small 40
zeroed 512 bytes at 267801 of /data/db/vlog/00000000000000000001.vlog (271801 bytes)
after the restart: big 29, small 29
missing small records: 110 101 102 103 104 30 105 106 107 108 109
the server's log:
  2026-11-16T20:17:35.117644Z  WARN surrealkv::lsm: WAL segment 1 ends before record 76, at offset 8992: the value it points to is not in the value log. 12 record(s) and 33578208 byte(s) were left out, and the segment as it was is kept as "/data/db/wal/00000000000000000001.wal.corrupt".
  2026-11-16T20:17:35.118287Z  WARN surrealkv::lsm: WAL recovery completed with modifications: 33578208 bytes trimmed, 12 records dropped, 1 quarantined files: ["/data/db/wal/00000000000000000001.wal.corrupt"]
```

`LATER_BIG=1 sh vlog-tail-cut.sh`:

```text
acknowledged: big 31, small 40
read back:    big 31, small 40
zeroed 512 bytes at 267860 of /data/db/vlog/00000000000000000001.vlog (280860 bytes)
the server did not start:
  2026-11-16T20:17:39.371143Z ERROR surrealdb_server::cli: There was a problem with the datastore: Other error: WAL segment 00000000000000000001.wal at offset 8992: the value that record 76 points to is not in the value log, a record after it points to one that is, and a sync mark says that the segment was synced past record 76. The log is not cut automatically: configure WalRecoveryMode::SalvageValidPrefix to end it at that record.
```

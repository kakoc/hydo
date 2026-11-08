# sqld forgets admin API settings on restart: a reproduction without hydo

This folder shows finding 3 of `../RESULTS.md` with plain Docker. When sqld runs with namespaces
disabled (the default), a setting made through the admin API (`POST
/v1/namespaces/default/config`) holds until the server restarts. After any restart, the default
namespace has the default configuration again. The `durability_mode` and `block_writes` settings
are both lost.

The cause, read in the source of `libsql-server-v0.24.33` (`main` has the same code): when
namespaces are disabled, every start of a primary calls `namespace_store.create(default,
RestoreOption::Latest, Default::default())` (`libsql-server/src/lib.rs`). `create` stores the
configuration it is given in the meta store (`libsql-server/src/namespace/store.rs`). That replaces
the configuration the meta store loaded from disk with `DatabaseConfig::default()`.

## Run it

```sh
SQLD_TAG=v0.24.33 sh run.sh      # Docker and curl
docker compose down -v           # remove the container and its volume
```

`run.sh` starts one primary from `ghcr.io/tursodatabase/libsql-server`, with its admin API on
port 9090. It sets `durability_mode` to `strong` and reads the setting back. Then it restarts sqld
three ways (`SIGKILL` then a start, a clean stop then a start, and `docker compose restart`) and
reads the setting back after each restart. In a second part it sets `block_writes` and does one
clean restart. `PRAGMA synchronous` shows what a connection uses: 1 is `NORMAL` (the `relaxed`
mode) and 2 is `FULL` (the `strong` mode).

## Output on 2026-11-08

Docker Engine 28.4.0 (an Ubuntu 24.04 arm64 VM) on an Apple Silicon Mac. The image is `v0.24.33`, arm64, digest
`sha256:f8ca0134cdde…`; `sqld --version` prints `sqld 0.24.33 (40a151bd 2025-12-19)`. Fields
that never changed (`block_reads`, `max_db_size`, `heartbeat_url`, `jwt_key`, `allow_attach`) are
left out of the lines below.

```
fresh start                   "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
== 1. durability_mode=strong, then restarts
after POST strong             "block_writes":false,"txn_timeout_s":null,"durability_mode":"strong"   PRAGMA synchronous: 2
5 s later                     "block_writes":false,"txn_timeout_s":null,"durability_mode":"strong"   PRAGMA synchronous: 2
after SIGKILL and start       "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
after POST strong again       "block_writes":false,"txn_timeout_s":null,"durability_mode":"strong"   PRAGMA synchronous: 2
after clean stop and start    "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
after docker compose restart  "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
== 2. block_writes=true, then a clean restart
after POST block_writes       "block_writes":true,"block_reason":"maintenance","durability_mode":"strong"
                              INSERT: "error":{"message":"Operation was blocked: maintenance","code":"BLOCKED"}
after clean stop and start    "block_writes":false,"block_reason":null,"durability_mode":"relaxed"   PRAGMA synchronous: 1
                              INSERT: ok
```

The same result was seen earlier on the build machine (x86_64, the amd64 image of the same tag),
with a `kill -9` and with a clean stop.

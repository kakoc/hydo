# The minimum path: your own binaries, no Compose file

This page is the least you need to write a first `hydo/` folder for a system whose services are
programs built from the project's source. Write the folder from it, run `hydo build hydo/hydo.yaml
--check`, and read the full topic named in each section when a check or a run fails.

## 1. The binaries

The guest has no network, so every program is a file in the image. Build each one as a static
Linux binary for the guest: arm64 on a Mac, x86_64 on Linux. The section "Where each program
comes from" of `hydo guide workflow` has the recipes for Go, Rust, and C. List each file under
`root.files`:

```yaml
root:
  files:
    - {src: ../target/aarch64-unknown-linux-musl/release/node, dst: /bin/node, mode: "0755"}
```

`src` is relative to `hydo.yaml`.

## 2. The services

One entry per long-running process (`hydo guide services`):

```yaml
network: bridge                  # each service gets its own address and network namespace
services:
  - name: node1                  # lowercase; fault tokens name it (K=node1@5+2)
    cmd: /bin/node --bind 10.89.0.11:7000 --data /data/node1   # in the foreground
    setup: mkdir -p /data/node1  # before every start; idempotent
    ready: tcp:7000              # ready when a connection to the port succeeds
    ready_timeout: 60            # seconds
    port: 7000                   # network faults act on it
    addr: 10.89.0.11             # numeric; peers reach the service by its name
    depends_on: []               # started after these
    # start_after: started       # on the 2nd and later members of a cluster that needs quorum
    restart: on-failure          # as in production (Docker Compose's words)
data:
  dirs: [node1]                  # under /data, the persistent disk that faults act on
guest:
  ram_mib: 2048                  # size it with `hydo run ... --memory`
```

## 3. The commands

One executable file in `hydo/workload/` per client operation (`hydo guide composer`). The
filename sets when it runs:

| filename | runs |
|---|---|
| `first_<name>` | once, before the faults: create topics, tables, users |
| `parallel_driver_<name>` | repeatedly, several at once, under faults |
| `serial_driver_<name>` | repeatedly, one at a time, under faults |
| `anytime_<name>` | now and then, alongside the drivers |
| `eventually_<name>` | once after the faults stop |
| `finally_<name>` | at the end, when every command finished |

A driver exits `0` when the operation succeeded, `100` when it saw a violation (the run fails),
and anything else when the operation failed (recorded, not fatal). Use `$HYDO_SEED` for every
random choice and `$HYDO_CMD_ID` as the operation id. Append the id of every acknowledged
operation to the file `$HYDO_ACKED`.

## 4. The checks

One executable file in `hydo/checks/` per property (`hydo guide checks`). `final_<name>` runs at
the end, also after a power cut. It exits `0` (holds), `1` (violated) or `2` (cannot tell now),
and its last output line is the explanation. For example, it reads back every id in
`$HYDO_ACKED` and fails when one is missing.

To let hydo judge a history instead, each command writes two records: one before the request and
one after the reply. The shell recorder `hydo_hist` comes from `/etc/hydo/prelude.sh`:

```sh
hydo_hist kv $HYDO_CMD_ID invoke write k1 5      # HYDO-HIST <history> <process> <type> <time> <op> <args>
hydo_hist kv $HYDO_CMD_ID ok write k1 5          # ok, fail (certainly no effect) or info (unknown)
```

A message queue or a stream records sends, receives and acknowledgements for a `messages` check
(`hydo guide messages`, with ready-made bundles of promises).

## 5. The first runs

```sh
hydo build hydo/hydo.yaml --check                 # after every file, until it prints OK
hydo build hydo/hydo.yaml
hydo run "disk=none dseed=1 | run" --memory       # must end HYDO-ORACLE: OK; prints the memory peaks
hydo run "disk=none dseed=1 | run"                # again: the same fp= fingerprint
hydo explore --seeds 1-20 --strategy process      # schedules with faults
hydo report
```

When a run fails, read the `hydo: diagnosis:` line first. Each service's log is then in the run
directory as `logs/<service>.log` (`hydo guide inspect`).

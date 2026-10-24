# hydo.yaml — the project manifest

A project is onboarded by writing ONE file, `hydo.yaml`. `hvx/onboard/hydo-build` compiles it
into a guest image, and one generic guest init (`hvx/onboard/hydo-init.sh`) runs any manifest:
it starts the services, runs the workload under the explorer's faults and checks the
invariants. No per-project build script or boot script is written by hand.

This file is the reference for people and for tools that generate manifests (a Claude skill
that reads a repository and writes `hydo.yaml` is the intended next step). Complete
examples: `examples/redis/hydo.yaml` (one service, about 60 lines),
`examples/stack/hydo.yaml` (MySQL + Kafka + Redis + a gRPC front door),
`examples/redis-oci/hydo.yaml` (Redis from the official Docker image, no file list),
`examples/etcd/hydo.yaml` (a replicated system: a three-member etcd cluster, with link faults
between members), and
`hvx/knownbugs/sqlite/hydo.yaml` (no service: a multi-threaded program using SQLite; one
manifest, two versions via `params:`).

## What the user writes, and what is automatic

You write: which files make up the guest (store paths, host files, or a Docker/OCI image per
service), how each service
starts and how to tell it is ready, a workload split into numbered steps, the invariants as
shell commands, and optionally the durability settings the explorer may relax.

Automatic, for every project: the read-only ext4 root and the journaled ext4 data disk
(byte-reproducible), busybox, the deterministic CRNG seed, loopback networking, a fixed
start date for the clock, starting services in dependency order with readiness waits, the
explorer protocol (snapshot points S0/S1, one command per stop), every fault kind — kill,
pause, network loss, one-way loss, power cut, clock jump, disk faults — applied by service
name, restarting crashed services, HYDO-ASSERT records for every invariant, and the
HYDO-ORACLE verdict line the explorer reads.

## How a run goes

1. Boot. The init mounts `/proc`, `/sys`, `/dev`, `/tmp`, seeds the CRNG, brings `lo` up.
2. **S0** (snapshot point). The clock is set to `guest.clock_start`.
3. Provision: mount the data disk on `/data`, create `data.dirs`, start every service (in
   order), run `provision`, `sync` (provisioning is never part of the window under test).
4. **S1** (snapshot point). The explorer types one schedule, e.g. `run K=redis@5+3 C=12`.
5. Workload: steps `0 .. steps-1` run in ranges; between ranges the faults of the schedule
   are applied at their step numbers. `every:` invariants run in the background.
6. Without a power cut: faults are healed, dead services restarted, `workload.retry` runs,
   then every `final` invariant is checked and ONE verdict line is printed.
   With a power cut: everything freezes; only data that reached the disk survives; a fresh
   VM from S0 mounts that disk, starts the services (crash recovery; services with
   `recovery: false` are skipped) and checks the `final` invariants.

Without `hydo_phase=serve` on the kernel command line the image runs the same steps once, with
no faults ("solo").

## Schema

All keys are optional unless marked **required**. Unknown keys are errors (typos do not
silently do nothing).

```yaml
name: myapp                    # required. lowercase letters, digits, '-'
description: one line          # free text, anywhere below too

guest:
  ram_mib: 1024                # guest RAM in MiB (default 1024): 256..3072 on x86_64, 256..4096 on arm64
  clock_start: 1767225600      # unix seconds the realtime clock starts at (default 2026-01-01)
  clock_after_cut: 60          # optional: after a power cut the recovery boot's clock continues from the cut, + 60 s
  cpu: v3                      # guest CPU level (x86_64 only): v2 (default) = x86-64-v2, no AVX;
                               # v3 = adds AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE, XSAVE

root:                          # what goes on the read-only root filesystem
  size_mib: 1792               # default: 1.3 x the content, rounded up to 64 MiB
  nix: [/nix/store/...-mysql-8.4.10]   # store paths; each FULL closure is copied to the
                                       # same path, so binaries run unchanged
  files:                       # host file or directory -> guest path
    - {src: build/myapp, dst: /bin/myapp, mode: "0755"}   # src relative to hydo.yaml, or ~ / absolute
  text:                        # small files written inline
    /etc/myapp.conf: |
      port 8080
  dirs: [/etc/myapp]           # empty directories to create

data:                          # the read-write disk, mounted on /data; the fault target
  size_mib: 512                # default 512
  mount: rw                    # ext4 mount options
  dirs: [db, logs]             # created under /data on every mount

env:                           # exported to every command (services, workload, invariants)
  MYAPP_HOME: /opt/myapp

shell: |                       # shell helpers, sourced before every command
  SQL() { mysql -h127.0.0.1 -uroot -N -B "$@"; }

params:                        # build parameters: {{name}} in any string value below is replaced
  version: "3.51.2"            #   by the default here or by `hydo-build --set version=3.51.3`.
                               #   One manifest then builds several versions of a project (a
                               #   buggy and a fixed release). Values are strings; a {{name}}
                               #   that is not declared, or a --set of an undeclared name, is an
                               #   error. The values are recorded in the image as /etc/hydo/params.

services:                      # may be empty ([] or left out): a project that is only a
  - name: db                   #   workload program (e.g. a library under test). Started in this
                               #   order, moved only as far as depends_on requires.
    # name: required, [a-z][a-z0-9_]* -- it is also the name fault tokens use (K=db@...)
    cmd: /bin/mydb --data /data/db        # required. runs in the foreground; its output
                                          #   goes to $HYDO_LOG (/tmp/hydo/<name>.log)
    setup: '[ -d /data/db/x ] || /bin/mydb --init'   # run before EVERY start (incl. restarts
                                          #   after a kill and crash recovery); make it idempotent
    ready: /bin/mydb-cli ping             # required. exit 0 = ready; polled once per second
    ready_timeout: 120                    # seconds (default 120)
    port: 5432                            # the port network faults (N=, O=) act on
    addr: 127.0.0.11                      # the service's own loopback address: enables link
                                          #   faults between services (see "Link faults")
    depends_on: [cache]                   # start after these
    recovery: true                        # false: not started for the power-cut recovery check

  - name: cache                # a service whose files come from a Docker/OCI image (see below)
    image: images/redis.tar    # a `docker save` tar or an OCI layout directory, relative to
                               #   hydo.yaml; or {x86_64: <path>, arm64: <path>}
    image_ref: redis:7.2.4     # which image in the file: a tag or sha256:<digest> (needed only
                               #   when the file holds more than one)
    cmd: redis-server --appendonly yes   # optional: replaces the image's CMD (string = shell
                               #   words, list = argv); the ENTRYPOINT stays
    entrypoint: [/bin/sh, -c]  # optional: replaces ENTRYPOINT (and, as in Docker, drops CMD)
    env: {TZ: UTC}             # optional: added to / overriding the image's Env
    workdir: /data             # optional: default the image's WorkingDir
    user: "999:999"            # optional: default the image's User ("" = root)
    ready: hydo-oci cache redis-cli ping | grep -q PONG

provision: /bin/myapp-admin create-schema   # once, after all services are ready, before S1

workload:                      # required
  steps: 30                    # required, 1..1000. Faults are placed at step numbers.
  run: /bin/loadgen $HYDO_FROM $HYDO_TO     # required. runs steps [HYDO_FROM, HYDO_TO);
                               #   called several times per run (once per fault-free range)
  retry: /bin/loadgen --retry-unacked       # after faults heal (no power cut), optional
  timeout: 300                 # seconds per run/retry call; then it is killed (default 300).
                               #   A client without its own deadline hangs on a paused service.
  process: loadgen             # pattern (pgrep -f) telling whether a request is in flight
                               #   (used by mid-request pauses); default: run's first word

knobs:                         # durability settings the explorer may relax
  - token: M                   # one capital letter, not K N C P T O Q X (built-in schedule tokens)
    name: MySQL innodb_flush_log_at_trx_commit
    default: "1"               # required. the value the image starts with (the safe one)
    set: SQL -e "SET GLOBAL innodb_flush_log_at_trx_commit=$HYDO_VALUE"   # required
    values: ["0", "1", "2"]    # documentation for generators
    safe: "1"

invariants:                    # required, at least one
  - id: orders.exactly_once    # required. [A-Za-z0-9_.-]
    msg: every acknowledged order is stored exactly once
    type: always               # always (default) | always_or_unreachable | eventually
    cmd: /bin/check-orders $HYDO_ACKED      # required. exit 0 = holds, 1 = violated,
                               #   2 = could not be evaluated now (e.g. the db is down)
    every: 2                   # also check every 2 s of guest time during the workload
    final: true                # check at the end (default: true unless every: is given)
  - id: orders.drained         # bounded liveness (see "Liveness" below)
    type: eventually
    cmd: /bin/count-pending | grep -qx 0      # exit 0 = satisfied now, anything else = not yet
    after: heal                # anchor: heal (default) | start
    within: 10                 # required: deadline in whole seconds of guest time (1..3600)
    poll: 1                    # seconds between checks (default 1); no every:/final: here

faults: [kill, pause, network, oneway, cut, clock, disk]   # which kinds make sense here (also
                               # freeze, protocol; see their sections);
                               # network/oneway also pull iproute2 into the image
```

`buggify: true` (top level, optional) says the project's programs carry BUGGIFY fault points
from an assertion SDK (Go `hydoassert.Buggify`, Rust `hydo_assert::hydo_buggify!`, JavaScript `buggify`; see
`hvx/ASSERTIONS.md`). The init then turns the schedule token `B=<seed-hex>[,<enable-pm>[,<fire-pm>]]`
(drawn by `explore.py --buggify`, which keeps `B=` for such a manifest and drops it for others)
into `/tmp/hydo-buggify`, the file the SDKs read at a
program's first fault-point evaluation; without `B=` the file is absent and every point stays
off. The token `B` is then reserved (no knob may use it). Opt-in: `hydo-init-buggify.sh` is
spliced into the init only for a manifest that sets it, so other images build as before.

### Services from a Docker/OCI image

A service with `image:` takes its files from an image instead of `root.nix`/`root.files`.
hydo-build never pulls: the image is a file on the build machine. Make one with any of

```sh
docker save -o redis.tar redis:7.2.4                       # Docker (classic or >= 25 format)
skopeo copy docker://docker.io/library/redis@sha256:<manifest digest> \
       docker-archive:redis.tar:redis:7.2.4                # skopeo, a docker-save style tar
skopeo copy docker://docker.io/library/redis@sha256:<digest> oci:redis-oci:redis:7.2.4
                                                           # skopeo, an OCI layout directory
```

(gzip-compressed tars and OCI layouts holding a multi-platform index work too; the build picks
the `linux/<arch>` manifest. zstd layers are not supported by the build box's Python: re-export
with `skopeo copy --dest-compress-format gzip`.) Pin by digest: `skopeo copy ...@sha256:...`,
and/or `image_ref: sha256:<manifest digest>` (OCI layout) or `sha256:<image id>` (docker save).
Every blob is checked against its digest and every layer against the config's `diff_ids`.
`examples/redis-oci/fetch-image.sh` is a complete example.

What the build does:

- **A root per service, not a merged root.** The layers are applied in order (whiteouts
  `.wh.<name>` and opaque directories `.wh..wh..opq` honoured) into `/oci/<service>` on the
  read-only root, and the service runs chrooted there, as in Docker. Images with different C
  libraries can coexist, and busybox, `/sbin/init` and the rest of the hydo root are untouched.
- `/proc`, `/sys`, `/dev`, `/tmp` and `/data` of the guest are bind-mounted into that root.
  `/data` is the data disk (the fault target): point the program's data directory there (it
  is the image's `WorkingDir` in many official images). `/tmp` is shared, so `$HYDO_ACKED`
  etc. are the same files inside. `/etc/hosts` in the image is replaced by
  `127.0.0.1 localhost` (Docker also writes it at run time).
- **Defaults from the image config, overridable in the manifest:** `ENTRYPOINT` + `CMD`
  (Docker semantics: `cmd:` replaces `CMD`; `entrypoint:` replaces `ENTRYPOINT` and drops
  `CMD`), `Env` (`env:` adds or overrides by name; `HOME` is the user's home from the image's
  `/etc/passwd`; `PATH` defaults to Docker's), `WorkingDir` (`workdir:`), `User` (`user:`,
  name or uid[:gid], switched with busybox `chpst`). The manifest's top-level `env:` and
  `shell:` are NOT exported into the image (they belong to the hydo side); the `HYDO_*`
  variables are.
- **Running commands inside the image:** `hydo-oci <service> <command> [args]` runs a command
  in the service's root with its env and working directory, as root. Use it in `ready:`, the
  workload, knobs and invariants (`hydo-oci cache redis-cli GET k`). A static busybox is put at
  `/.hydo/busybox` in each image root for `env -i`, `cd` and the user switch, so images without
  a shell (distroless) work.
- A killed, paused or frozen image service is the same process tree as any other service (the
  launcher `exec`s down to the program), so every fault applies unchanged.
- **Reproducible:** owners, groups and modes (including setuid) come from the layer tars and
  are written into the ext4 root with the pinned `debugfs` (x86_64) or into the cpio headers
  (arm64); timestamps are normalised like every other file (fixed mtime, zeroed
  ctime/atime/crtime). Two builds from the same image file are byte-identical; the files
  under `/oci/<service>` depend only on the image's content (`image.json` also records where it
  came from). Device nodes, FIFOs and extended
  attributes (file capabilities) in layers are skipped; a path containing `"`, `\` or a newline
  is refused.
- **Architecture:** the image config's `architecture` must match the build (`amd64` for the
  default x86_64 build, `arm64` for `--arch arm64`); anything else is refused with an error.
- Inside the image: `/etc/hydo/oci/<service>/run` (the launcher), `exec` (used by `hydo-oci`),
  `image.json` (digests, layers, the image config and the effective command/env/workdir/user).

### Variables every command sees

| variable | meaning |
|---|---|
| `HYDO_FROM`, `HYDO_TO` | the workload step range (`workload.run` only) |
| `HYDO_STEPS` | `workload.steps` |
| `HYDO_ACKED` | file: one id per line for every operation the client saw acknowledged. The workload appends; invariants read. Carried across a power cut. |
| `HYDO_OUTCOMES` | file: `<id> ack|failed|unknown` per attempt (the last line per id wins) |
| `HYDO_FAILED` | file: ids whose last outcome was `failed` (computed before the check) |
| `HYDO_LOG` | the service's log file (`setup`/`cmd`/`ready` only) |
| `HYDO_SVC` | the service's name |
| `HYDO_VALUE` | the knob value (`knobs[].set` only) |

### Invariants

An invariant is a shell command. Exit status 0 means the property holds, 1 means it is
violated, 2 means it could not be evaluated right now. Its last output line (or its own
`HYDO-ORACLE:` line, if it prints one) becomes the details.

- Every evaluation becomes a `HYDO-ASSERT` record (`hvx/ASSERTIONS.md`), once per id and
  outcome, so `hydo_assert_report` and the explorer's property table include them.
- `every:` invariants run while the workload runs. A violation is remembered and fails the
  run at the end.
- `final` invariants run once at the end (same boot, or the recovery boot after a power cut),
  in manifest order. The verdict is `HYDO-ORACLE: VIOLATION [<id>] <details>` for the first
  violated one, `HYDO-ORACLE: ERROR invariant <id> could not be evaluated` if one returned 2,
  else `HYDO-ORACLE: OK invariants=<n> held (<first id>: <details>)`.
- Use `always_or_unreachable` for a check that may legitimately never get to run (a periodic
  query while its service is killed).

### Liveness (`type: eventually`)

A safety invariant says "never X"; a liveness invariant says "X happens". hydo checks the
bounded form: **after an anchor, the check succeeds within `within` seconds of guest time**.
Guest time is deterministic under hvx, so the verdict and the measured time are the same on
every run of a schedule. Anchors:

| `after:` | the deadline counts from |
|---|---|
| `heal` (default) | the end of fault injection: the workload's last step has run. Everything the init then does to heal -- lift network faults, end pauses, restart killed services, the workload's `retry` -- counts against the deadline. After a power cut: the start of the recovery boot (the services' crash recovery counts). Solo: after the last step. |
| `start` | the start of the workload (after a power cut: the start of the recovery boot) |

From the anchor the init polls `cmd` in the background every `poll` seconds (each call bounded
by `within + 1` s). Exit 0 = satisfied; any other status = not yet. At the end (`finish`,
before the `final` invariants) it waits for the result and emits ONE `HYDO-ASSERT` record of
type `eventually` with `details` `{"after","elapsed_s","out","within_s"}`: `ok:true` if the
check succeeded by the deadline, `ok:false` if the deadline passed first. A missed deadline is
a violation: the verdict is `HYDO-ORACLE: VIOLATION [<id>] not satisfied <t>s after heal,
deadline <d>s (last check rc=1: <its last line>)`. Liveness results come before the `final`
invariants, so when both fail the verdict names the liveness one; the OK line's
`invariants=<n>` counts the `final` invariants only.

Deadlines are in seconds, not workload steps: after the heal no steps run, and a step's
duration depends on the faults. A fault that keeps a service unavailable after the workload
ends (for example a timed pause `P=<svc>@<at>~<off>:<dur>` longer than the remaining
workload -- the init waits for it before restarting anything) makes a `heal` deadline miss
deterministically.

Only images whose manifest has an `eventually` invariant get the liveness code: `hydo-build`
then inserts `hvx/onboard/hydo-liveness.sh` into `/sbin/init` before its `main` section, with
renamed copies of the four init functions it wraps (`step_range`, `start_periodic`,
`start_all`, `finish`). Every other image's `/sbin/init` is `hydo-init.sh` byte for byte.
`hydo-build --print-init` shows the generated init. Example: `examples/redis/hydo-live.yaml`.

The program under test can still use the assertion SDK (`hvx/stack/hydoassert`) for
properties that need to see inside it; both kinds of record mix on the console.

### Fault tokens (what the explorer types at S1)

| token | effect |
|---|---|
| `K=<svc>@<at>+<down>` | SIGKILL the service before step `at`; restart it (setup + cmd + ready) before step `at+down` |
| `P=<svc>@<at>+<n>` | SIGSTOP before step `at`, SIGCONT before step `at+n` |
| `P=<svc>@<at>~<off>:<dur>` | pause after `off` ms of guest time for `dur` ms, while steps run; `off` may have a fraction down to microseconds (`60589.25`) |
| `N=<svc>@<from>-<to>:<netem>` | `tc netem` on traffic to the service's `port` for steps `[from,to)` |
| `O=<svc>@<from>-<to>:req\|resp:<netem>` | one direction only |
| `C=<at>` | power cut before step `at` |
| `T=<at>:<delta>` | step the realtime clock by `delta` seconds |
| `N=<a>-<b>@...`, `N=<a>-*@...` | netem on the link between two services with `addr:` (or between `a` and every other one); see "Link faults" |
| `O=<a>-<b>@<from>-<to>:req\|resp:<netem>` | one direction of a link: `req` = packets from `a` to `b`, `resp` = from `b` to `a` |
| `F=<svc>><peer>@<step>[#<n>][:<match>]:<dur>` | freeze `<svc>` at the n-th packet it **sends** to `<peer>` (`<` = **receives** from) at/after step `<step>`, for `<dur>` ms; only with `freeze` in `faults:` (see "Freeze at a call") |
| `H=<target>@<from>-<to>:<rule>[,<rule>...]` | a protocol fault on a fake or proxy for steps `[from,to)`: an error, slow, missing or reset answer, or a duplicated / reordered / late / lost webhook; see "Fake outside services" |
| `<X>=<value>` | a knob from `knobs:` |
| host side: `disk=ioerr:<p>` / `lying:<p>` | disk I/O errors / a disk that drops acknowledged writes |

Lifting a network fault (`N=`/`O=`, at `to` or at the end of the workload) does not drop
the packets still waiting in netem's delay queue: the init first changes the netem to no
delay/loss/duplication/corruption (queued packets keep their send times, new packets follow
them in order), waits until the queue is empty (at most the largest delay in force plus
500 ms of guest time), then removes the qdisc. The console shows
`network fault lifted: queued=<n> drained_ms=<ms> left=<n>`. Before 2026-10-23 the qdisc was
deleted at once and every queued packet was lost, so TCP retransmitted after a timeout.

A token naming a service the manifest does not have is ignored. `explore.py` draws its
faults from the manifest (`hvx/explore/manifest_space.py`): kills and pauses from all
services, network and one-way faults from services with a `port:`, knob values from
`knobs:`, fault placement from `workload.steps`, fault kinds from `faults:`. It reads
`/etc/hydo/hydo.yaml` out of an `explore-ob-*` image automatically; pass `--manifest` for
any other image. A knob token that collides with a built-in token (K P N O C T Q X F) is
rejected. `explore.py --freeze-call` draws `F=` for a manifest whose `faults:` lists `freeze`;
`explore.py --grpc-faults` draws gRPC-level `H=` tokens for a manifest with a gRPC proxy (see "gRPC faults").

### Freeze at a call (stop a service at an exact point of its traffic)

Some bugs need a service stopped at one precise moment of a conversation -- between two
messages, not at a step boundary. List `freeze` in `faults:` and the explorer (and a hand
schedule) can use `F=<svc><dir><peer>@<step>[#<n>][:<match>]:<dur>`:

- `<dir>` is `>` (packets `<svc>` sends to `<peer>`) or `<` (packets it receives from `<peer>`);
- counting starts when workload step `<step>` begins; `<n>` (default 1) selects the n-th packet
  that carries payload in that direction (and, with `<match>`, contains those bytes; `%xx`
  escapes a byte). `#0` fires nothing and dumps a compact trace of the pair's traffic
  (`hydo-fc-trace:` lines) so you can read off the `<n>` of an event;
- at that packet the whole process tree of `<svc>` is SIGSTOPped -- held before it runs past the
  packet, which is then let through -- and SIGCONTed after `<dur>` ms of guest time.

It is deterministic under hydo, so the freeze lands on the same instruction every run. A packet
is held in a netfilter queue (`nfnetlink_queue` + the static helper `hydo_fc`, pinned
`iptables-legacy`); nothing of this is staged unless `freeze` is in `faults:`, so every other
image builds byte-identically. `freeze_match:` at the top level lists payload strings the
explorer may draw as `<match>` (so a known event has a small `<n>`): either a plain list, or a
mapping `replies: [...]` (strings that occur in packets the frozen service receives) and
`requests: [...]` (in packets it sends); a drawn string then gets its own direction. Which
`(svc, peer)` pairs the explorer freezes: a peer with a `port:` that `<svc>` reaches through its
`depends_on`; without such pairs, every ordered pair with a listening peer; and when no service
calls another (a single server), the peer `workload`: the workload's own client connections from
`127.0.0.1` to `<svc>`'s port (`F=redis<workload@3#2:500`; `examples/redis/hydo-freeze.yaml`).
Not supported yet: `--arch arm64`.

A random draw rarely lands on the one packet a narrow-window bug needs. `explore.py --sweep freeze`
tries every packet instead: for each packet stream (a pair, a direction, and either every packet
or one `freeze_match:` string) and each swept step `s` it first runs the schedule once with `#0`
and reads the guest's "freeze never fired: <N> of 0 matching events after step <s>" line, so N(s)
is the stream's packet count from step `s` to the end of fault injection. Step `s` then holds
N(s) - N(s+1) packets (the last step N(s)), and the sweep runs one schedule
`F=<svc><dir><peer>@<s>#<n>[:<match>]:<dur>` per packet n. Options: `--fc-pairs a>b,...`,
`--fc-dirs '<>'`, `--fc-steps 8-9`, `--fc-match all,manifest` (default: every packet plus each
`freeze_match:` string in its own direction), `--fc-durs 150000`, `--sweep-every k` (every k-th
packet), `--sweep-with "<tokens>"` (one more fixed fault on every point and trace run, e.g.
`K=kafka@9+1`; not a power cut, whose trace run never reaches the count), `--sweep-dry` (trace,
write `points.txt`, stop). Every failure is re-run twice. Trace counts are in `fc-traces.jsonl`,
point labels in `sweep-points.jsonl` (`compare.py sweep <out>` tabulates them).

### Clock after a power cut (`guest.clock_after_cut`)

Every serve boot sets the realtime clock to `guest.clock_start`, the recovery boot after a power
cut included. A check that compares a time written in the first boot with one written in the
recovery boot (for example a broker append time against the time an engine processed an event
only after the restart) then sees time run backwards and can report a false violation.
`guest.clock_after_cut: <seconds>` makes the recovery boot continue the first boot's clock
instead: at the cut the first boot adds `clock=<epoch>` to its `hydo-explore-acked:` line, which
the harness passes on in the recovery boot's `oracle` command, and the recovery boot starts its
clock at that epoch plus `<seconds>` (the time the machine was off), as a real
clock keeps running across a power loss. Opt-in: `hydo-init-clock.sh` is spliced into the
init only for a manifest that sets it.

### Link faults (a network fault between two named services)

`N=` and `O=` on a service act on its `port:`, i.e. on traffic from every client to that service.
A replicated system also needs faults on the link between two members, leaving the others
connected (a partition). For that, give each member an `addr:`: its own loopback address
(`127.x.y.z`, not `127.0.0.1`, unique). Then a fault token may name a **link**:

- `N=a-b@<from>-<to>:<netem>`: netem on every packet between `a`'s and `b`'s addresses, both ways;
- `N=a-*@...`: the same between `a` and every other service with an `addr:` (isolates `a`);
- `O=a-b@...:req:<netem>` / `:resp:` : only the packets from `a` to `b` / from `b` to `a`.

hydo-init matches on the IP source and destination (tc u32 on `lo`), so the packets must carry
the members' addresses at both ends: each member listens on its `addr:` and its **outgoing**
connections must leave from it too. On loopback the kernel picks `127.0.0.1` as the source unless
the program binds its dialer or the routing says otherwise; `examples/etcd/hydo.yaml` shows a
recipe that needs no change to the program: run each member as its own user and add a policy
routing rule per uid (`ip rule add uidrange …`, a table whose `local 127.0.0.0/8` route has
`src <addr>`). Clients that connect from `127.0.0.1` are not touched by a link fault.

With two or more services that have `addr:`, `explore.py` (manifest_space.py) adds every pair
`a-b`, and with three or more every `a-*`, to the targets its network and one-way faults pick
from. The extension lives in `hydo-init-links.sh`; hydo-build copies it into the image and adds one
line to `/sbin/init` that sources it, only for manifests with an `addr:`, so every other
manifest builds a byte-identical image. Not supported yet: `--arch arm64`.

## Fake outside services and protocol faults (`fakes:`, `proxies:`, `H=`)

Business software mostly fails at its edges: a payment provider answers 500 or 429, answers too
late, takes the money and never answers, sends a webhook twice or out of order. `fakes:` declares
the outside services the system talks to; hydo serves them **inside the guest** (one static
program, `/bin/hydo-world`, source `hvx/onboard/world/`), so they are as deterministic as
everything else. Every request a fake (or proxy) receives is **recorded**, so invariants can ask
what the outside world saw: "the customer was charged at most once per order". `proxies:` puts
the same faults and records in front of a real service of the manifest.

```yaml
fakes:
  - name: payments             # [a-z][a-z0-9_]*, not a service name; H= tokens and checks name it
    port: 9001                 # required; listens on addr:port
    addr: 127.0.0.1            # default 127.0.0.1 (any 127.x.y.z)
    protocol: http             # http (default: HTTP/1.1 and cleartext HTTP/2) | grpc (see below)
    idempotency: header:Idempotency-Key   # where a request's idempotency key is (extractor)
    ref: json:order            # the business reference a request is about (extractor); per route too
    routes:                    # required for a fake; the first match wins
      - name: charge           # [a-z][a-z0-9_]*; the target <fake>.<route> of H= and of checks
        method: POST           # default: any
        path: /v1/charges      # exact, a prefix /v1/*, or with segments /v1/orders/{id}
        status: 200            # the answer (default 200), with headers: and a templated body:
        headers: {Content-Type: application/json}
        body: '{"id":"ch_{seq}","order":"{ref}","amount":{json:amount}}'
        idempotent: true       # a repeat with the same key gets the first answer, no new effect
        effect: true           # does a request here act in the world (default: not GET/HEAD/OPTIONS)
        webhook:               # after an effect, call the system back (asynchronously)
          url: http://127.0.0.1:8080/webhooks/payments
          body: '{"type":"charge.succeeded","id":"ch_{seq}","order":"{ref}"}'
          delay_ms: 20         # after the effect
          retries: 5           # a failed (non-2xx) delivery is retried after 0.5, 1, 2, ... s
proxies:
  - name: redisp               # a fault proxy in front of a real service
    port: 6380                 # the system talks to this port instead of the service's
    service: redis             # forward to that service's addr:port (or to: 127.0.0.1:6379)
    protocol: tcp              # tcp (any protocol; one connection = one request) | http | grpc
    # http/grpc proxies may have ref:, idempotency: and routes: (name/method/path/ref/effect only),
    # which name the requests in the records and as H= targets
```

**Extractors** (`idempotency:`, `ref:`): `header:<Name>`, `json:<field>` (dotted path into a JSON
body), `query:<name>`, `form:<name>`, `path:<name or 1-based segment>`, `pb:<field number>` (a
top-level field of the first gRPC message: a string or a varint), `body` (a hash of the body).

**Templates** (answer bodies, header values, webhook url/body/headers): `{seq}` (the record number
of the request; replays use the original's), `{ref}`, `{idem}`, `{fake}`, `{route}`; in answers
also `{json:a.b}`, `{header:X}`, `{query:x}`, `{path:x}`, `{form:x}`, `{pb:n}` from the request.
Single braces: `{{name}}` is a build parameter (`params:`). A webhook may use only the first five
(it can be re-sent after a power cut, without its request).

**gRPC** (`protocol: grpc`): cleartext HTTP/2 with prior knowledge. A route's `path` is the method
(`/pkg.Service/Method`), its answer `message:` (hex protobuf bytes, default empty) and `code:`
(gRPC status, default 0); an error rule answers trailers-only with `grpc-status`. Status codes in
`H=` rules may be numbers or names (`status=UNAVAILABLE`).

### The record (what the outside world saw)

`/tmp/hydo/world.log` in the guest, one line per event, in arrival order, fields `k=v` (values
%-escaped):

```
seq=7 t=<unix ms> kind=req ep=payments route=charge from=127.0.0.1 method=POST path=/v1/charges
      ref=o2 idem=charge-o2 body=<hash> fx=1 out=ok+delay3000 rule=1
seq=8 ... kind=req ... fx=0 out=replay+delay3000 orig=7 rule=1      (an idempotent repeat)
seq=11 ... kind=hook ep=payments route=charge ref=o2 src=7 n=1 dup=0 out=200   (webhook delivery)
seq=13 ... kind=event ep=- route=cancelled ref=o2 fx=1               (hydo-world event)
```

`fx=1` marks a request that took effect in the world (an effectful route, answered or not, not an
idempotent replay; for a proxy: forwarded to the service). `out` is how it was answered: `ok`,
`replay`, `fwd` (proxy), `-` (the world never acted: an error rule without `effect=yes`), plus
`+delay<ms>`, `+drop`, `+reset`, `+status<code>`. A webhook delivery `out` is the HTTP status the
system answered, `err`, `lost` or `gave-up`. Just before the verdict the console gets a compact
copy (`hydo-world: <seq> payments.charge ref=o2 idem=charge-o2 fx=1 ok+delay3000`, at most 150
lines).

The workload (or the system) can add its own events to the same ordered record:
`hydo-world event <name> <ref>` (for example `hydo-world event cancelled o2` once a cancellation
is acknowledged), which "no side effect after <event>" checks compare against.

### Side-effect checks

`hydo-world check <kind> <target> [options]` reads the record; use it as an invariant's `cmd:`
(exit 0 holds, 1 violated, 2 cannot decide; its `HYDO-ORACLE:` line becomes the details, so
`HYDO-ASSERT` records, the verdict, the explorer and the shrinker work unchanged):

| kind | holds when |
|---|---|
| `at-most-once <ep>.<route> [--by ref\|idem]` | no ref (or idempotency key) took effect twice |
| `effect-for-acked <ep>.<route> [--ref 'o{id}'] [--acked FILE]` | every acknowledged id (`$HYDO_ACKED`; `--ref` turns an id into the ref the effect carries) has an effect. As a `type: eventually` invariant: "every acknowledged request produced its side effect within N s" |
| `exactly-once <ep>.<route> [--ref ...]` | both of the above |
| `none-after <ep>.<route> --after event:<name>\|<ep>.<route>` | no ref took effect after that ref's first `<after>` record (no payment after its cancellation) |
| `count <ep>.<route>` | always; prints the number of effects |

`examples/shop/hydo.yaml` uses each of them.

### Protocol faults (`H=`)

```
H=<target>@<from>-<to>:<rule>[,<rule>...]
  target   <fake|proxy> (every route) or <fake|proxy>.<route>
  window   on while workload steps [from, to) run (switched at step boundaries, like N=), off when
           fault injection ends (and frozen with everything else at a power cut)
  request rules (what the answer to a matching request becomes):
    status=<code>       an error answer: an HTTP status, or for grpc a gRPC code (number or name)
    retry-after=<s>     with status=: a Retry-After header (429, 503)
    delay=<ms>          the answer comes <ms> of guest time late (combines with status= / drop / reset)
    drop                no answer: the request is held until the client gives up or the rule ends
    reset               the connection is reset instead of answered
  webhook rules (on the webhooks of effects made while the rule is on):
    dup                 delivered twice
    reorder             delivered after the next webhook of the same route (pairs swap)
    late=<ms>           delivered <ms> later
    lost                never delivered
  modifiers:
    p=<1..100>          share of the matching requests (or webhooks) the rule acts on (default 100)
    effect=yes|no       does the world still perform the side effect? Default: no for status=
                        (rejected before acting), yes for delay/drop/reset (the request arrived and
                        acted; only the answer is slow or lost). status=500,effect=yes = "charged,
                        then failed to say so"
```

On a `tcp` proxy a request is a connection: `delay` holds every chunk the service sends back,
`drop` lets the client's bytes through and never returns the answer, `reset` resets the connection
when the answer arrives (`effect=no`: nothing reaches the service); `status=` has no meaning there.

Determinism: which matching request a rule acts on is a pure function of the token text and the
request's index among those the rule has seen (`mix64(fnv64(token) + k) % 100 < p`); the order of
requests is the guest's, which hvx makes deterministic. Several `H=` tokens may be given; the first
firing request rule (in token order) decides a request, and every webhook rule that fires applies.
A target the manifest does not have is ignored (as for every fault token). `H` is reserved: no knob
may use it.

**Power cut.** A cut stops the system under test, not the outside world. At the cut the world is
frozen (a request that arrives later is never recorded) and its effects, events, idempotency memory
and undelivered webhooks are appended to the acknowledged ids as `w:` tokens (at most 3000 bytes;
`hvx_explore` carries that line into the recovery boot's `oracle` command, which has room for it
since this feature). The recovery boot's world starts from them: the checks see both boots, a repeat
of an old idempotency key is still a replay, and undelivered webhooks are re-sent. If the carried
record does not fit, it is replaced by `w:trunc` and every check answers 2 (cannot decide), never a
false verdict.

**Explorer.** `explore.py --protocol-faults` draws `H=` (one token on 60% of the schedules, a
second on 25%) on its own random stream, from the targets `manifest_space.py` lists: every fake
route (with the webhook rules when it has a webhook), every proxy and proxy route. Without the flag
every schedule string is byte-identical (`hvx/explore/protocol_faults_ident.py`).
`--sweep protocol` runs every rule kind (status, status with effect=yes, delay=3000, drop, reset,
and on webhook routes dup, reorder, late=5000, lost) on every target, one workload step at a time. The shrinker drops
`H=` tokens, narrows their window, and removes `p=`, `retry-after=` and `effect=`, then halves
delays. A manifest with a `faults:` list draws `H=` only if it lists `protocol`.

**Build.** `hvx/onboard/world/build.sh` builds `build/hydo-world` (pinned Go, standard library
only); `hydo-build` stages it (`HYDO_WORLDBIN` overrides), writes `/etc/hydo/world.json`
(`--print-world` shows it) and splices `hvx/onboard/hydo-init-fakes.sh` into the init. A manifest
without `fakes:`/`proxies:` builds a byte-identical image. Not supported yet: `--arch arm64`.

**Extending it (the code layout).** `world/rules.go`: the `H=` grammar (`parseRule`, one `Rule`
per token, `fires`, `applied`); a new rule is a field there plus its action. `world/serve.go`: one
handler per endpoint protocol -- HTTP and gRPC share `handler()` (decide under the world lock,
record, then act: delay / drop / reset / status / answer), `serveTCP` is the byte-stream proxy;
a new protocol is a new serve function that calls `lockGate`, `requestRule`, `record` in that order.
`world/world.go`: records, webhooks, the control socket (`on`/`off`/`heal`/`event`/`dump`/`summary`)
and the power-cut carry. `world/check.go`: the checks. `world/config.go`: `world.json`, extractors,
templates. The explorer's draws are `add_protocol_faults` (rule kinds per target come from
`manifest_space.Space._pf_targets`).

### gRPC faults (a fault proxy in front of a gRPC service)

A proxy with `protocol: grpc` (`proxies:`, above) sits between a gRPC client and a gRPC service. It is a streaming HTTP/2
proxy (cleartext HTTP/2 with prior knowledge, what gRPC clients speak without TLS): every call is
forwarded to the service as it arrives, message by message in both directions, so unary,
client-streaming, server-streaming and bidirectional calls all pass, and a fault can act after the
service has answered or in the middle of a stream. The client must dial the proxy's `port:`
(usually an environment variable or flag of the client); the service keeps its own port.

```yaml
proxies:
  - name: orders
    protocol: grpc
    port: 50052                 # the client dials this
    to: 127.0.0.1:50051         # or service: <name> (a service with a port:)
    ref: pb:1                   # the business reference: field 1 of the request message
    routes:                     # optional: name methods, so a fault can target <proxy>.<route>
      - {name: place, path: /hydo.OrderService/Place}
```

Faults are `H=` tokens (`H=<proxy>[.<route>]@<from>-<to>:<rule>[,<rule>...]`, on while workload
steps `[from,to)` run, off when fault injection ends), with these rules on a gRPC proxy:

| rule | what the client sees | did the server act? |
|---|---|---|
| `drop` | no answer; it waits until its deadline (or the fault ends, then its stream is reset) | yes: forwarded, the answer is discarded |
| `status=<code>` | that gRPC status, at once | no (not forwarded) |
| `status=<code>,effect=yes` | that status, after the server has finished | yes |
| `delay=<ms>` | the server's answer, ms of guest time late (past its deadline when ms exceeds it) | yes |
| `reset` (`reset=stream`) | its HTTP/2 stream reset (`INTERNAL`, RST_STREAM) when the server answers | yes |
| `reset=conn` | its TCP connection reset when the server answers: every call on it fails (`UNAVAILABLE`), the client reconnects | yes |
| `cut=<n>` | n response messages, then its stream reset (`dir=req`: n request messages reach the server, then both halves are cut) | partly |
| `hold=<n>` | n messages, then nothing more in that direction until the fault ends (then the stream continues) or the client gives up | partly |

`<code>` is a name (`UNAVAILABLE`, `DEADLINE_EXCEEDED`, `RESOURCE_EXHAUSTED`, `INTERNAL`,
`ABORTED`, or any other gRPC code name) or a number. `effect=no` on drop, reset or delay means the
call is not forwarded at all. `delay=` combines with any other rule (the delay comes first).
Which calls a rule acts on:

- `<proxy>.<route>`, or `rpc=<name>` on the proxy (`Method`, `Service/Method` or `/pkg.Service/Method`): one method;
- `call=<a>` or `call=<a>-<b>`: the a-th (to b-th) call of a method through this proxy, counted from 1
  since the guest booted (every call counts, faulted or not; the recovery boot after a power cut counts again from 1);
- `p=<1..100>` with an optional `seed=<n>`: that share of the matching calls; which ones is a hash of
  the token text and the call's index under the rule, so `seed=` picks another set.

Every decision is a pure function of the token and the order in which calls arrive, which hvx
makes deterministic: a schedule gives the same faults on every run. `rpc=`, `call=`, `cut=`,
`hold=`, `dir=` and `reset=conn` act on gRPC proxies only (hydo-world refuses them elsewhere);
on a gRPC fake (`fakes:` with `protocol: grpc`) select calls by route, with `status=`, `delay=`,
`drop` and `reset`.

**Records.** Each call gives two records in `/tmp/hydo/world.log`: at arrival `kind=req` (as for
every proxy, with `call=<k>`; `fx=1` = forwarded), and at its end

```
kind=call ep=orders route=place path=/hydo.OrderService/Place ref=7 call=8 req=<seq of the req record>
          status=<what the client got> server=<1|0|?> upstream=<the server's status> msgs=<in>/<out> out=fwd+drop
```

`status` is the gRPC status the client received (a number), `rst` (stream reset), `conn`
(connection reset) or `none` (the client gave up before any answer). `server=1`: the server
answered, so it acted; `0`: the call never reached it; `?`: forwarded, no answer from the server.
"The server acted, the client saw a failure" is `status` other than 0 with `server=1`; a history
or side-effect check can join these records with the client's own outcomes by `ref`.
`hydo-world check calls <proxy>[.<route>]` prints the four counts (answered OK; failed after the
server acted; failed before reaching it; failed, server's part unknown) as a HYDO-ORACLE OK line
(it always holds, so it can be an invariant that only reports). The console summary before the
verdict shows each `call` record.

**Explorer.** `explore.py --grpc-faults` draws up to two such tokens per schedule on the
manifest's gRPC proxies (`hvx/explore/grpc_faults.py`), from its own random stream: without the
flag every schedule string is byte-identical (`grpc_faults_ident.py`). The shrinker drops a
token, narrows its window, and removes `call=`, `rpc=`, `p=`/`seed=`, `dir=req`, turns
`reset=conn` into `reset` and halves `cut=`/`hold=`. Example: `examples/stack-grpc/`.

**Code.** gRPC proxies are served by `world/grpc.go` (`serveGRPCProxy`, `grpcHandler`), not by
`handler()`; it parses the gRPC-only rule keys (`parseRuleExt`, called from `parseRule`), picks the
rule with the extra filters (`grpcRule`), and implements `check calls`. gRPC fakes still use
`handler()`.

Limits: cleartext HTTP/2 only (no TLS); the client must be pointed at the proxy; a `ref:`/
`idempotency:` taken from the message (`pb:`) reads the call's first message before the decision
(a protobuf field holding its default value, such as an id of 0, is absent, so the ref is empty);
messages up to 64 MiB; compressed messages are passed through and counted, not decoded.

## YAML subset

The build machine has no YAML library, so `hydo-build` parses a subset: block mappings and
sequences, `- key: value` mapping items, `|` / `|-` literal and `>` / `>-` folded blocks,
flow `[a, b]` and `{k: v}`, single- and double-quoted strings, integers, `true`/`false`,
comments. Not supported: anchors and aliases, tags, multiple documents, multi-line plain
scalars. A `.json` file with the same structure is accepted as well.
Quote values YAML would otherwise turn into numbers or booleans when you mean a string
(`"1"`, `"no"`).

## Building and running

```sh
# on the build machine; tool paths default to build-stack.sh's pins
HYDO_SEEDRNG=~/hydo/build/hvx_seedrng hvx/onboard/hydo-build path/to/hydo.yaml
hvx/onboard/hydo-build path/to/hydo.yaml --set version=3.51.3   # a build parameter
hvx/onboard/hydo-build path/to/hydo.yaml --check         # validate only
hvx/onboard/hydo-build path/to/hydo.yaml --print-shell   # show the compiled manifest
# -> build/explore-ob-<name>-disk.img; copy it to ~/hydo/build on the bare-metal machine, then:
~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz \
    ~/hydo/build/explore-ob-<name>-disk.img <outdir> "disk=none dseed=1 | run C=12"
python3 hvx/explore/explore.py --image explore-ob-<name>-disk.img --seeds 1-50 --out <dir>
```

Guest RAM: `explore.py` reads `guest.ram_mib` from the manifest inside the image (or from
`--manifest`) and starts the engine with `--ram-mib N`; `explore.py --ram-mib N` overrides it.
The size is not part of a schedule, so a hand run of a non-default image needs the same flag:
`~/bin/hvx-run sudo ./hvx_explore --ram-mib 2048 one ...` (the report's repro line includes it).
Out-of-range values are rejected by `hydo-build`, `explore.py` and both engines.

Guest CPU: `guest.cpu: v3` makes `explore.py` start `hvx_explore --cpu v3` (hvx flag
`HVX_VM_X86_64_V3`): the guest sees AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE and XSAVE (x87|SSE|AVX
only, no AVX-512), which prebuilt x86-64-v3 binaries and images built for Haswell/Broadwell need.
`explore.py --cpu v3|v2` overrides it. Like the RAM size it is not in the schedule, so a hand run
needs `--cpu v3` too (the report's repro line includes it). Without the key nothing changes.

Inside the image: `/etc/hydo/hydo.yaml` (the manifest as written), `/etc/hydo/manifest.sh`
(compiled), `/etc/hydo/prelude.sh` (env + shell helpers), `/sbin/init` (`hydo-init.sh`).

## Limits of v1

- No OCI/Docker import yet (`image:` is reserved); files come from store paths or the host.
- The kernel command line is the explorer's. Guest RAM is per image (`guest.ram_mib`), at most
  3072 MiB on x86_64 (hvx keeps RAM below the 4th-GiB MMIO hole) and 4096 MiB on arm64.

- Images are files on the build machine (no registry pulls); one root per image service, not
  a merged root; no xattrs/capabilities, devices or FIFOs from layers; no zstd layers.
- Guest RAM is fixed at 1024 MiB and the kernel command line is the explorer's.
- The guest console is 64 KiB: a workload that prints every acknowledged id should print
  short ids, or the done marker can be lost.
- One data disk, ext4. The workload's step count is the unit of fault placement.

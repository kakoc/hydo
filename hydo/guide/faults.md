# Faults, knobs, and exploring

The full reference is in `hydo guide manifest`, in the sections `Fault tokens`, `Freeze at a
call`, `Clock steps to a time`, `Clock after a power cut`, `Link faults`, `One-shot jobs` and
`Groups of services and partitions`.

## Which faults a project allows

`faults:` in `hydo.yaml` lists the kinds the explorer may draw (default: all except `freeze`):

| kind | what happens | schedule token |
|---|---|---|
| `kill` | SIGKILL a service's whole process tree, restart it later (setup + cmd + ready) | `K=<svc>@<step>+<down>`, timed `X=` |
| `pause` | SIGSTOP / SIGCONT, also in the middle of a request | `P=<svc>@<step>+<n>`, `P=<svc>@<step>~<ms>:<ms>` |
| (none) | the workload's own processes stop for some guest time, as under a CPU limit (section "Pause the workload") | `P=workload@<step>~<ms>:<ms>` |
| (none) | named threads of the workload stop for some guest time, and its other threads run on (section "Pause threads of the workload") | `P=workload/<thread>@<step>~<ms>:<ms>` |
| `network` | netem delay, loss, duplication, and reordering on traffic to a service's `port:`, or on a link between two services with `addr:`. With `network: bridge` the link carries their real traffic, and the explorer draws only links between services that list each other in `depends_on`. | `N=<svc>@<from>-<to>:<netem>`, `N=a-b@...` |
| `oneway` | the same in one direction only | `O=<svc>@<from>-<to>:req\|resp:<netem>` |
| `cut` | power cut: only data that reached the disk survives; a fresh boot recovers and runs the final checks | `C=<step>`, host `cf=<k>` (at the k-th disk flush) |
| `clock` | step the realtime clock (like an NTP step), or set it to the next instant of a time of day or a calendar boundary (section "Clock steps to a time") | `T=<step>:<seconds>`, `T=<step>:@<point>` |
| `disk` | I/O errors, a lying disk that drops acknowledged writes | host `disk=ioerr:<p>`, `disk=lying:<p>` |
| `freeze` | stop a service at the n-th packet it sends to or receives from a peer (on a Mac: a release with the second guest kernel) | `F=<svc>><peer>@<step>#<n>:<ms>` |
| `protocol` | faults on fakes and proxies (`hydo guide outside-services`) | `H=...` |
| `corrupt` | bit flips, zeroed ranges or truncation of one file in a service's `storage:` directory, while the service runs or while it is down | `G=<svc>@<step>[+<down>]:flip\|zero\|trunc[,n=][,len=][,file=][,seed=]` |
| `powerloss` | a power loss of one service: what it had not written back to its own file system (`storage: {dir:, size_mib:}`) is lost, the others run on | `L=<svc>@<step>+<down>` |

`<step>` counts the workload's steps: a step is one numbered part of `workload.run`, or one round
of commands of the test composer (`workload.steps` sets how many). A fault at step n happens
before step n runs.

Leave out kinds that cannot apply (no `network` for a project whose services have no port).
`corrupt` and `powerloss` need `services[].storage` (`hydo guide manifest`, "Storage faults of
one service"): the directory under /data where the service keeps its files, with `size_mib` for
a file system of its own.
A schedule looks like `disk=none dseed=1 | run K=db@5+2 C=12 W=0000002a,p4,q2,r200`: host tokens
before the `|`, guest tokens after `run`.

## Host tokens

The host tokens set up the machine the guest runs on. Each is one `key=value` word before the
`|`; a schedule without one uses its default.

| token | what it does |
|---|---|
| `disk=none` | The default. The data disk has no write cache: a write reaches the disk when the guest writes it. A power cut (`C=`) then loses only what the guest still held in memory, never a write the disk took. |
| `disk=cut` | The data disk keeps a volatile write cache, as real disks do. A power cut (`C=` or `cf=`) loses every write the guest did not flush (`fsync`). Use it to test durability. |
| `disk=ioerr:<p>` | Seeded I/O errors on the data disk; `<p>` is the error rate in the engine's units (larger means more errors). |
| `disk=lying:<p>` | A disk that drops some writes it acknowledged; a power cut then loses them even when they were flushed. |
| `dseed=<hex>` | The seed of the disk's decisions: which requests fail or are dropped (default 0). The same seed gives the same decisions. |
| `cf=<k>` | A power cut at the k-th disk flush of the workload, instead of `C=` (hvx only; needs `disk=cut`). |
| `tj=<hex>` | Seeded tick jitter: some timer ticks come late, which changes thread interleavings. On hvx a delay counts guest branches; on the Mac and on KVM it counts VM exits. |
| `tjpm=<n>` | With `tj=`: how many ticks per 1,000 are delayed (default 10). |
| `tjmax=<n>` | With `tj=`: the longest delay (default 500,000 branches on hvx, 1,000 exits on the Mac and KVM). |
| `pct=<hex>`, `pctn=<n>` | The Mac and KVM: `<n>` forced preemption points at seeded VM exits. |
| `spin=rewind` | The Mac and KVM: run the schedule in rewind mode, which ends busy waits that make no VM exits deterministically. hydo switches to it by itself when a run needs it. |

The explorer and `hydo explore --sweep` write the other host tokens themselves (inserted ticks
`pp=` and `xt…` on hvx, vCPU turns `q=` with several vCPUs, the run budget `rb=`).

## Pause the workload

A container with a CPU limit stops every thread of the container when it has used its CPU time
for the period. A test that depends on thread timing can fail only then. The timed pause stops
the workload's own process tree, the test program of `workload.kind: test`, in the same way:

```sh
hydo run "disk=none | run P=workload@0~4:45"
```

Before step 0, hydo starts a timer in the guest. After 4 ms of guest time, hydo stops every
process of the workload with SIGSTOP. After 45 ms more, hydo continues them with SIGCONT. Guest
time is the guest kernel's clock, so the numbers mean the same on every engine. The pause ends on
its own: nothing in the workload has to run for it.

- `workload` may stand in a list with services: `P=workload,db@3~20:100` stops both.
- The pause by steps, `P=workload@<step>+<n>`, is refused: the workload does not run between
  steps.
- A schedule holds one `P=`, so a pause of the workload and a pause of a service do not go
  together.
- `faults:` does not need `pause`.

`hydo explore --pause-workload` draws such a pause for half of the schedules, from a stream of
its own:

- The step is one where the workload runs.
- The start is at most `--pause-max` ms after the step's start (default 1000). Half of the
  starts are drawn evenly, the other half on a log scale, so about a quarter fall in the first
  10 ms. Set `--pause-max` near the step's length in guest time.
- The length is 10, 20, 50, 100, 200, or 500 ms.

The shrinker tries the schedule without the pause, the pause at half its length (down to 10 ms),
and the pause at the step's start. `hydo guide flaky-tests` has a worked example.

## Pause threads of the workload

A race between two threads of one process needs one thread held off while the other runs. A
pause of the workload stops both. A thread pause stops only the threads it names:

```sh
hydo run "disk=none | run P=workload/tests::wal_reco@0~4433:500"
```

After 4,433 ms of guest time in step 0, hydo stops every thread of the workload whose name is
`tests::wal_reco`. After 500 ms more, it lets them run again. The other threads run on.

- The name is the thread's name as Linux keeps it in `/proc/<pid>/task/<tid>/comm`: at most 15
  bytes. A longer name is refused with its first 15 bytes, for example `tokio-runtime-w`.
- A name has letters, digits, and `_ . : + -`. `*` matches any characters, and `?` matches one
  character, such as a space.
- `#<k>` picks the k-th thread of that name, counted from 1 in the order the threads were
  created: `P=workload/tokio-rt-worker#2@0~100:200`. Without `#<k>`, every thread of that name
  stops.
- The threads are those of every process of the workload when the pause starts.
- When no thread matches, nothing stops. The control transcript then names the threads the
  workload had: `PAUSED nothing: no thread of the workload matches workload/nosuch (timer; its
  threads: busybox, collection.test, tests::wal_reco, tokio-rt-worker (4))`.

To find a thread's name, run that schedule with a name no thread has, at a start where the threads
exist. The guest kernel's cgroup v2 freezer stops the threads, in a cgroup of the workload's own
cgroup tree. No image needs a rebuild. The pause runs on hvx only: the guest kernels of the Mac
and KVM engines have no cgroups, so those engines refuse it before any VM boots.

`hydo explore --pause-thread <name>[,<name>...]` draws such pauses, as `--pause-workload` draws
pauses of the workload. With both options, the target is drawn among the workload and the names.
Two more options narrow the draw:

- `--pause-min <ms>`: the earliest start (default 0). The starts lie between it and `--pause-max`.
- `--pause-lengths <ms>,...`: the lengths to draw from (default 10, 20, 50, 100, 200, and 500).

## Faults on a cluster

Declare a cluster's nodes as a group, so that one token acts on several of them:

```yaml
groups:
  - name: nodes
    services: [n1, n2, n3]
    primary: /bin/print-leader     # optional: prints the current leader's service name
```

| token | what it does |
|---|---|
| `K=n1,n2,n3@5+2` | kills the three services before round 5, restarts them before round 7 |
| `K=nodes.all@5+2` | the same, for every member of the group |
| `K=nodes.majority@5+2` | kills floor(n/2)+1 members (`nodes.majority.1` picks the next subset) |
| `K=nodes.primary@5+1` | kills the member that `primary:` prints before round 5 |
| `P=nodes.minority@3+2` | pauses floor(n/2) members; `X=` takes the same targets |
| `N=nodes.halves@3-9:loss,100%` | cuts the links between two halves of the group (both ways) |
| `N=nodes.isolate.2@3-9:loss,100%` | cuts the third member off from the others |
| `N=nodes.primary@3-9:loss,100%` | cuts the leader off from the others |
| `N=nodes.ring@3-9:loss,100%` | each member hears only a majority, a different one for each member |
| `N=nodes.bridge@3-9:loss,100%` | two halves that reach each other only through one member |
| `N=n1/n2,n3@3-9:loss,100%` | cuts every link between the components `n1` and `n2,n3` |

A partition needs `addr:` on its services. The run's log names the services each selector chose
(`K: nodes.primary = n2`), and the schedule keeps the selector. `hydo explore --cluster-faults`
draws these tokens for a manifest with `groups:` and shrinks a selector towards one member.

## Knobs (durability settings the explorer may relax)

```yaml
knobs:
  - token: R                      # one capital letter, not a built-in token (K P N O C T Q X F H W B)
    name: Redis appendfsync
    default: always               # the safe value the image starts with
    values: [always, everysec, "no"]
    set: /bin/redis-cli CONFIG SET appendfsync $HYDO_VALUE    # changes it at run time
```

A setting that needs a restart cannot be a knob; say so in the report. A failure found with a
relaxed knob and a power cut is classified as an unsafe setting, not as a bug.

## Freeze at a packet

For bugs that need a service stopped between two messages of a conversation, list `freeze` in
`faults:`. Optionally add `freeze_match:`: payload strings that mark interesting packets, as
lists under `replies:` and `requests:`.

To find the packet, run `hydo explore --sweep freeze --fc-adaptive`. It sweeps the matched
streams (or every eighth packet), then every packet in a widening window around each failure. So
it maps a narrow failing band with a fraction of the runs of `--sweep freeze`, which tries every
packet.

## Clock steps to a time

A system that acts at a time of day or on a date needs the clock to reach that time while the
faults happen. Examples are a biller at minute 10 of every hour, a monthly invoice on the first
of the month, and a certificate that expires at midnight. `T=<step>:@<point>` sets the clock,
before the step, to the next instant of the point, in UTC. A point is `@00:10` (a time of day),
`@:10` (a minute of every hour), or `@hour`, `@day`, `@month` or `@year` (the start of the next
one). An offset can
follow, such as `@:10-2s` or `@month+10m`. The step always goes forward.

```sh
hydo run "disk=none | run T=0:@:10-2s K=redis@1+4"   # 2 s before the next minute 10, then Redis down
```

List the times in `hydo.yaml` so that the explorer draws them among its clock steps:

```yaml
faults: [kill, pause, network, cut, clock]
clock_points: ["@:10-2s", "@month+10m"]
```

Without `clock_points:` the explorer draws only steps by fixed amounts (30 s, 5 minutes, 1 hour,
1 day, 30 days and a year, forward or back).

## Clock after a power cut

Every boot starts its clock at `guest.clock_start`. Without `guest.clock_after_cut`, the recovery
boot after a power cut starts its clock at `clock_start` again, so time runs backwards across the
cut. No real machine does that. Set `guest.clock_after_cut: 60`, so the recovery boot continues
the first boot's clock plus 60 seconds, in two cases:

- A check compares a time written before a power cut with one written after it. Without the
  key, the check can report a false violation.
- The system stores times and checks them when it starts: databases with hybrid clocks
  (YugabyteDB, CockroachDB), leases, certificates. Without the key, a service refuses to start
  in the recovery boot ("clock skew"), and the run ends with `VIOLATION service cannot recover`.
  That is a false finding.

The key covers both kinds of power cut: `C=` and a cut at the k-th disk flush (`cf=`).

## BUGGIFY

`buggify: true` says the programs carry BUGGIFY fault points from an assertion SDK
(FoundationDB style: the code takes its own error path at chosen points). The explorer's
`--buggify` then draws the seed token `B=`. See `hydo guide sdks`.

## Exploring

```sh
hydo explore --seeds 1-50 --strategy process     # a Jepsen-style fault process: the best so far
hydo explore --seeds 1-50 --strategy swarm       # fault categories switched on and off per run
hydo explore --sweep kill                        # every kill point (also cut, net, pause, clock, ...)
hydo report                                      # each problem, with the command that shows it
hydo report --details                            # classes, signatures and minimal schedules
hydo repro <explore dir>                         # re-run every failure, compare fingerprints
hydo save <explore dir>                          # minimal schedules -> hydo/regressions/
```

Useful options of `hydo explore`:

- `--more-faults`: timed kills and k-th-flush cuts. Only the hvx engine has them; the explorer
  refuses the option with `--backend kvm` and on a Mac (`hydo guide capabilities`).
- `--new-faults`: pauses, clock jumps, and one-way loss.
- `--timing`: thread-timing jitter.
- `--concurrency`: concurrent clients, through a manifest knob `Q`.
- `--durable-only`: only durable settings and an honest disk.
- `--protocol-faults`, `--grpc-faults`, `--buggify`.
- `--storage-faults`: `G=` and `L=` for services with `storage:`.
- `--cluster-faults`: kills, pauses, and partitions of the services of `groups:`.
- `--pause-workload`: pauses of the workload's own processes (section "Pause the workload").
- `--pause-thread <name>`: pauses of named threads of the workload, with `--pause-min` and
  `--pause-lengths` (section "Pause threads of the workload").
- `--vcpus N --turns`: several vCPUs (hvx and KVM) and drawn turn lengths `q=`. `--vcpus N
  --sweep turns` searches the turn lengths of one schedule for a change of verdict (`hydo guide
  flaky-tests`).
- `--steer <id>` or `--steer-match REGEX`: an experimental steered search. It keeps the
  schedules whose runs came closest to failing, by a number the system prints, and changes them a
  little. On the one test it was measured on it found the failure later than the plain
  exploration. The section "Steer the search by a number the test prints" of
  `hydo guide flaky-tests` shows it.
- `--search state`: an experimental state search. A run takes up to 2 snapshots at stops whose
  console shows something rare. Later runs, the branches, start from such a snapshot. Each
  branch draws new faults for the remaining steps. On KVM and on the Mac it also draws new
  timing, with the host token `br=`. The search runs on one worker and writes `state-search.jsonl`. On the bug
  benchmark and on etcd 3.5.5 it found no failure sooner than the plain exploration.
  - `--state-mix N`: branches per 1,000 schedules (default 500). `0` runs the plain
    exploration's schedules with the same bookkeeping.
  - `--state-corpus N`: the most snapshots kept (default 4 on hvx, 32 otherwise).
  - `--state-rare N`: a console feature is rare when at most N runs showed it (default 2).
  - `--state-snaps N`: the most snapshots of one run (default 2).
  - `--state-stop`: stop after the round of the first failure.

  Use `--no-shrink` with it: the shrinker does not know `br=`. The design and the numbers are in
  `docs/design/state-search.md` in hydo's repository.
- `--shrink-causes N`: shrink up to N failures of one signature, each with other fault kinds
  than the ones already chosen (default 1: only the first failure). One last line can have
  several causes; with `--shrink-causes 3` a "fee missing" from a power cut and one from a clock
  step back each get their own minimal schedule. `report.md` names each shrink's fault kinds.
- `--workers N`.

hydo's own benchmark showed that where faults land, and in what combination, matters more than
how the next schedule is chosen. So prefer `--strategy process` with `--more-faults`. With
`--backend kvm` and on a Mac, use `--new-faults` instead.

One schedule holds one `K=`, one `P=`, one `X=`, and one `C=`; a second one is refused. Several
`N=` tokens are one fault when they share the window and the netem arguments. Every token names
something the image has: a service, a group, a link between services with `addr:`, a fake or
proxy, or a knob of `knobs:`. A token for anything else is refused before the run, with a message
that names what the image has.

To crash or stop several services at once, name them in one token (`K=n1,n2@5+2`) or select them
from a group (section "Faults on a cluster"). To act at the moment the system reports an event,
use reactions: write `A=<event>:kill-<svc>`. `<event>` is the id of an assertion record that the
system writes to the console (`hydo guide manifest`, section `Reactions`).
`hydo explore --reactive fault --react-events <id>` draws them.

# Faults, knobs, and exploring

The full reference is in `hydo guide manifest`, in the sections `Fault tokens`, `Freeze at a
call`, `Clock after a power cut`, `Link faults`, and `Groups of services and partitions`.

## Which faults a project allows

`faults:` in `hydo.yaml` lists the kinds the explorer may draw (default: all except `freeze`):

| kind | what happens | schedule token |
|---|---|---|
| `kill` | SIGKILL a service's whole process tree, restart it later (setup + cmd + ready) | `K=<svc>@<step>+<down>`, timed `X=` |
| `pause` | SIGSTOP / SIGCONT, also in the middle of a request | `P=<svc>@<step>+<n>`, `P=<svc>@<step>~<ms>:<ms>` |
| `network` | netem delay, loss, duplication, and reordering on traffic to a service's `port:`, or on a link between two services with `addr:`. With `network: bridge` the link carries their real traffic, and the explorer draws only links between services that list each other in `depends_on`. | `N=<svc>@<from>-<to>:<netem>`, `N=a-b@...` |
| `oneway` | the same in one direction only | `O=<svc>@<from>-<to>:req\|resp:<netem>` |
| `cut` | power cut: only data that reached the disk survives; a fresh boot recovers and runs the final checks | `C=<step>`, host `cf=<k>` (at the k-th disk flush) |
| `clock` | step the realtime clock (like an NTP step) | `T=<step>:<seconds>` |
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

## Clock after a power cut

Every boot starts its clock at `guest.clock_start`. A check can compare a time written before a
power cut with one written after it. Then set `guest.clock_after_cut: 60`, so the recovery boot
continues the first boot's clock plus 60 seconds. Otherwise time runs backwards, and the check
can report a false violation.

## BUGGIFY

`buggify: true` says the programs carry BUGGIFY fault points from an assertion SDK
(FoundationDB style: the code takes its own error path at chosen points). The explorer's
`--buggify` then draws the seed token `B=`. See `hydo guide sdks`.

## Exploring

```sh
hydo explore --seeds 1-50 --strategy process     # a Jepsen-style fault process: the best so far
hydo explore --seeds 1-50 --strategy swarm       # fault categories switched on and off per run
hydo explore --sweep kill                        # every kill point (also cut, net, pause, clock, ...)
hydo report                                      # findings with minimal schedules
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

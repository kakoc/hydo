# Faults, knobs and exploring

Full reference: "Fault tokens", "Freeze at a call", "Clock after a power cut", "Link faults"
and "Groups of services and partitions" in `hydo guide manifest`.

## Which faults a project allows

`faults:` in `hydo.yaml` lists the kinds the explorer may draw (default: all except `freeze`):

| kind | what happens | schedule token |
|---|---|---|
| `kill` | SIGKILL a service's whole process tree, restart it later (setup + cmd + ready) | `K=<svc>@<round>+<down>`, timed `X=` |
| `pause` | SIGSTOP / SIGCONT, also in the middle of a request | `P=<svc>@<round>+<n>`, `P=<svc>@<round>~<ms>:<ms>` |
| `network` | netem delay, loss, duplication, reordering on traffic to a service's `port:` (or on a link between two services with `addr:`; with `network: bridge` the link carries their real traffic and the explorer draws only links between services that list each other in `depends_on`) | `N=<svc>@<from>-<to>:<netem>`, `N=a-b@...` |
| `oneway` | the same in one direction only | `O=<svc>@<from>-<to>:req\|resp:<netem>` |
| `cut` | power cut: only data that reached the disk survives; a fresh boot recovers and runs the final checks | `C=<round>`, host `cf=<k>` (at the k-th disk flush) |
| `clock` | step the realtime clock (like an NTP step) | `T=<round>:<seconds>` |
| `disk` | I/O errors, a lying disk that drops acknowledged writes | host `disk=ioerr:<p>`, `disk=lying:<p>` |
| `freeze` | stop a service at the n-th packet it sends to or receives from a peer (on a Mac: a release with the second guest kernel) | `F=<svc>><peer>@<round>#<n>:<ms>` |
| `protocol` | faults on fakes and proxies (`hydo guide outside-services`) | `H=...` |
| `corrupt` | bit flips, zeroed ranges or truncation of one file in a service's `storage:` directory, while the service runs or while it is down | `G=<svc>@<round>[+<down>]:flip\|zero\|trunc[,n=][,len=][,file=][,seed=]` |
| `powerloss` | a power loss of one service: what it had not written back to its own file system (`storage: {dir:, size_mib:}`) is lost, the others run on | `L=<svc>@<round>+<down>` |

Leave out kinds that cannot apply (no `network` for a project whose services have no port).
`corrupt` and `powerloss` need `services[].storage` (`hydo guide manifest`, "Storage faults of
one service"): the directory under /data where the service keeps its files, with `size_mib` for
a file system of its own.
A schedule looks like `disk=none dseed=1 | run K=db@5+2 C=12 W=0000002a,p4,q2,r200`: host tokens
before the `|`, guest tokens after `run`.

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
relaxed knob and a power cut is classified as "unsafe setting", not as a bug.

## Freeze at a packet

For bugs that need a service stopped between two messages of a conversation, list `freeze` in
`faults:` and optionally `freeze_match:` (payload strings that mark interesting packets, as lists under `replies:` and `requests:`). Finding the packet:
`hydo explore --sweep freeze --fc-adaptive` sweeps the matched streams (or every 8th packet),
then every packet in a widening window around each failure, so a narrow failing band is mapped
with a fraction of the runs of `--sweep freeze` (every packet).

## Clock after a power cut

Every boot starts its clock at `guest.clock_start`. If a check compares a time written before a
power cut with one written after it, set `guest.clock_after_cut: 60` so the recovery boot
continues the first boot's clock plus 60 seconds; otherwise time runs backwards and the check can
report a false violation.

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

Useful options (`hydo explore` passes them to the explorer): `--more-faults` (timed kills and
k-th-flush cuts; the hvx engine only: the explorer refuses it with `--backend kvm` and on a Mac,
`hydo guide capabilities`), `--new-faults` (pauses, clock jumps, one-way loss), `--timing`
(thread-timing jitter), `--concurrency` (a manifest knob `Q`), `--durable-only` (only durable settings and an honest disk),
`--protocol-faults`, `--grpc-faults`, `--buggify`, `--storage-faults` (`G=` and `L=` for
services with `storage:`), `--cluster-faults` (kills, pauses and partitions of `groups:`),
`--workers N`. The lesson of hydo's own
benchmark: where and in what combination faults land matters more than how the next schedule is
chosen, so prefer `--strategy process` with `--more-faults` (with `--backend kvm` and on a Mac,
`--new-faults`).

One schedule holds one `K=`, one `P=`, one `X=` and one `C=` (a second one is refused). Several
`N=` tokens are one fault when they share the window and the netem arguments. Every token names
what the image has: a service, a group, a link between services with `addr:`, a fake or proxy,
or a knob of `knobs:`. A token for anything else is refused before the run, naming what the
image has. To crash or stop several services at once, name them in one token (`K=n1,n2@5+2`) or
select them from a group (see "Faults on a cluster"). To act at the moment the system reports an
event, use reactions: `A=<event>:kill-<svc>`, where `<event>` is the id of an assertion record
the system writes to the console (`hydo guide manifest`, "Reactions").

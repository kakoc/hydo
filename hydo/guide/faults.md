# Faults, knobs, and exploring

The full reference is in `hydo guide manifest`, in the sections `Fault tokens`, `Freeze at a
call`, `Clock after a power cut`, and `Link faults`.

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
- `--workers N`.

hydo's own benchmark showed that where faults land, and in what combination, matters more than
how the next schedule is chosen. So prefer `--strategy process` with `--more-faults`. With
`--backend kvm` and on a Mac, use `--new-faults` instead.

One schedule holds one `K=`, one `P=`, and one `X=`; a second one is refused. Every token names
something the image has: a service, a link between services with `addr:`, a fake or proxy, or a
knob of `knobs:`. A token for anything else is refused before the run, with a message that names
what the image has.

To crash or stop several services at once, for example every node of a cluster, use reactions.
Write `A=<event>:kill-<svc>` once per service. `<event>` is the id of an assertion record that the
system writes to the console (`hydo guide manifest`, section `Reactions`).
`hydo explore --reactive fault --react-events <id>` draws them.

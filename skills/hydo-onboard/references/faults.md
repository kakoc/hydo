# Faults, knobs and exploring

Full reference: "Fault tokens", "Freeze at a call", "Clock after a power cut" and "Link faults"
in `hydo guide manifest`.

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
| `freeze` | stop a service at the n-th packet it sends to or receives from a peer (x86_64 only) | `F=<svc>><peer>@<round>#<n>:<ms>` |
| `protocol` | faults on fakes and proxies (outside-services.md) | `H=...` |

Leave out kinds that cannot apply (no `network` for a project whose services have no port).
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
`--buggify` then draws the seed token `B=`. See sdks.md.

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
k-th-flush cuts), `--new-faults` (pauses, clock jumps, one-way loss), `--timing` (thread-timing
jitter), `--concurrency`, `--durable-only` (only durable settings and an honest disk),
`--protocol-faults`, `--grpc-faults`, `--buggify`, `--workers N`. The lesson of hydo's own
benchmark: where and in what combination faults land matters more than how the next schedule is
chosen, so prefer `--strategy process` with `--more-faults`.

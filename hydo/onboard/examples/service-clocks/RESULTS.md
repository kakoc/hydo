# One service's own clock: results

This example shows `clock: own` and `T=<svc>@<at>:<change>` (Milestone 372,
`docs/design/service-clocks.md`). Two nodes, `n1` and `n2`, share a lease in the file `/tmp/lease`.
Each node reads its own wall clock with `clockprobe`, a static Go program, and counts itself the
leader until the lease it wrote runs out by that clock (`lease.sh`). The check `lease.one_leader`
fails when the lead changes more than once, which happens only when both nodes lead at once.

## The runs

On hvx on the bare-metal machine, with this tree's `hydo` and `hydo-hvx` over the 0.6.0 Linux
release's guest files. The image is `6d3430c3…`, built at the fingerprint epoch 6. Every
schedule ran twice and gave one fingerprint.

| Schedule | Verdict | Fingerprint |
|---|---|---|
| `disk=none \| run` | OK, the lead changed 0 times | `57d19bbc77f8360b` |
| `disk=none \| run T=3:5` | OK, 0 times: both clocks jump together | `1ff65322865695d5` |
| `disk=none \| run T=n2@3:5` | VIOLATION `[lease.one_leader]`, 23 changes from uptime 7.78 s | `9feba06d7a2b47c3` |
| `disk=none \| run T=n2@3:-5` | OK, 0 times: a clock behind never sees the lease run out | `fad0820e66206c18` |
| `disk=none \| run T=n2@0:500000ppm` | VIOLATION `[lease.one_leader]`, 23 changes from uptime 9.54 s | `bd68cd0ff4c2b957` |
| `disk=none \| run T=n1@3:1500ms T=n2@3:-1500ms` | OK, 0 times | `1df85a95a9dee374` |
| `disk=none \| run T=n1@0:4 T=n2@0:4` | OK, 1 change: both clocks jump together at step 0 | `beeb8a7a0e432b60` |
| `disk=none \| run P=n1@2+3` | OK, 1 change: `n2` took the lease while `n1` was paused | `61ccc82fa554b277` |
| `disk=none \| run K=n1@2+2 T=n1@1:2` | OK, 0 times: `n1` kept its offset across the restart | `07c13db36f21324b` |

The check `clocks.offset` printed what the two nodes read at the end of each run:

- `T=n2@3:5`: `n2 reads 5006 ms ahead of n1`;
- `T=n2@3:-5`: `-5005 ms`;
- `T=n2@0:500000ppm`: `4411 ms`, after about 8.8 s of drift at one half;
- `T=n1@3:1500ms T=n2@3:-1500ms`: `-3005 ms`.

The few milliseconds beyond the offset come from `/proc/uptime`, which counts in steps of 10 ms,
and from the time between a node's two readings.

## The cost of a read

The workload's step 0 reads the wall clock 20,000 times with `clockprobe bench 20000`, once as
the guest's own program and once under `hydo-clock`:

```text
hydo-clocks: guest reads=20000 ns_per_read=4406 odd=0
hydo-clocks: own reads=20000 ns_per_read=40482 odd=0
```

A read takes 4.4 µs of guest time through the vDSO, and 40.5 µs under hydo-clock: a ptrace stop
per read. On the bare-metal machine itself, outside a VM, the same program took 0.12 µs and
21.9 µs.

## Build and run

```sh
hydo/crates/hydo-clock/build.sh build             # -> build/hydo-clock
hydo/onboard/examples/service-clocks/build.sh      # -> clockprobe.bin (needs Go)
HYDO_CLOCKBIN=$PWD/build/hydo-clock hydo build hydo/onboard/examples/service-clocks/hydo.yaml
hydo run "disk=none | run T=n2@3:5"
```

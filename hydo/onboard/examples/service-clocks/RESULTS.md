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
hydo/crates/hydo-clock/build.sh build             # -> build/hydo-clock (and build/hydo-clock-words)
hydo/onboard/examples/service-clocks/build.sh      # -> clockprobe.bin (needs Go)
HYDO_CLOCKBIN=$PWD/build/hydo-clock hydo build hydo/onboard/examples/service-clocks/hydo.yaml
hydo run "disk=none | run T=n2@3:5"
```

## On the KVM engine

Milestone 379 (tentative number). The KVM engine runs `clock: own` only for an image of
`guest.kernel: containers`, because only those guest kernels have the seccomp filters that
hydo-clock installs. `hydo-kvm.yaml` is `hydo.yaml` with that key. `hydo build` stages the build
of hydo-clock that reaches the traced program's memory with `PTRACE_PEEKDATA`, because no KVM
kernel has `process_vm_readv`. `hydo.yaml`'s image stops on KVM before any VM, with a message
that names the key.

The runs used GitHub's runners, with this tree's programs over the 0.7.0 Linux release's guest
files. The image is `e4701af2…` (a `clockprobe.bin` of Go 1.26.5). Every schedule ran twice per
host and gave one fingerprint per CPU vendor, and every verdict is hvx's:

| Schedule | Verdict | Intel Xeon 6975P-C (four hosts) | AMD EPYC 7763 (two hosts) |
|---|---|---|---|
| `disk=none \| run` | OK, the lead changed 0 times | `082079041061e37c` | `2c43f47c27113fe1` |
| `disk=none \| run T=3:5` | OK | `5e133c16434ed2dc` | `3621cb1014f82f13` |
| `disk=none \| run T=n2@3:5` | VIOLATION `[lease.one_leader]`, 23 changes from uptime 11.91 s | `b9e4d42c9444690a` | `1ec986860f1f37e9` |
| `disk=none \| run T=n2@3:-5` | OK | `27b7d766ff399b00` | `0a66fd05cff789fc` |
| `disk=none \| run T=n2@0:500000ppm` | VIOLATION `[lease.one_leader]`, 23 changes from uptime 13.66 s | `28f253191301593f` | `eed3a446252de5c2` |
| `disk=none \| run T=n1@3:1500ms T=n2@3:-1500ms` | OK | `8b1de1e33ffc8539` | `abe9ea76863f3374` |
| `disk=none \| run T=n1@0:4 T=n2@0:4` | OK, 1 change | `5835d5942a9c69d7` | `226493112c25e058` |
| `disk=none \| run P=n1@2+3` | OK, 1 change | `2f5f2de2b4e2b1c8` | `faf5aa38c187cd7a` |
| `disk=none \| run K=n1@2+2 T=n1@1:2` | OK | `178fd94e317e22cf` | `343894aadee08a36` |

A read of the wall clock took 5.2 µs of guest time as the guest's own program and 21.2 µs under
hydo-clock.

```sh
hydo/crates/hydo-clock/build.sh build             # -> build/hydo-clock and build/hydo-clock-words
HYDO_CLOCKBIN_WORDS=$PWD/build/hydo-clock-words hydo build hydo/onboard/examples/service-clocks/hydo-kvm.yaml
hydo run --backend kvm "disk=none | run T=n2@3:5"
```

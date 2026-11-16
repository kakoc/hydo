# Leader election on a Lease, in k3s in the guest

This example runs Kubernetes inside hydo's VM. k3s, a Kubernetes distribution in one binary,
runs as one hydo service on hvx. It starts a StatefulSet of 3 pods, and the pods elect a leader
on a Lease. A Lease is a Kubernetes API object that names one holder. The example has three
rules:

- At most one pod acts as leader at any instant.
- A leader exists again after the faults.
- The StatefulSet gets its 3 ready pods back.

The design and the kernel question are in `docs/design/k3s-in-guest.md`.

## The short answers

- **Does k3s run in the guest?** Yes, on hvx, with no change to hydo, or to the guest kernel.
  The node is Ready 15.3 s after the guest boots, about 8 s after k3s starts. The 3 pods are
  ready and a leader holds the Lease at 37.6 s. Everything happens in provisioning, before the
  snapshot point S1, so each schedule starts from a running cluster.
- **How much memory?** A guest of 768 MiB is enough. With the 3 pods ready, 236 MB were
  available of 768 MiB, 504 MB of 1,024 MiB, and 1,474 MB of 2,048 MiB. `hydo run --memory`
  gives the k3s process a peak of 485 MiB.
- **Is it deterministic?** Yes. Every schedule that ran twice from a fresh boot gave one
  fingerprint. There were seven such pairs, on five builds of the image: without faults, with a
  crashing leader, and with clock steps.
- **Does the rule hold?** A leader that crashes in a loop hands the Lease over, the rule holds,
  and the StatefulSet heals. A kill and a pause of k3s keep one leader. A forward clock step of
  30 s before step 2 gives two leaders for 0.67 s of guest time, in every run of that schedule.
  This is the documented limit of leader election on Leases: the candidates judge the Lease by
  the wall clock, and the holder judges its own term by the monotonic clock.
- **What did the explorer find?** A step of the wall clock back by more than an hour leaves no
  leader. It puts the clock before the start of k3s's certificates, and every client call fails.
  The cluster is one minute old in every run, so every such step meets it.

## How to build and run it

On a Linux x86_64 machine with the hydo release's `bin/` on `PATH`:

```sh
sh fetch-k3s.sh                       # k3s v1.37.2+k3s1, two images, 57 kernel modules
hydo build hydo.yaml
hydo run "disk=none | run"
```

`fetch-k3s.sh` checks every file by sha256. It runs the k3s binary once, to unpack k3s's own
programs into `k3s/data/`. The kernel modules come from the guest kernel's build output, the
nixpkgs store path `rp8637p7…-linux-6.6.35`, which `nix-store -r` fetches from cache.nixos.org.
Set `KMODS` to another copy of its `lib/modules/6.6.35` folder.

The image is 512 MiB of root and a 2,048 MiB data disk. It needs hvx: the Mac and KVM guest
kernels lack the kernel options k3s needs.

## What runs in the guest

- `k3s-setup.sh` runs before every start of k3s. It loads the modules, mounts cgroup v2, gives
  k3s writable directories on the data disk, and adds a dummy network interface with a default
  route.
- k3s starts with its embedded SQLite datastore, flannel's host-gw backend and kube-proxy.
  Traefik, the service load balancer, the metrics server, local storage, CoreDNS, the network
  policy controller and the Helm controller are off.
- `manifests/elect.yaml` is applied by k3s at its start: a ServiceAccount with a Role for Leases,
  a headless Service, and the StatefulSet `elect` with 3 pods of busybox.
- Each pod runs `elect.sh`. It calls kubectl, which is mounted from the node's k3s files.
- `workload.sh` lets the cluster run 2 s per step for 10 steps. With the knob `Z=<step>`, it
  kills the process of the pod that holds the Lease before that step.
- `one-leader.sh` reads the pods' logs on the data disk and checks that no two pods' spans as
  leader share an instant.

## The leader election

`elect.sh` does what client-go's `leaderelection` package does, with a lease duration of 10 s,
a renew deadline of 5 s and a retry period of 2 s:

- A pod takes the Lease when it has no holder, or when `renewTime` plus 10 s has passed on the
  pod's wall clock. The write carries the Lease's `resourceVersion`, so of two pods only one wins.
- The holder writes its wall clock into `renewTime` every 2 s.
- The holder acts as leader until 5 s have passed on its monotonic clock since the start of its
  last successful renewal, or until it sees another holder.
- While it leads, a pod writes `L <uptime in centiseconds>` to its log every half second.

## The runs

Every run used hvx on the bare-metal machine, with one vCPU, `hydo run` of the 0.3.3 Linux
release, and the image built from this folder (`explore-ob-k3s-lease-disk.img`, sha256
`73f447de6647f0a1…`). Each run took 76 s to 90 s of wall time while other sessions loaded the
machine. The times are the guest's uptime in seconds.

| schedule | runs | fingerprint | verdict | what happened |
|---|---|---|---|---|
| `disk=none \| run` | 2 | `a23e6b290c893367` | OK | `elect-2` creates the Lease at 33.39 and renews it to the end |
| `disk=none \| run Z=4` | 1 | `b2b47087593e0619` | OK | `elect-2` is killed at 42.99. The kubelet starts it again, and it resumes its own Lease at 44.05. It is killed again at 44.33 and waits in the kubelet's back-off. `elect-1` takes the Lease at 55.53, after the 10 s lease duration. |
| `disk=none \| run T=2:30` | 2 | `06c012f535a1a661` | VIOLATION | At 40.28 the wall clock steps 30 s forward. Then `elect-1` finds the Lease expired. It takes the Lease at 40.82. `elect-2` acts as leader until 41.49 and sees the new holder at 42.06. |
| `disk=none \| run K=k3s@4+2` | 1 | `2b1b1dccbf4f8cce` | OK | k3s, with the API server, is killed before step 4. It is started again before step 6. `elect-2` keeps the Lease. |
| `disk=none \| run P=k3s@4+5` | 1 | `53745f636138dba9` | OK | k3s is stopped for 5 steps. `elect-2` cannot renew, and it steps down at 49.51 when its renew deadline passes. It takes the Lease again at 50.18. |

The violation's message is `1 overlapping pairs of leader spans; elect-1 led from 40.82 s to
58.39 s and elect-2 from 33.39 s to 41.49 s (guest uptime)`.

A forward step does not always win. The holder renews every 2 s, and a step breaks the rule only
when a candidate reads the Lease before the holder renews it. Of the steps `T=<n>:30` before
steps 1 to 6, only the one before step 2 broke the rule. Each step lasts 1.3 s, so the steps meet
the 2 s renewal at different phases. With steps of 2 s, an earlier version of the workload met
it at one phase only, and no step of 30 s broke the rule.

Earlier builds of the image gave the same result for every repeated schedule:

| image | schedule | fingerprint of both runs |
|---|---|---|
| a first probe, without the workload | `disk=none \| run` | `fabb5d08aa7856f8` |
| 10 steps of 2 s | `disk=none \| run` | `3db57cd50966fd39` |
| 16 steps of 1.3 s, before the checker's message changed | `disk=none \| run` | `e53cc6661c845f08` |
| the same | `disk=none \| run Z=4` | `061b6585631ea0d8` |

Copies of the image with less memory gave `da655db929c6930d` with `guest.ram_mib: 1024` and
`16c779f15472172c` with 768, both OK.

## The explorations

`hydo explore` runs each schedule from a snapshot of the booted guest at S1, so a run costs only
its 16 steps. All three explorations used one worker and `--ram-mib 1024`. A first try with two
workers and 2,048 MiB guests was ended by the host's out-of-memory killer, on a machine that other
sessions shared.

| exploration | schedules | failures | mean run | what it drew |
|---|---|---|---|---|
| `--seeds 1-16` (random) | 16 | 0 | 23.0 s | kills of k3s and the knob `Z` |
| `--seeds 1-24 --strategy swarm` | 24 | 0 | 26.0 s | kills of k3s and the knob `Z` |
| `--seeds 1-24 --new-faults` | 8, then stopped | 1 | | also pauses of k3s and clock steps |

The worker's boot to S1 took about 295 s of wall time on the loaded machine.

The failure was `disk=none dseed=6e73e372e2338aca | run K=k3s@3+2 T=8:-86400`: a kill of k3s,
then a step of the wall clock one day back. The rule `elect.leader_again` broke, because no pod
led again within 60 s. The exploration was stopped before it shrank the schedule, because each
run took minutes on the loaded machine. By hand, on the same 1,024 MiB image:

| schedule | fingerprint | verdict |
|---|---|---|
| `disk=none \| run T=8:-86400` | `0dc6e6d7e72a35ab` | VIOLATION `elect.leader_again` |
| `disk=none \| run K=k3s@3+2 T=8:-86400` (twice) | `81bc334dc1a6e343` | VIOLATION `elect.leader_again` |
| `disk=none \| run T=8:-7200` | `795494b0d2c21d40` | VIOLATION `elect.leader_again` |
| `disk=none \| run T=7:-3600` (in the exploration) | | OK |

The kill is not needed. The cause is in the workload's last lines: every client call fails with
`x509: certificate has expired or is not yet valid: current time 2025-12-31T00:00:50Z is before
2025-12-31T23:00:02Z`. k3s made its certificates at 00:00:02 on the guest's first day, valid from
one hour before. A backward step of more than that hour puts the clock before the certificates,
and no client can reach the API server. The leader cannot renew, steps down after its renew
deadline, and no pod can take the Lease.

This is k3s's behaviour on any node whose clock goes back past its certificates' start, for
example a node whose hardware clock was reset. Here every cluster is one minute old, because
the image starts it at `guest.clock_start`, so every backward step of more than an hour meets
it. On a cluster made long before the run, only a step back past the cluster's age would.

## The Mac and KVM engines (Milestone 256)

The folder `containers/` holds the same example for the guest kernel variant for containers
(`guest.kernel: containers`, `docs/design/k3s-in-guest.md`). Its `hydo.yaml` uses this folder's
scripts, carries no kernel modules, and takes k3s's files for one architecture from its own
`fetch-k3s.sh`: arm64 for the Mac engine, amd64 for KVM. The arm64 k3s binary unpacks its files
to the hash `10bae91d040ed733…`. `containers/probe.yaml` checks the kernel without k3s.

On the Mac engine, with the 0.3.3 release's guest files and this tree's programs:

| image | schedule | runs | fingerprint | verdict |
|---|---|---|---|---|
| `probe.yaml` (`610fea05…`) | `disk=none \| run` | 2 | `bc9b021a4c1cc0c0` | OK: 16 of 16 checks |
| `probe.yaml` without `guest.kernel` | `disk=none \| run` | 1 | `51d0dceb26e3d9b0` | VIOLATION: 13 of 16 checks fail |
| `containers/hydo.yaml` (`50c600fa…`) | `disk=none \| run` | 2 | none | `ERROR boot: SPIN` at 31 s of guest time |

In the probe, runc started busybox as PID 1 of its own PID namespace, with the host name `runc`,
a memory limit of 64 MiB and a limit of 64 processes. Without the key the kernel has only the cpu
controller, no overlayfs, no NAT table, and no nftables.

k3s did not reach the node's readiness on the Mac engine. Its setup ran, and k3s started. Then a
thread of k3s looped in `runtime.selectgo`, called from the watcher of kine, k3s's SQLite store,
and made no VM exits. The Mac engine's clock counts VM exits, so no timer interrupt preempted the
thread. With `HVM_SPIN_SECS=120` the loop still ran after 10 minutes. With k3s's embedded etcd
(`--cluster-init`) the boot reached 119.5 s of guest time and stopped in another such loop. So
the two-leaders finding of `T=2:30` could not be repeated on the Mac engine. This is open item
13 of AGENTS.md, a limit of the engine, not of the kernel.

On KVM, GitHub's runners ran the one-off workflow `tmp-k3s-kvm` (run 43698484545) with the amd64
k3s, whose files unpack to the hash `342d21a06834327c…` as for hvx:

| host | vCPUs | image | runs | fingerprint | verdict |
|---|---|---|---|---|---|
| Xeon 6975P-C, Xeon Platinum 8370C | 1 | `probe.yaml` | 2 each | `490a7a6c2ad8c861` | OK: 16 of 16 checks |
| Xeon 6975P-C | 2 | `probe.yaml` | 2 | `d45ed401275c075c` | OK: 16 of 16 checks |
| AMD EPYC 9V74 | 2 | `probe.yaml` | 2 | `6035b2f3d8f130c1` | OK: 16 of 16 checks |
| Xeon 6975P-C, Xeon Platinum 8370C | 1 | `containers/hydo.yaml` | 9 each | `473f8c50652edfd6` | `ERROR service start (k3s)` |
| Xeon 6975P-C, AMD EPYC 9V74 | 2 | `containers/hydo.yaml` | 1 each | none | `ERROR boot: SPIN` at VM exit 143,533 |

With 1 vCPU every schedule ended the same way, also `T=1:30` to `T=6:30` and `Z=4`: k3s exits
with `failed to find cpuset cgroup (v2)`. The one-vCPU KVM kernel has no cpuset controller,
because that controller needs SMP. With 2 vCPUs the boot stopped in the loop of the Mac engine:
the kicks hit the channel locks of `select` (`runtime.lock2`, `runtime.selunlock`).
`docs/design/k3s-in-guest.md` explains the loop. So the two-leaders finding was not repeated on
KVM either.

## KVM with the branch tick (Milestone 274)

`containers/hydo.yaml` now has `guest.branch_tick: true` (`docs/design/exit-free-loops.md`). On
the KVM engine of an Intel host with a counter of the guest's branches, the engine interrupts
kine's loop after 2^26 of the guest's user-mode conditional branches, at that exact count. The
setup also runs `containers/cpuinfo.sh`. The KVM guest's `/proc/cpuinfo` has no `cpu MHz` line,
and without it the kubelet stopped with "could not detect clock speed". The script binds a copy
with the line `cpu MHz : 2000.000` over the file when the line is missing.

The one-off workflow `tmp-exitfree-kvm` ran it on GitHub's runners with the 0.3.4 Linux release,
2 vCPUs (k3s needs the cpuset controller of the SMP kernel), and the image `5897029e…` (run
43917522895, one Xeon 6975P-C host):

| schedule | runs | fingerprint | verdict |
|---|---|---|---|
| `disk=none \| run` | 3, one with a margin of 1024 branches | `8c43fbd8700b2862` | OK: one leader span |
| `disk=none \| run T=2:30` | 2 | `4736cf7ec6c1d53e` | OK: two leader spans, none overlap |
| `disk=none \| run T=3:30` | 1 | `7f36d45d138bf971` | OK: two leader spans, none overlap |

The node was Ready at 3.1 s of guest time, the 3 pods at 9.2 s, and a pod led on the Lease at
9.2 s. A boot had 32 to 63 ticks. The forward steps gave a second leader span, but not the two
leaders at once that hvx shows. On the Mac engine the image is refused: an M2 has no counter of
the guest's branches.

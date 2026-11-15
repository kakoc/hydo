# Kubernetes in the guest: k3s on hvx

Status: an experiment, Milestone 246 (tentative number), 2026-11-14. k3s runs in the guest on
hvx with no change to hydo, to the hvx module, or to the guest kernel. The example is
`hydo/onboard/examples/k3s-lease`, and its measurements are in its `RESULTS.md`.

Milestone 256 (tentative number), 2026-11-15, built the kernel variant of the section "The Mac
and KVM engines" below: `guest.kernel: containers`. With it, containers run on the Mac engine.
k3s itself does not start there: a loop of k3s's makes no VM exits, and the Mac engine cannot
interrupt such a loop (section "The variant, as built").

## The question

Some software is about Kubernetes itself. An operator or a controller reads and writes the
Kubernetes API. A program can depend on a pod being started again on failure. It can also depend
on a leader elected through a Lease, a small API object that names one holder at a time. `hydo k8s`
(Milestone 243, `docs/design/k8s-import.md`) turns manifests into hydo services, and no
Kubernetes runs. So such software cannot be tested with it.

k3s is a Kubernetes distribution in one static binary. It holds the API server, the scheduler,
the controllers, the kubelet (the node agent that starts the pods), and containerd (the container
runtime). It keeps its state in SQLite by default. This page answers four questions:

1. What does k3s need from the guest kernel, and is a kernel change an epoch change?
2. Does k3s start in the guest, and how much guest time and memory does it take?
3. Do two runs of one schedule give one fingerprint?
4. Can hydo check a rule of a program on Kubernetes under faults?

## What k3s needs from the kernel

The table lists the kernel options that k3s and its parts use: `k3s check-config`, the
kubelet, containerd with runc, flannel (the pod network) and kube-proxy (the Service addresses).
It compares them with the three guest kernels of hydo. `y` is built in, `m` is a module, and `-`
is absent.

| needed for | options | hvx `bzImage` | KVM `bzImage-kvm-cg` | Mac `Image-explore-netns-cg` |
|---|---|---|---|---|
| containers | `PID_NS`, `IPC_NS`, `UTS_NS`, `USER_NS` | y | - | - |
| containers | `SECCOMP`, `SECCOMP_FILTER`, `POSIX_MQUEUE`, `KEYS` | y | - | - |
| container limits (cgroup v2) | `MEMCG`, `CGROUP_PIDS`, `CPUSETS`, `CGROUP_BPF`, `BPF_SYSCALL` | y | - | - |
| container limits (cgroup v2) | `CGROUPS`, `CGROUP_SCHED`, `CFS_BANDWIDTH` | y | y | y |
| image layers | `OVERLAY_FS` | m | - | - |
| pod network | `VETH`, `BRIDGE` | m | y | y |
| pod network | `BRIDGE_NETFILTER`, `VXLAN`, `DUMMY` | m | - | - |
| Service addresses | `NF_CONNTRACK`, `NF_NAT`, `IP_NF_NAT`, `IP_NF_MANGLE`, `IP_NF_TARGET_MASQUERADE`, `IP_NF_TARGET_REJECT` | m | - | - |
| Service addresses | the `xt` matches `addrtype`, `conntrack`, `comment`, `multiport`, `statistic`, `mark` | m | - | - |
| Service addresses | `IP6_NF_IPTABLES`, `IP6_NF_FILTER`, `IP6_NF_NAT` | m | - | - |
| network policies | `IP_SET`, `NETFILTER_XT_SET` | m | - | - |

hvx's kernel is nixpkgs' `linux_6_6` (`0a9ede35…`), a distribution kernel, and it has every
option. The networking options are modules. A module file belongs to one kernel build, and the
nixpkgs store path of that build (`rp8637p7…-linux-6.6.35`) holds all of them. Milestone 158
checked that this store path holds hvx's `bzImage` byte for byte. hydo's images carried only 14 of
these modules before: the ones for network faults, and the ones for service networks.

The KVM and Mac kernels were built from `allnoconfig` and a short list of options, and they have
no module support. They lack the container namespaces, seccomp, the memory and pids controllers,
overlayfs, connection tracking, and NAT. A Kubernetes node cannot run without these.

## No kernel change on hvx

On hvx, k3s needs no new kernel. The image carries the 57 modules that k3s needs, with their
dependencies, decompressed: 4.4 MB. Before k3s starts, the service's `setup:` loads them with
`insmod` in the order of `modules.dep`. The kernel bytes do not change, so nothing recorded
moves, and the fingerprint epoch stays where it is. The modules are files of one image, as a
service's binary is.

The example builds this with `root.files:` and a script, `fetch-k3s.sh`, that resolves the
modules' dependencies. hydo could do the same for any project with a format-1 addition, a list
`guest.modules:` that the builder resolves against the release's module tree. The Linux release
ships 14 modules today. The 57 modules for k3s add about 1.3 MB compressed. This is not built.

## The Mac and KVM engines: a kernel variant, not an epoch change

The Mac and KVM engines need new kernels with the options of the table. Such a kernel can be an
opt-in variant, chosen by the manifest, and it does not move any recorded value:

- A format-1 addition such as `guest.kernel: containers` asks for the variant. No recorded image
  has the key, so every recorded image keeps its kernel.
- The Mac engine names an image's kernel by sha256 in the image record (Milestone 155), so the
  record of an image with the key names the variant's sha256. The run code does not change.
- The KVM engine finds its kernel by the hvx kernel that the record names, and by the image's
  epoch (`hydo_kvm::kernel::twin_for`, Milestone 234). The variant needs one more field in the
  record, for example `kernel_variant`, that `twin_for` reads. An image record without it is read
  as before.
- The variant's options are the list of `hv/kvm/linux/hydo-kvm-cgroups.config` with the options
  of the table, all built in. The options that a timer updates in the background, such as
  pressure accounting (`CONFIG_PSI`), stay off, as in Milestone 234.

The epoch rule (AGENTS.md) raises the epoch for a change that moves a recorded fingerprint.
Here no recorded fingerprint moves, so the variant is not an epoch change. Each variant still
needs its pins. Each of the two Mac kernels and the two KVM kernels must build twice to the same
bytes, the KVM ones with a sandboxed Nix on GitHub's runners (`docs/BUILD-PINS.md`). That
is the cost of the four kernels of Milestone 234 again, and it was not spent here. The k3s
binary exists for arm64 too, so the Mac engine could run the same example with such a variant.

## The variant, as built (Milestone 256)

The manifest key `guest.kernel: containers` is a format-1 addition. `hydo build` writes the
variant into the image's record as `kernel_variant`. An image without the key has a record
without the field, so its bytes do not change.

| engine | kernel | what the key does |
|---|---|---|
| the Mac engine | `Image-explore-containers` (`b6211b80…`) | the record names this kernel by sha256 |
| KVM, 1 vCPU | `bzImage-kvm-containers` (`0c9c1c95…`) | `twin_for` picks it by the record's `kernel_variant` |
| KVM, 2 to 8 vCPUs | `bzImage-kvm-smp-containers` (`6f85c1c5…`) | the same, with the cpuset controller |
| hvx | its one `bzImage` | nothing: the kernel has every option, the network ones as modules |

Each variant takes the list of the kernel with cgroups for its engine, drops the options that
its own list names, and appends that list (`hv/hvm/explore/hvm-containers.config`,
`hv/kvm/linux/hydo-kvm-containers.config`). The lists hold every `y` option of the first table,
and `CONFIG_DUMMY`, `CONFIG_NF_CT_NETLINK`, and the nftables options. Every option is built in.
Pressure accounting (`CONFIG_PSI`) stays off. The KVM engine's one-vCPU kernel has no cpuset
controller, because that controller needs SMP; the kubelet uses only the controllers that the
kernel has. `docs/BUILD-PINS.md` "The guest kernels for containers" has the pins and how each
kernel was built twice.

On hvx the key changes nothing. hvx's guest kernel has all these options, so it needs no variant,
and the key is not refused: an x86_64 image with the key runs on hvx and on KVM. An image for hvx
that needs the network options carries their modules and loads them itself, as `../hydo.yaml`
of the example does.

**The probe.** `hydo/onboard/examples/k3s-lease/containers/probe.yaml` checks the kernel without
k3s, with k3s's own `runc`, `iptables`, `ipset` and `busybox`. It runs 16 checks:

- the cpu, memory and pids controllers, and overlayfs;
- a container that runc starts with a memory and a pids limit;
- NAT through iptables-legacy, iptables-nft and ip6tables;
- the conntrack, comment, multiport, statistic and mark matches;
- connection tracking, bridged traffic through iptables, ipsets, and VXLAN.

On the Mac engine, with the key, all 16 passed, and two runs gave one fingerprint,
`bc9b021a4c1cc0c0`. The same image without the key failed 13 of them: its kernel has only the
cpu controller, no overlayfs, no NAT table, and no nftables.

The container ran as PID 1 of its own PID namespace, with the host name `runc`, `memory.max`
67108864 and `pids.max` 64. On the Mac engine runc needs `--no-pivot`: the image's root is the
initramfs, and the kernel refuses `pivot_root` from it. containerd has the same option
(`no_pivot`).

**k3s on the Mac engine.** The image of `containers/hydo.yaml` boots the variant, and k3s's setup
runs: the cgroup v2 mount, the overlay over k3s's files and the dummy interface. Then the boot
stops with `ERROR boot: SPIN` at 31 s of guest time (VM exit 51,150). One thread of k3s runs in
`runtime.selectgo`, called from the watcher of kine (k3s's SQLite store, kine v0.17.1,
`pkg/server/watch.go`), and makes no VM exits. The Mac engine's clock counts VM exits, so no
timer interrupt comes, the kernel never preempts the thread, and the thread it waits for never
runs (open item 13). The loop draws a new random number in every pass, so the engine's spin
observer cannot prove it a cycle. With `HVM_SPIN_SECS=120` the run was still in the loop after
10 minutes. With k3s's embedded etcd (`--cluster-init`) the boot reached 119.5 s of guest time
and stopped in another such loop. On hvx the PMU clock interrupts such loops, so k3s runs there.

**k3s on KVM.** The one-off workflow `tmp-k3s-kvm` builds the two KVM kernels in a sandboxed
Nix on a GitHub runner and runs the probe and k3s on KVM with 1 and 2 vCPUs. The KVM engine's
clock also counts VM exits, so k3s's loop can stop the boot there too. `docs/HISTORY.md`
Milestone 256 has the results.



k3s expects a writable Linux host. hydo's image has a read-only root, a data disk on `/data`,
and only the loopback interface. The example bridges the gap in its `k3s-setup.sh`, which runs
before every start of the service:

- **Writable state.** Each directory that k3s writes (`/var/lib/rancher`, `/var/lib/kubelet`,
  `/var/lib/cni`, `/var/log`, `/var/tmp` and `/etc/rancher`) is a bind mount of a directory on
  the data disk. `/run` is a tmpfs, and the image's `/var/run` is a link to it.
- **k3s's own files.** At its first start, k3s unpacks 270 MB of programs (containerd, runc, the
  CNI plugins, iptables, kubectl) into its data directory. The example unpacks them once, at build
  time, onto the read-only root. An overlayfs mount puts a writable layer on the data disk over
  them, because k3s's iptables wrapper makes links in that directory.
- **cgroup v2** is mounted on `/sys/fs/cgroup`.
- **A network interface.** k3s needs an interface with an address and a default route. A dummy
  interface `k3s0` gets `10.77.0.2/24` and a default route. flannel uses its host-gw backend, so
  the pods' traffic stays on the node's bridge.
- **Container images.** The pause image and busybox, pinned by digest, are in k3s's import
  directory. k3s imports them at every start. Nothing is pulled; the guest has no network.

k3s runs without seven optional parts:

- the Traefik ingress controller and the service load balancer;
- the metrics server and the local storage provisioner;
- CoreDNS;
- the network policy controller and the Helm controller.

## Determinism

The question was whether something in k3s escapes hvx's determinism: timers, the Go scheduler's
choice among goroutines, random IDs, or reads of the CPU's time stamp counter (the TSC). None
did. Two fresh boots of one schedule gave the same fingerprint, the same pod addresses, and the
same Lease holder. `RESULTS.md` lists the runs.

Each source is fixed by hvx itself, not by anything that k3s does:

- The guest has one vCPU. The module's clock counts the guest's own events: the VM exits, or the
  retired branches with the PMU branch clock. Timer interrupts arrive at the same counts. So
  the Go runtime's threads take turns at the same instructions in every run.
- `RDTSC` and the clocks that come from it follow the same count.
- Random IDs, TLS keys and certificates come from the kernel's random number generator, which
  `hvx_seedrng` seeds the same way in every run.
- The realtime clock starts at `guest.clock_start`, so the certificates' validity and every
  timestamp are the same.

## The proof

The example runs a StatefulSet of 3 pods. Each pod runs `elect.sh`, a leader election on the Lease
`elect` written as client-go's `leaderelection` package does it. A pod takes the Lease when the
holder's `renewTime` plus the lease duration has passed on its own wall clock. The holder renews
every 2 s, and it acts as leader until 5 s have passed on its monotonic clock since its last
successful renewal. A leading pod writes a line every half second to its log on the data disk.

The rules are hydo invariants:

- `elect.one_leader`: no two pods act as leader at the same instant.
- `elect.leader_again`: a pod leads again within 60 s after the faults heal.
- `elect.statefulset_healed`: the StatefulSet has its 3 ready replicas again within 120 s.

The knob `Z=<step>` kills the leading pod's process from the node before that step. hydo's own
`T=<step>:<seconds>` steps the wall clock, as an NTP step does on a real node. `K=k3s` kills
k3s, which is the control plane, the kubelet, and containerd together.

The results, on hvx with one vCPU (the example's `RESULTS.md` has every fingerprint):

- **Start.** The guest reaches the service's setup at 6.6 s of uptime, and the node is Ready at
  15.3 s, about 8 s after k3s starts. The 3 pods are ready, and one leads, at 37.6 s. This is
  provisioning, before the snapshot point S1, so a schedule starts from a running cluster.
- **Memory.** A guest of 768 MiB is enough: 236 MB were still available with the 3 pods ready.
  The k3s process peaks at 485 MiB.
- **Wall time.** A run of 16 steps took 76 s to 90 s on the bare-metal machine, while other
  sessions loaded it.
- **Determinism.** Seven schedules ran twice from a fresh boot, on five builds of the image, and
  each pair gave one fingerprint. Two pairs broke a rule both times.
- **A crashing leader.** `Z=4` kills the leader's process before steps 4 and 5. The kubelet starts
  it again at once the first time, and the pod resumes its own Lease. After the second crash the
  kubelet waits, and another pod takes the Lease when the 10 s lease duration has passed. The rule
  holds, and the StatefulSet heals.
- **The control plane down.** `K=k3s@4+2` and `P=k3s@4+5` keep one leader. During the pause the
  leader cannot renew and steps down after 5 s, and it takes the Lease again when k3s runs.
- **A clock step.** `T=2:30` steps the wall clock 30 s forward. A candidate finds the Lease
  expired and takes it, while the holder still acts as leader for 0.67 s of guest time, by its
  monotonic clock. client-go documents this limit: a Lease does not fence the old leader. The
  step breaks the rule only when a candidate reads the Lease before the holder renews it. Of the
  steps before steps 1 to 6, only the one before step 2 did.
- **A step back.** The explorer found that `T=8:-86400`, one day back, leaves no leader: every
  client call fails with `x509: certificate has expired or is not yet valid`. k3s makes its
  certificates valid from one hour before their creation. The cluster was made at
  `guest.clock_start`, so a step back of more than an hour puts the clock before them. A real
  node whose clock goes back past its certificates' start fails the same way.

## What is left

- k3s on the Mac engine: a loop without VM exits stops its boot (open item 13). The kernel
  variant is built; the engine needs a deterministic way to interrupt such a loop.
- `guest.modules:` in hydo, so that a project names modules and the release supplies them.
- Faults on pods by name. hydo's faults name services, and here the only service is k3s. A
  kill of one pod is a knob of the example's own workload.
- More than one node. Each k3s node would be one service with its own network namespace
  (`network: bridge`), and the pods' traffic would cross hydo's network faults.
- Operators from images: an operator's image must be pinned and imported like busybox here.

# hydo on Linux: quickstart

hydo runs your software in a deterministic virtual machine and injects faults into it. The faults
are crashes, pauses, clock jumps, network delay and loss, disk errors, a disk that loses
acknowledged writes, and power cuts. Every run is deterministic, so each failure it finds comes
back as a short schedule string that replays bit-for-bit, with the same fingerprint. This guide
takes you from installing to your first exploration and a reproduced failure.

Words this page uses:

- Guest: the Linux system that runs in hydo's virtual machine. Image: the disk file that holds a
  guest's programs and data, which `hydo build` writes.
- Workload: the commands that act as your system's client during a run. It is split into
  numbered steps; a step is one part of it, for example one write.
- Schedule: one line of text that names the faults of one run, `<host tokens> | run <guest
  tokens>`. Host tokens set up the machine, for example `disk=cut` (a disk that loses unflushed
  writes at a power cut) or `dseed=<hex>` (the seed of the disk's faults). Guest tokens place a
  fault before a step of the workload, for example `C=12` (a power cut before step 12).
  `hydo guide faults` lists the tokens.
- Fingerprint: a hash of the guest's whole final state. The same schedule on the same image
  gives the same fingerprint.
- Verdict: the run's outcome: `OK`, `VIOLATION` (an invariant, a property that must always hold,
  failed), or `ERROR` (the run could not finish its workload).
- Fingerprint epoch: the generation of hydo's fingerprints. A change that moves recorded
  fingerprints raises it; this release's is 2. Runs of different epochs compare only verdicts.
- Recovery: after a power cut, hydo boots the guest again on what the disk kept and runs the
  invariants there.
- Engine: the program that runs the virtual machine: hvx (hydo's kernel module) or KVM.
- Clock: hydo's guest time. The PMU is the CPU's performance-monitoring unit. With the PMU
  branch clock, guest time advances with the guest's conditional branches; with the exit-count
  clock, it advances with the virtual machine's exits to the engine.

## What you need

hydo has two engines on Linux, and each needs different things:

- The KVM engine runs on the KVM that comes with Linux, on an Intel or AMD CPU, with no kernel
  module and no root: your user needs only access to `/dev/kvm`. Install with
  `install.sh --no-module` and follow [section 6](#6-run-without-the-kernel-module-kvm); the
  example in sections 2 to 5 works the same on KVM. Without the kernel module (no `/dev/hvx`),
  hydo uses the KVM engine by itself; on a machine with both engines, add `--backend kvm`.
- The kernel module `hvx.ko`, which sections 1 to 5 use, needs what the rest of this list says.
  It adds several virtual CPUs, a power cut at the k-th disk flush (`cf=`), and inserted timer
  ticks (extra timer interrupts between the regular ones, which change thread interleavings).

For the kernel module:

- An x86_64 Linux machine with an Intel CPU and VT-x (Intel's hardware support for virtual
  machines) enabled in the firmware. Bare metal is best.
- A CPU with a performance-monitoring unit that is not a hybrid (P-core/E-core) design gives hydo
  its PMU branch clock. The unit counts the guest's conditional branches, so a timer tick lands at
  an exact instruction. Without one, hydo uses the exit-count clock. Runs are still
  deterministic, but their fingerprints differ from the ones printed below.
- Root through `sudo`: loading the kernel module needs it, and so does opening `/dev/hvx`
  unless you install the optional udev rule (see "Runs without sudo" below).
- The headers of the running kernel (Debian/Ubuntu: `sudo apt install linux-headers-$(uname -r)`),
  a C compiler and `make` (`sudo apt install build-essential`) for the kernel module, `xz`, and
  `flock`. Nothing else: no Python, Go, or Rust (every hydo program comes built).
- KVM must not be running virtual machines: hvx takes VT-x over from KVM. Without root, or on a
  machine where KVM must stay, use hydo's KVM engine instead (section 6): it needs only
  `/dev/kvm`.
- No Nix and no internet access: the release carries the guest kernel, the guest tools, and the
  disk-image tools, pinned by sha256.

## 1. Install

If the module is already loaded on this machine and someone else manages it, install with
`--no-module` instead, so that the installer neither builds nor loads a module:
`./hydo-linux-0.3.3/install.sh --no-module`. Otherwise:

```sh
tar xzf hydo-linux-0.3.3.tar.gz
./hydo-linux-0.3.3/install.sh                 # or: --prefix DIR (default ~/.hydo), --add-to-path
export PATH="$HOME/.hydo/bin:$PATH"
```

The installer copies the release into `~/.hydo`; everything else is done by the `hydo` program:

- `hydo setup` checks every file against the release's `SHA256SUMS`, and against the sha256 pins
  built into `hydo`: the guest kernel, the guest tools, the disk-image tools, and the guest
  programs. It unpacks the guest tool tree, and makes the programs executable. A damaged or
  incomplete download is refused before it replaces an earlier installation. Running
  `hydo setup` again does nothing on a complete installation; `hydo setup --check` only reports
  (exit status 0: nothing to do, 1: a file is damaged, 3: setup has steps left).
- `hydo module install` builds the `hvx` kernel module for your running kernel and loads it under
  the module lock (see below). Loading asks for `sudo` and unloads `kvm_intel` and `kvm` if they
  are loaded. Run again, it does nothing while this release's module is loaded; after a kernel
  update it builds the module again. `hydo module uninstall` unloads it and deletes the build.
- `hydo doctor` checks this machine and the installation.

The run engine needs no compiling: it runs inside `hydo` itself when you can open
`/dev/hvx` (as root, or after `hydo module udev install`), or as the program `bin/hydo-hvx` under
`sudo` when you cannot. The installer writes nothing outside `~/.hydo` except your shell startup
file (only with `--add-to-path`) and the udev rule (only with `--udev`). If the module is already
loaded (a machine where someone else manages it), `./hydo-linux-0.3.3/install.sh --no-module`
skips `hydo module install`; `hydo doctor` then warns that `hvx.ko` was not built by this
release. In that case run `hydo module build`, not `hydo module install`: it builds this
release's module without loading it, and `hydo module status` then says whether the loaded module
has the same source. On a machine without `/dev/hvx` where your user can open `/dev/kvm`, the installer runs
`hydo doctor --backend kvm` instead (section 6), which ends `doctor: all checks passed`. If the module could not be installed (for example without kernel headers or a C
compiler), fix what the message says, then run:

```sh
hydo module install
hydo doctor
```

`hydo doctor` checks these things, and every line says what to do when a check fails:

- VT-x, the PMU, the kernel headers, and whether `kvm_intel` is loaded;
- the module and its device, and sudo;
- the installation's files (what `hydo setup --check` would fix), and disk space.

`hydo module status` shows whether the module is loaded, and how many runs hold the module lock
right now. After `hydo module build` (or `hydo module install`) it also shows whether the loaded
module has this release's source: the same `srcversion`, the hash the kernel computes over a
module's source. Before a build it says `release build: not built`.

**Runs without sudo (optional).** `/dev/hvx` belongs to root with mode 0600, so by default every
run calls `sudo`, and a password prompt can block the explorer's workers. To let your user run
guests without sudo, install the udev rule once:

```sh
hydo module udev install          # or, at install time: ./install.sh --udev
```

It creates the group `hydo` if it is missing (`--group NAME` picks another), adds you to it,
and installs `/etc/udev/rules.d/70-hydo-hvx.rules`, which gives `/dev/hvx` to that group with
mode 0660. Log out and in again (or run `newgrp hydo`) so your shell is in the group. Then
`hydo doctor` says `/dev/hvx can be opened by you: runs do not use sudo`. `hydo run` and
`hydo explore` no longer call sudo (`HYDO_SUDO=1` forces it). Loading and unloading the module
still need sudo. `hydo module udev status` shows the rule and the device;
`hydo module udev remove` deletes the rule and gives the device back to root.

What this means for security: anyone in the group can run guests through the hvx kernel module.
The module runs in the kernel, and each guest takes a CPU and up to 8 GiB of memory (its
`guest.ram_mib`). The module is a test tool that was not hardened against hostile users, so a bug
in it could let a group member crash the machine or worse. Give the group only to people you
would also give sudo on that machine.

**The module lock.** Every run holds a shared lock on `~/.hvx-module.lock` (`$HYDO_LOCK`) for as
long as its virtual machine exists. `hydo module install`, `load`, `unload`, and `uninstall` take
the same lock exclusively. So they wait until no run is in progress, instead of pulling the module
from under a running VM. `--dry-run` prints what they would do. The lock file is in your home
directory, so it covers only your own runs. Another user's runs do not hold it, for example the
runs of another member of the udev group. Other tools that open `/dev/hvx` should use the same lock
(`flock -s ~/.hvx-module.lock <command>`).

## 2. Run the bundled example: Redis

The example is unmodified Redis 7.2 with its append-only file on the guest's data disk, and a
workload that stores 20 keys. Its one invariant says that every write the client saw acknowledged
is still there afterwards.

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
```

This writes `hydo-out/explore-ob-redis-disk.img`, and next to it the steps of the build
(`*-ext4.img`, `*-data.img` and a `*-root` folder). The image has a read-only root partition with Redis
and `hydo-agent`, plus a data partition. `hydo-agent` is a small program that is the guest's first
process and only carries out what the host tells it. Every decision (when to start Redis, when to
inject a fault, when to check the invariant) is made by `hydo` on the host, which also records it.
Building the same `hydo.yaml` again gives the same bytes (sha256 `668bf8adfdc64b2a…`). The image names the guest kernel it was built for, by sha256, in
`/etc/hydo/image.json`. So a later hydo boots the same kernel, or says which one it needs. Next to
the image, `explore-ob-redis-disk.img.json` records which hydo built it (version and commit) and
the manifest's sha256.

Run one schedule with no faults. Then run one with a power cut before step 12, while Redis is set
to sync its log only once a second. The workload's 20 steps count from 0, and step 5, for
example, writes the key `k:5`. `R=everysec` is a guest token of this example: it sets Redis's
`appendfsync` to `everysec` for this run, so Redis syncs its log once a second (`appendfsync
always`, the default, syncs after every write).

```sh
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

Each prints the engine's `RESULT` line (shortened here; `…` stands for fields left out):

```
RESULT one fp=5ec14b8b4bdcf367 wfp=30e74366625f3474 secs=0.4 stops=23604 typed=21 … inh=… ctl=c24534f57aab6d42 verdict=HYDO-ORACLE: OK invariants=1 held (redis.acked_writes_survive: acked=20 all present)
RESULT one fp=9f8f90a7d14566df wfp=f4de09236076bc87 secs=0.9 stops=24042 typed=30 … inh=… ctl=a98cc29af3d645f4 verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
```

- `verdict=` is the outcome. `HYDO-ORACLE: OK` means every invariant held; `VIOLATION` names the
  invariant that failed and why. Here the power cut lost `k:1`, which Redis had already
  acknowledged; the check stops at the first lost key and names it. That is
  Redis's documented `appendfsync everysec` window, and the run reproduces it every time.
- `fp=` is the fingerprint: a hash of the guest's whole final state (memory, CPU, disks,
  console). That it comes back every time is what bit-for-bit reproduction means. `wfp=` is the
  same hash taken at the end of the workload, before recovery.
- `ctl=` is the hash of the run's control transcript: every command the host sent to the guest
  and the point at which it sent it. `stops=` and `typed=` count the points at which the host
  looked at the guest and the commands it typed. `inh=` is a hash of that typed input: each
  command's bytes and the guest time at which it was typed.
- `secs=` is how long the run took.

With Redis's default setting, `appendfsync always`, the same cut loses nothing:

```sh
hydo run "disk=cut | run C=12"
```

It ends `OK` with `acked=12 all present` (`f8af29d45e9502f8`; with `--backend kvm`
`ff4fb00ab4f76b6b`).

`hydo run` exits with status 0 whenever the run finishes, whatever its verdict. A script tells a
pass from a failure by `verdict.kind` in the run's `result.json`: `ok`, `violation`, or `error`.

Run either command again: the fingerprint is the same. Each run leaves a directory under
`hydo-out/runs/` with these files:

- the guest's console output (`one.log`), and the agent's own console in `one.agent.log`, which
  is often empty;
- the control transcripts (`control.boot.jsonl`, `control.jsonl`);
- after a power cut, the recovery boot's files: `control.recovery.jsonl`, `one.recovery.log` and
  `one.recovery.agent.log`;
- the engine's logs (`one.engine.err`, `engine.log`), after a power cut the agent's events
  (`agent-events.jsonl`), and `result.json` (the run's result in hydo's results format).

`hydo repro <that directory>` runs it again from a fresh boot and compares the fingerprint:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=9f8f90a7d14566df (recorded 9f8f90a7d14566df)  disk=cut | run C=12 R=everysec
             HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
hydo: 1 of 1 reproduced with the recorded fingerprint (consoles under hydo-out/runs/run-…/repro-…)
```

These fingerprints were recorded on a Xeon E3-1270 v6 with the PMU branch clock. The exit-count
clock gives other fingerprints with the same verdicts.

## 3. Explore

Let hydo generate fault schedules, run them, re-run every failure to check that it reproduces,
and shrink each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm --workers 2
hydo report
```

`--workers N` is how many virtual machines run at a time. Without it, hydo sizes the pool from
the machine's CPUs and free memory. Each worker boots the guest once and starts every schedule
from a snapshot of it; hvx keeps the snapshots in memory, the KVM engine in files. The strategy
decides how schedules are drawn: `swarm` switches whole kinds of faults on and off per schedule,
and `process` injects faults as a process over the run. `hydo guide faults` lists the other
options and explains them.

In a terminal, `hydo explore` ends with how many runs failed, in how many ways, and whether
each failing run failed again when hydo replayed it. `hydo report` prints each problem it found:

- the problem in one line, how often it happened, and a label (`Worth a look`, `Expected for
  this setting` or `Expected`);
- what happens, and whether it is a bug;
- the `hydo run` command that shows it again, with the fewest faults that still cause it.

`hydo report --html` writes the same problems as one web page, with each failure's timeline.
`hydo report --details` prints the technical report. It gives each failure's class (the cause,
such as `lying-disk`) and signature (the verdict with keys and numbers blanked), whether it
reproduced, the minimal schedules, and a table of properties. The table lists hydo's own `fault.*`
properties (which fault the runs reached) and the invariants. A program that uses hydo's SDKs
adds its own assertions there (`hydo guide sdks`). This example has none. Its invariant is
marked `[the project's own check]`, as every invariant and check of a `hydo.yaml` is. `NOT EXERCISED` means no run
reached a property.

For this example expect only these classes in `hydo report --details`:

- a disk that loses acknowledged writes (`lying-disk`);
- injected disk errors (`io-errors`);
- the relaxed `appendfsync` settings with a power cut (`unsafe-knob`; a knob is a setting of the
  system that a guest token changes, here `R=`).

With the default settings and an honest disk, Redis loses nothing (the `disk=cut | run C=12`
run of section 2). On the machine of section 2 it found 9 failures (io-errors 4, lying-disk 3, unsafe-knob 2), and all
of them reproduced. The two minimal schedules were
`disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no` (`d3c11e15f3835b06`) and
`disk=lying:2 dseed=17144527b62d669c | run C=17` (`32676d17c82aa953`).

To re-run every failure of an exploration from a fresh boot and compare the fingerprints:

```sh
hydo repro hydo-out/explore
```

It ends `hydo: 11 of 11 reproduced with the recorded fingerprint` (the 9 failures and the 2
minimal schedules).

## 4. Network faults

`hydo-netem.yaml` is the same example plus an invariant that counts TCP retransmissions, and it
allows network faults. Its image carries `tc` and the guest's netem modules (netem is Linux's
network emulator, which delays, drops, or reorders packets):

```sh
hydo build hydo-netem.yaml
hydo run --image hydo-out/explore-ob-redis-netem-disk.img "disk=none | run N=redis@3-6:delay,300ms"
```

It ends `OK` with `retrans=0` (`dc095cf7124dd3ac`; on the KVM engine `45ff2e9ddab67aa5`; image
sha256 `8a402198b0099369…`): no packet was sent twice. With two images in `hydo-out`, `hydo
run` and `hydo explore` need `--image` to know which one you mean.

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hydo/onboard/MANIFEST.md` describes every field; `~/.hydo/examples/redis/hydo.yaml` is a
complete example.

The release includes the onboarding skill for AI coding tools of any vendor, as an Agent Skills
folder (https://agentskills.io). It reads your repository and writes the project's `hydo/`
folder. Run `hydo init` in your project, then tell your AI tool to run `hydo guide skill` and
follow it. To write the folder yourself, start with `hydo guide workflow`.

`hydo init` writes `hydo/hydo.yaml`, `hydo/workload/parallel_driver_todo`,
`hydo/checks/final_todo` and `hydo/regressions/README.md`. It adds three lines to `.gitignore`
and a hydo section to `AGENTS.md`, which it creates when it is missing. The folder uses the test
composer: each client operation is one command in `hydo/workload/`, and each check is one
program in `hydo/checks/` (`hydo guide composer`). The Redis example is older: its `hydo.yaml`
holds a numbered-steps workload and its invariants (`hydo guide numbered-steps`).
`hydo build hydo/hydo.yaml --check` checks the folder's format; it warns while the two
placeholder files are still there, because every run fails with them.
`hydo skill install --agent claude|codex|cursor|copilot|gemini|all` copies the skill to where
that tool reads skills: `.claude/skills/` for Claude Code, `.agents/skills/` for the others.
Review the checks it proposes: that is the one step that needs your judgement.

Your programs must be x86_64 Linux programs. Each can be a static build, the files of a package
with the shared libraries the program loads, or a Docker/OCI image saved with `docker save`
(`services[].image:`, see MANIFEST.md). List files under `root.files:` with paths relative to
`hydo.yaml`. `root.nix:` (a Nix closure) needs `nix-store` on the machine that runs `hydo build`;
everything else builds without Nix. Then: `hydo build hydo/hydo.yaml`, `hydo run "disk=none |
run"`, `hydo explore`. The image of a `hydo/` folder goes to `.hydo/` instead of `hydo-out/`.

Two ready-made tools come with the release:

- `hydo-read` (in `guest/`) reads a MySQL, PostgreSQL, Kafka, Redis, or etcd store as JSON
  Lines. In an image it also answers as `hydo-check`, which compares two readings. `hydo build`
  puts them into an image when a command of the manifest calls them (MANIFEST.md, section
  `Reading the stores`).
- `hydo-grpc-gen <proto dir> --out <dir>` writes gRPC drivers for your services from their
  `.proto` files, with the static client `hydo-grpc` copied from `guest/` (MANIFEST.md, section
  `gRPC services`).

## 6. Run without the kernel module (KVM)

hydo has a second engine for Linux that needs no kernel module and no root: it runs the same
images on the KVM that comes with Linux (`/dev/kvm`). Use it on a machine where you cannot load
`hvx` (no root, a cloud VM with nested virtualization, a machine that runs other KVM virtual
machines). It needs x86_64 Linux 5.10 or later with `/dev/kvm` usable by your user. Run
`hydo doctor --backend kvm` first. Add yourself to the group `kvm` only if it says that
`/dev/kvm` is not usable (`sudo usermod -aG kvm "$USER"`, then log in again). It runs on Intel
and AMD CPUs. Its fingerprints differ between CPU vendors, and on Intel between CPU models; its
verdicts do not.

```sh
./hydo-linux-0.3.3/install.sh --no-module     # nothing to build or load
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor --backend kvm
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml                           # the same image as in section 2
hydo run --backend kvm --image hydo-out/explore-ob-redis-disk.img "disk=none | run"
hydo run --backend kvm --image hydo-out/explore-ob-redis-disk.img "disk=cut | run C=12 R=everysec"
hydo explore --backend kvm --image hydo-out/explore-ob-redis-disk.img --seeds 1-20 --strategy swarm --workers 2
hydo repro hydo-out/explore
```

If you did sections 2 to 4 already, skip the `cp` line (it would copy the example into
`~/hydo-redis/redis`) and run `cd ~/hydo-redis` instead. `--image` names the image, because
section 4 left a second one in `hydo-out`. Without the kernel module (no `/dev/hvx`), hydo uses
the KVM engine by itself, so `--backend kvm` is needed only on a machine that has both engines.

`HYDO_BACKEND=kvm` selects it for every command instead of `--backend kvm`. The engine runs inside
`hydo` (or as `bin/hydo-kvm` with `HYDO_KVM=process`) and boots its own build of the guest kernel,
`guest/bzImage-kvm`, which takes all its time from the engine's clock. Runs are deterministic, with
the same verdicts as with hvx, but they are counted differently:

- Guest time advances with the virtual machine's exits to the engine; there is no PMU branch
  clock.
- A guest has one virtual CPU, or 2 to 8 with `--vcpus N` (`hydo run --backend kvm --vcpus 2
  …`). Several vCPUs take turns of VM exits and boot the kernel's build for several CPUs,
  `guest/bzImage-kvm-smp` (`hydo guide flaky-tests`). The vCPUs run at the x86-64-v2 level (the
  x86_64 instruction set with SSE4.2 and POPCNT, no AVX), or with `guest.cpu: v3` at x86-64-v3
  (AVX2) on a host that has it.
- The fingerprints are this engine's own. `hydo repro` of an hvx run with `--backend kvm` (or the
  other way round) compares verdicts, not fingerprints, and says so.

A host power cut at the k-th disk flush (`cf=`) and inserted ticks (`pp=`, `xt…`) are hvx's
alone; the explorer draws the faults both engines apply. With the image of section 2, the two
schedules end `OK` (`b04d1932dfb20e09`) and `VIOLATION … key k:1 lost` (`1b61c2c7501a6f25`).
These values come from an Intel Core i7-13700HX. Its performance cores and its efficiency cores
give the same values. On Intel a KVM fingerprint holds for one CPU model: a Xeon Platinum 8370C
and a Xeon 6975P-C each gave the same verdicts with fingerprints of their own. The AMD EPYC
models measured (7763, 9V45, 9V74) gave one set of fingerprints among them, other than Intel's,
with the same verdicts.

On the Intel machine the exploration finds 11 failures, and its report gives three minimal
schedules:

- `disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20` (`lying-disk`, a lost key);
- `disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no` (`unsafe-knob`);
- `disk=lying:6 dseed=b3c3d806a5d34604 | run C=5` (`lying-disk`: the data disk cannot be
  mounted after the cut, `Data consistency error`).

`hydo repro hydo-out/explore` ends `hydo: 14 of 14 reproduced with the recorded fingerprint`.
`hydo repro` of a run or an exploration uses the engine that recorded it, so it needs no
`--backend`.

## Notes

- `/dev/hvx` is mode 0600, so runs use `sudo` unless you installed the udev rule (section 1).
- To remove hydo: `hydo module uninstall` (not needed when you installed with `--no-module`),
  then `chmod -R u+w ~/.hydo && rm -rf ~/.hydo ~/.hvx-module.lock`. `chmod` is needed because the
  guest tools under `~/.hydo/guest/store/` are read-only.
- A build leaves read-only files in `hydo-out/` too (the `*-root` folders of images with the
  guest network tools), so remove a project's outputs with `chmod -R u+w hydo-out && rm -rf
  hydo-out`. After sections 2 to 4, `~/hydo-redis` holds about 580 MB.

## Licenses and source code

hydo is licensed under the Apache License 2.0 (`LICENSE`); the kernel module's source in
`module/` is licensed under GPL-2.0-only (`licenses/hydo/hv/hvx/`). The release also contains
this third-party software:

- the guest kernel and its modules, and busybox;
- the iproute2 and iptables tools with their libraries;
- e2fsprogs and libfaketime with their glibc;
- the Redis example's binaries;
- the Rust standard library, and the Rust crates compiled into hydo's programs.

`THIRD-PARTY.md` lists each one with its version, its license, the files it is in, and where its
source is. `licenses/` holds the full text of every license.

The archive `hydo-linux-0.3.3-sources.tar.gz` accompanies this release; it is published next to
the tarball, with the same `SHA256SUMS`. It contains the corresponding source:

- the exact upstream source archives of every third-party component, including every GPL- and
  LGPL-licensed one;
- the patches and configurations applied to them;
- the build recipes.

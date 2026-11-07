# hydo on Linux: quickstart

hydo runs your software in a deterministic virtual machine and injects faults: crashes, pauses,
clock jumps, network delay and loss, disk errors, a disk that loses acknowledged writes, and
power cuts. Every run is deterministic, so each failure it finds comes back as a short schedule
string that replays bit-for-bit, with the same fingerprint. This guide takes you from
installing to your first exploration and a reproduced failure.

## What you need

- An x86_64 Linux machine with an **Intel** CPU and VT-x enabled in the firmware. Bare metal is
  best. A CPU with a performance-monitoring unit that is not a hybrid (P-core/E-core) design
  gives hydo its fast "PMU clock"; without one hydo uses the slower exit-count clock (runs are
  still deterministic, but their fingerprints differ from the ones printed below).
- Root through `sudo`: loading the kernel module needs it, and so does opening `/dev/hvx`
  unless you install the optional udev rule (see "Runs without sudo" below).
- The headers of the running kernel (Debian/Ubuntu: `sudo apt install linux-headers-$(uname -r)`),
  a C compiler and `make` (`sudo apt install build-essential`) for the kernel module, `xz` and
  `flock`. Nothing else: no Python, Go or Rust (every hydo program comes built).
- KVM must not be running virtual machines: hvx takes VT-x over from KVM. Without root, or on a
  machine where KVM must stay, use hydo's KVM engine instead (section 6): it needs only
  `/dev/kvm`.
- No Nix and no internet access: the release carries the guest kernel, the guest tools and the
  disk-image tools, pinned by sha256.

## 1. Install

```sh
tar xzf hydo-linux-0.1.0.tar.gz
./hydo-linux-0.1.0/install.sh                 # or: --prefix DIR (default ~/.hydo), --add-to-path
export PATH="$HOME/.hydo/bin:$PATH"
```

The installer copies the release into `~/.hydo`; everything else is done by the `hydo` program:

- `hydo setup` checks every file against the release's `SHA256SUMS` and against the sha256 pins
  built into `hydo` (the guest kernel, the guest tools, the disk-image tools and the guest
  programs), unpacks the guest tool tree and makes the programs executable. A damaged or
  incomplete download is refused before it replaces an earlier installation. Running
  `hydo setup` again does nothing on a complete installation; `hydo setup --check` only reports
  (exit status 0: nothing to do, 1: a file is damaged, 3: setup has steps left).
- `hydo module install` builds the `hvx` kernel module for your running kernel and loads it under
  the module lock (see below). Loading asks for `sudo` and unloads `kvm_intel` and `kvm` if they
  are loaded. Run again, it does nothing while this release's module is loaded; after a kernel
  update it builds the module again. `hydo module uninstall` unloads it and deletes the build.
- `hydo doctor` checks this machine and the installation.

The run engine needs no compiling: it is Rust, and it runs inside `hydo` itself when you can open
`/dev/hvx` (as root, or after `hydo module udev install`), or as the program `bin/hydo-hvx` under
`sudo` when you cannot. The installer writes nothing outside `~/.hydo` except your shell startup
file (only with `--add-to-path`) and the udev rule (only with `--udev`). If the module is already
loaded (a machine where someone else manages it), `./hydo-linux-0.1.0/install.sh --no-module`
skips `hydo module install`; `hydo doctor` then warns that `hvx.ko` was not built by this
release. If the module could not be installed (for example without kernel headers or a C
compiler), fix what the message says, then run:

```sh
hydo module install
hydo doctor
```

`hydo doctor` checks VT-x, the PMU, the kernel headers, whether `kvm_intel` is loaded, the
module and its device, sudo, the installation's files (what `hydo setup --check` would fix) and
disk space; every line says what to do when a check
fails. `hydo module status` shows whether the module is loaded, whether it was built from this
release's source (the same `srcversion`), and how many runs hold the module lock right now.

**Runs without sudo (optional).** `/dev/hvx` belongs to root with mode 0600, so by default every
run calls `sudo`, and a password prompt can block the explorer's workers. To let your user run
guests without sudo, install the udev rule once:

```sh
hydo module udev install          # or, at install time: ./install.sh --udev
```

It creates the group `hydo` if it is missing (`--group NAME` picks another), adds you to it,
and installs `/etc/udev/rules.d/70-hydo-hvx.rules`, which gives `/dev/hvx` to that group with
mode 0660. Log out and in again (or run `newgrp hydo`) so your shell is in the group; then
`hydo doctor` says "/dev/hvx can be opened by you: runs do not use sudo", and `hydo run` and
`hydo explore` no longer call sudo (`HYDO_SUDO=1` forces it). Loading and unloading the module
still need sudo. `hydo module udev status` shows the rule and the device;
`hydo module udev remove` deletes the rule and gives the device back to root.

What this means for security: **anyone in the group can run guests through the hvx kernel
module.** The module runs in the kernel; each guest takes a CPU and up to 3 GiB of memory, and
the module is a test tool that was not hardened against hostile users, so a bug in it could let
a group member crash the machine or worse. Give the group only to people you would also give
sudo on that machine.

**The module lock.** Every run holds a shared lock on `~/.hvx-module.lock` (`$HYDO_LOCK`) for as
long as its virtual machine exists; `hydo module install`, `load`, `unload` and `uninstall` take the same
lock exclusively, so they wait until no run is in progress instead of pulling the module from
under a running VM. `--dry-run` prints what they would do. Other tools that open
`/dev/hvx` should use the same lock (`flock -s ~/.hvx-module.lock <command>`).

## 2. Run the bundled example: Redis

The example is unmodified Redis 7.2 with its append-only file on the guest's data disk, a
workload that stores 20 keys, and one invariant: every write the client saw acknowledged is
still there afterwards.

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
```

This writes `hydo-out/explore-ob-redis-disk.img`: a read-only root partition with Redis and
`hydo-agent`, a small program that is the guest's first process and only carries out what the
host tells it, plus a data partition. Every decision (when to start Redis, when to inject a
fault, when to check the invariant) is made by `hydo` on the host, which also records it.
Building the same `hydo.yaml` again, on this or another machine, gives the same bytes (sha256
`d255fb2bff5209bc…`). The image names the guest kernel it was built for (by sha256, in
`/etc/hydo/image.json`), so a later hydo boots the same kernel or says which one it needs; next to
it, `explore-ob-redis-disk.img.json` records which hydo built it (version and commit) and the
manifest's sha256.

Run one schedule with no faults, then one with a power cut before write 12 while Redis is set
to sync its log only once a second:

```sh
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

Each prints the engine's `RESULT` line (shortened here):

```
RESULT one fp=1893d8c55d7704d9 wfp=1421b01fdc984975 secs=0.4 stops=23608 typed=21 … ctl=d38223b17189d9a9 verdict=HYDO-ORACLE: OK invariants=1 held (redis.acked_writes_survive: acked=20 all present)
RESULT one fp=3520af95868b4e8f wfp=fce0cfbd850a0e9d secs=0.9 stops=24042 typed=30 … ctl=59ae8f3720816102 verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
```

- `verdict=` is the outcome. `HYDO-ORACLE: OK` means every invariant held; `VIOLATION` names the
  invariant that failed and why. Here the power cut lost key `k:1`, which Redis had already
  acknowledged: that is Redis's documented `appendfsync everysec` window, found and reproduced
  deterministically. With the default `appendfsync always` the same cut loses nothing.
- `fp=` is the **fingerprint**: a hash of the guest's whole final state (memory, CPU, disks,
  console). The same schedule on the same image gives the same fingerprint every time; that is
  what "reproduces bit-for-bit" means. `wfp=` is the same kind of hash taken at the end of the
  workload, before recovery.
- `ctl=` is the hash of the run's control transcript: every command the host sent to the guest
  and the point at which it sent it. `stops=` and `typed=` count the points at which the host
  looked at the guest and the commands it typed.
- `secs=` is how long the run took.

Run either command again: the fingerprint is the same. Each run leaves a directory under
`hydo-out/runs/` with the guest's console output (`one.log`, and the agent's own console in
`one.agent.log`), the control transcripts (`control.boot.jsonl`, `control.jsonl`), the engine's
log and `result.json` (the run's result in hydo's results format). `hydo repro <that directory>`
runs it again from a fresh boot and compares the fingerprint:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=3520af95868b4e8f (recorded 3520af95868b4e8f)  disk=cut | run C=12 R=everysec
             HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
hydo: 1 of 1 reproduced with the recorded fingerprint (consoles under hydo-out/runs/run-…/repro-…)
```

These fingerprints were recorded on a Xeon E3-1270 v6 with the PMU clock, from the image with
the sha256 shown, both as a user whose runs use sudo and as root (the engine then runs inside
`hydo`): the two give the same fingerprints. A different image, or the exit-count clock, gives
different fingerprints with the same verdicts.

## 3. Explore

Let hydo generate fault schedules, run them, re-run every failure to check that it reproduces,
and shrink each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm --workers 2
hydo report
```

`--workers N` is how many virtual machines run at a time; without it hydo sizes the pool from
the machine's CPUs and free memory (each Redis machine needs about 1.5 GiB of RAM; snapshots
are kept in memory). Every explorer option works, for example `--strategy process`,
`--timing`, `--new-faults` or `--sweep cut`. The report lists every failure with its class and
signature, says whether it reproduced, and gives the minimal schedule of each failure kind with
a `hydo run` command that replays it. For this example expect only expected kinds: a disk that
loses acknowledged writes (`lying-disk`), injected disk errors (`io-errors`), and the relaxed
`appendfsync` settings with a power cut (`unsafe-knob`). With the default settings and an
honest disk, Redis loses nothing. On the machine above, the command took 14 s and found 11
failures (io-errors 4, lying-disk 5, unsafe-knob 2), all reproduced, with two minimal
schedules: `disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20` (`747a6fbfbceecaa3`) and
`disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no` (`89105a398516a2ea`).

To re-run every failure of an exploration from a fresh boot and compare the fingerprints:

```sh
hydo repro hydo-out/explore
```

It ends `hydo: 13 of 13 reproduced with the recorded fingerprint` (the 11 failures and the 2
minimal schedules).

## 4. Network faults

`hydo-netem.yaml` is the same example plus an invariant that counts TCP retransmissions, and it
allows network faults (its image then carries `tc` and the guest's netem modules):

```sh
hydo build hydo-netem.yaml
hydo run --image hydo-out/explore-ob-redis-netem-disk.img "disk=none | run N=redis@3-6:delay,300ms"
```

It ends `OK` with `retrans=0` (`0a706d74e43f2f4b`, image sha256 `bd4c979f8d6a448d…`): the
delayed packets were delivered when the fault was lifted. With two images in `hydo-out`, `hydo
run` and `hydo explore` need `--image` to know which one you mean.

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hydo/onboard/MANIFEST.md` describes every field; `~/.hydo/examples/redis/hydo.yaml` is a
complete example. The release includes an onboarding guide for AI coding tools of any vendor (an
Agent Skills folder, https://agentskills.io) that reads your repository and writes the project's
`hydo/` folder: run `hydo init` in your project, then tell your AI tool "run `hydo guide` and
follow it". `hydo skill install --agent claude|codex|cursor|copilot|gemini|all` copies the skill
where that tool reads skills (`.claude/skills/` for Claude Code, `.agents/skills/` for the others).
Review the checks it proposes: that is the one step that needs your judgement.

Your programs must be **x86_64 Linux binaries**: a static build, the files of a package with the
shared libraries the program loads, or a Docker/OCI image saved with `docker save`
(`services[].image:`, see MANIFEST.md). List files under `root.files:` with paths relative to
`hydo.yaml`. `root.nix:` (a Nix closure) needs `nix-store` on the build machine; everything else
builds without Nix. Then: `hydo build hydo.yaml`, `hydo run "disk=none | run"`, `hydo explore`.

Two ready-made tools come with the release. `hydo-read` and `hydo-check` (in `guest/`) read a
MySQL, PostgreSQL, Kafka, Redis or etcd store as sorted lines and compare two readings; `hydo
build` puts them into an image when a command of the manifest calls them (MANIFEST.md "Store
readers"). `hydo-grpc-gen <proto dir> --out <dir>` writes gRPC drivers for your services from
their `.proto` files, with the static client `hydo-grpc` copied from `guest/` (MANIFEST.md "gRPC
services").

## 6. Run without the kernel module (KVM)

hydo has a second engine for Linux that needs no kernel module and no root: it runs the same
images on the KVM that comes with Linux (`/dev/kvm`). Use it on a machine where you cannot load
`hvx` (no root, a cloud VM with nested virtualization, a machine that runs other KVM virtual
machines). It needs x86_64 Linux 5.10 or later with `/dev/kvm` usable by your user (often: be in
the group `kvm`); it has so far been run on Intel CPUs only.

```sh
./hydo-linux-0.1.0/install.sh --no-module     # nothing to build or load
hydo doctor --backend kvm
cd ~/hydo-redis && hydo build hydo.yaml        # the same image as above
hydo run --backend kvm "disk=none | run"
hydo run --backend kvm "disk=cut | run C=12 R=everysec"
hydo explore --backend kvm --seeds 1-20 --strategy swarm --workers 2
```

`HYDO_BACKEND=kvm` selects it for every command instead of `--backend kvm`. The engine runs inside
`hydo` (or as `bin/hydo-kvm` with `HYDO_KVM=process`) and boots its own build of the guest kernel,
`guest/bzImage-kvm`, which takes all its time from the engine's clock. Runs are deterministic, with
the same verdicts as with hvx, but they are counted differently: guest time advances with the
virtual machine's exits to the engine (there is no PMU clock), every guest has one virtual CPU at
the x86-64-v2 level (an image with several CPUs or `guest.cpu: v3` is refused), and the
fingerprints are this engine's own. `hydo repro` of an hvx run with `--backend kvm` (or the other
way round) compares verdicts, not fingerprints, and says so. A host power cut at the k-th disk
flush (`cf=`) and inserted ticks (`pp=`, `xt…`) are hvx's alone; the explorer draws the faults both
engines apply. With the image above, the two schedules end `OK` (`bfa03053d51bec8a`) and
`VIOLATION … key k:1 lost` (`8ed16d6da60c34c6`), on an Intel Core i7-13700HX on both its
performance and its efficiency cores.

## Notes

- Images do not depend on who builds them or where: `hydo build` sets every file in the root
  image to owner root, uses a fixed umask, ignores the machine's `/etc/mke2fs.conf`, and
  freezes the build clock. `HYDO_KEEP_OWNERS=1` keeps the building user's owner IDs instead
  (the image then matches a plain `hydo-build` run by the same user on the Nix build machine).
- `/dev/hvx` is mode 0600, so runs use `sudo` unless you installed the udev rule (section 1).
  The run engine keeps its snapshots in memory (it writes no snapshot directory).
- To remove hydo: `hydo module uninstall`, then `chmod -R u+w ~/.hydo && rm -rf ~/.hydo` (the guest
  tool tree is read-only).

## Licenses and source code

hydo is licensed under the Apache License 2.0 (`LICENSE`); the kernel module's source in
`module/` is licensed under GPL-2.0-only (`licenses/hydo/hv/hvx/`). The release also contains
third-party software: the guest kernel and its modules, busybox, the iproute2 and iptables tools
with their libraries, e2fsprogs and libfaketime with their glibc, the Redis example's binaries,
the Rust standard library and the Rust crates compiled into hydo's programs. `THIRD-PARTY.md`
lists each one with its version, license, the files it is in and where its source is, and
`licenses/` holds the full text of every license.

The archive `hydo-linux-0.1.0-sources.tar.gz` accompanies this release (it is published next to
the tarball, with the same `SHA256SUMS`). It contains the corresponding source: the exact
upstream source archives of every third-party component, including every GPL- and LGPL-licensed
one, the patches and configurations applied to them, and the build recipes.

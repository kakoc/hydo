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
  a C compiler and `make` (`sudo apt install build-essential`), Python 3.9 or later, `xz`,
  `flock`. `libipt-dev` is optional (coverage decoding for the coverage-guided tools).
- KVM must not be running virtual machines: hvx takes VT-x over from KVM.
- No Nix and no internet access: the release carries the guest kernel, the guest tools and the
  disk-image tools, pinned by sha256.

## 1. Install

```sh
tar xzf hydo-linux-0.1.0.tar.gz
./hydo-linux-0.1.0/install.sh                 # or: --prefix DIR (default ~/.hydo), --add-to-path
export PATH="$HOME/.hydo/bin:$PATH"
```

The installer checks the files against `SHA256SUMS`, copies the release into `~/.hydo`, compiles
the run engine (`hvx/hvx_explore`) and the `hvx` kernel module for your kernel, and runs
`hydo doctor`. It changes nothing outside `~/.hydo` and does not load the module.

Load the module (this unloads `kvm_intel` and `kvm` if they are loaded):

```sh
hydo module load
hydo doctor
```

`hydo doctor` checks VT-x, the PMU, the kernel headers, whether `kvm_intel` is loaded, the
module and its device, Python, sudo and disk space; every line says what to do when a check
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
long as its virtual machine exists; `hydo module load` and `hydo module unload` take the same
lock exclusively, so they wait until no run is in progress instead of pulling the module from
under a running VM. `--dry-run` prints what `load`/`unload` would do. Other tools that open
`/dev/hvx` should use the same lock (`flock -s ~/.hvx-module.lock <command>`).

## 2. Run the bundled example: Redis

The example is unmodified Redis 7.2 with its append-only file on the guest's data disk, a
workload that stores 20 keys, and one invariant: every write the client saw acknowledged is
still there afterwards.

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
```

This writes `hydo-out/explore-ob-redis-disk.img` (a read-only root partition with Redis and the
generic hydo init, plus a data partition). Building the same `hydo.yaml` again, on this or
another machine, gives the same bytes (sha256 `8c828d3ed9206506…`).

Run one schedule with no faults, then one with a power cut before write 12 while Redis is set
to sync its log only once a second:

```sh
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

Each prints the engine's `RESULT` line:

```
RESULT one fp=c513f10aee733aae wfp=178a4694a94a082b secs=2.6 verdict=HYDO-ORACLE: OK invariants=1 held (redis.acked_writes_survive: acked=20 all present)
RESULT one fp=ecef6f8fe6c39ec0 wfp=6913b248b3ac2d47 secs=18.3 verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:2 lost
```

- `verdict=` is the outcome. `HYDO-ORACLE: OK` means every invariant held; `VIOLATION` names the
  invariant that failed and why. Here the power cut lost key `k:2`, which Redis had already
  acknowledged: that is Redis's documented `appendfsync everysec` window, found and reproduced
  deterministically. With the default `appendfsync always` the same cut loses nothing.
- `fp=` is the **fingerprint**: a hash of the guest's whole final state (memory, CPU, disks,
  console). The same schedule on the same image gives the same fingerprint every time; that is
  what "reproduces bit-for-bit" means. `wfp=` is the same kind of hash taken at the end of the
  workload, before recovery.
- `secs=` is how long the run took.

Run either command again: the fingerprint is the same. Each run leaves a directory under
`hydo-out/runs/` with the guest's console output (`one.log`), the engine's log and `run.json`;
`hydo repro <that directory>` runs it again and compares the fingerprint:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=ecef6f8fe6c39ec0 (recorded ecef6f8fe6c39ec0)  disk=cut | run C=12 R=everysec
```

These fingerprints were recorded on a Xeon E3-1270 v6 with the PMU clock, from the image with
the sha256 shown. A different image, or the exit-count clock, gives different fingerprints with
the same verdicts.

## 3. Explore

Let hydo generate fault schedules, run them, re-run every failure to check that it reproduces,
and shrink each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

By default two virtual machines run at a time (`--workers N` changes it; each needs about
1.5 GiB of RAM and some space in `/var/tmp/hvx-snap`). Every `explore.py` option works, for
example `--strategy process`, `--timing`, `--new-faults` or `--sweep cut`. The report lists
every failure with its class and signature, says whether it reproduced, and gives the minimal
schedule of each failure kind with a `hydo run` command that replays it. For this example
expect only expected kinds: a disk that loses acknowledged writes (`lying-disk`), injected disk
errors (`io-errors`), and the relaxed `appendfsync` settings with a power cut (`unsafe-knob`).
With the default settings and an honest disk, Redis loses nothing.

To re-run every failure of an exploration from a fresh boot and compare the fingerprints:

```sh
hydo repro hydo-out/explore
```

## 4. Network faults

`hydo-netem.yaml` is the same example plus an invariant that counts TCP retransmissions, and it
allows network faults (its image then carries `tc` and the guest's netem modules):

```sh
hydo build hydo-netem.yaml
hydo run --image hydo-out/explore-ob-redis-netem-disk.img "disk=none | run N=redis@3-6:delay,300ms"
```

It ends `OK` with `retrans=0`: the delayed packets were delivered when the fault was lifted.

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hvx/onboard/MANIFEST.md` describes every field; `~/.hydo/examples/redis/hydo.yaml` is a
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

## Notes

- Images do not depend on who builds them or where: `hydo build` sets every file in the root
  image to owner root, uses a fixed umask, ignores the machine's `/etc/mke2fs.conf`, and
  freezes the build clock. `HYDO_KEEP_OWNERS=1` keeps the building user's owner IDs instead
  (the image then matches a plain `hydo-build` run by the same user on the Nix build box).
- `/dev/hvx` is mode 0600, so runs use `sudo` unless you installed the udev rule (section 1).
  Snapshots go to `/var/tmp/hvx-snap` (`$HYDO_SNAP_DIR`); a run without sudo that cannot write
  there (the directory was made by earlier sudo runs) uses `/var/tmp/hvx-snap-<your uid>`.
- To remove hydo: `hydo module unload`, then `chmod -R u+w ~/.hydo && rm -rf ~/.hydo` (the guest
  tool tree is read-only).

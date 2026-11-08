# hydo

hydo tests a distributed system under faults and replays every failure exactly. It runs your
services, their databases, and their client in one virtual machine. The execution is
deterministic: the same image and the same schedule give a bit-identical run. From many
generated schedules, hydo finds the runs that break an invariant of your system. It shrinks each
failure to a minimal schedule that you can replay.

The words this page uses:

- Image: what `hydo build` makes from your project's `hydo.yaml`. It is the virtual machine's
  disk. It holds your services and the commands that drive them.
- Workload: the commands that act as your system's client during a run, for example writes.
- Invariant: a property that must hold after every run, for example that every acknowledged
  write is still there. A command that you write checks it.
- Schedule: one line of text that names the faults of one run. The part before `|` sets the
  disk and timing faults of the whole machine. The part after `run` places faults at workload
  steps, and changes settings of your services. `hydo guide faults` lists every fault and its
  syntax.
- Fingerprint: a hash of the virtual machine's whole final state.

The current release is hydo 0.2.0. It is experimental: formats and commands can change between
versions.

## An example: Redis loses an acknowledged write

Each release contains an example that runs Redis 7.2 with a workload of 20 writes. Its invariant
says that every write the client saw acknowledged is still there after the run. The schedule
`disk=cut | run C=12 R=everysec` has two parts after `run`. `R=everysec` sets Redis to sync its
log once a second. `C=12` cuts the power before workload step 12.

After you install hydo ([Install](#install) says where each install puts the example), run:

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

Each run prints a result line. These two are from a recorded run on Linux, shortened at `…`:

```
RESULT one fp=1893d8c55d7704d9 … verdict=HYDO-ORACLE: OK … (… acked=20 all present)
RESULT one fp=3520af95868b4e8f … verdict=HYDO-ORACLE: VIOLATION […] key k:1 lost
```

- `verdict=` is the outcome. The power cut lost key `k:1`, which Redis had acknowledged. This is
  the documented window of Redis's `appendfsync everysec` setting.
- `fp=` is the fingerprint. Two runs of the same schedule on the same image and the same machine
  give the same fingerprint.

Each run writes a directory under `hydo-out/runs/`. `hydo repro` runs it again from a fresh boot
and compares the fingerprints:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=3520af95868b4e8f (recorded 3520af95868b4e8f)  disk=cut | run C=12 R=everysec
             HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
hydo: 1 of 1 reproduced with the recorded fingerprint (consoles under …)
```

Your fingerprints differ from these if your CPU or clock differs, and the verdicts are the same.
On a Mac the same schedule loses key `k:0`.

The values come from a run on an Intel Xeon E3-1270 v6 with the branch-counting clock.
[docs/REBASELINE.md](docs/REBASELINE.md) section 13.4 records them.

## Install

Each install gives you the `hydo` command. Then `hydo doctor` checks the machine and the
installation. The quickstarts take you from the install to your first exploration:
[Linux](hydo/dist/linux/QUICKSTART-linux.md) and [Mac](hydo/dist/mac/QUICKSTART-mac.md).

### macOS

hydo needs an Apple Silicon Mac with macOS 13 or later. With Homebrew:

```sh
brew install kakoc/tap/hydo
hydo doctor
```

The example is then in `$(brew --prefix hydo)/libexec/examples/redis`. Without Homebrew:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/hydo-mac-0.2.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/SHA256SUMS
shasum -a 256 -c --ignore-missing SHA256SUMS
tar xzf hydo-mac-0.2.0.tar.gz
./hydo-mac-0.2.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

The example is then in `~/.hydo/examples/redis`.

### Linux

Linux has two engines, so choose one:

- The kernel module `hvx.ko` needs an Intel CPU with VT-x and sudo. Its clock can count the
  guest's branches, and it supports several virtual CPUs.
- The KVM engine runs on Intel and AMD CPUs and needs only a `/dev/kvm` that your user can open.
  It needs no root, and it runs on a cloud VM with nested virtualization. It has one virtual CPU.

Download and check the release for both engines:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/hydo-linux-0.2.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar xzf hydo-linux-0.2.0.tar.gz
```

For the kernel module, the machine also needs a C compiler, `make`, `xz`, `flock`, and the running
kernel's headers. Do not install it while other virtual machines run under KVM: loading `hvx.ko`
unloads the kernel modules `kvm_intel` and `kvm`.

```sh
./hydo-linux-0.2.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

For the KVM engine, your user must be in the group that owns `/dev/kvm`, often `kvm`. Add yourself
with `sudo usermod -aG kvm "$USER"`, then log in again. Then:

```sh
./hydo-linux-0.2.0/install.sh --no-module
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor --backend kvm
```

Add `--backend kvm` to `hydo run`, `hydo repro` and `hydo explore`, or set `HYDO_BACKEND=kvm`
once. The example is in `~/.hydo/examples/redis` for both engines.

### With cargo

```sh
cargo install hydo --locked
hydo setup
hydo doctor
```

`cargo install` builds only the `hydo` command. `hydo setup` downloads the release for your
platform into `~/.hydo/versions/0.2.0` and checks every file by sha256. The example is then in
`~/.hydo/versions/0.2.0/examples/redis`. This install was tested on a Mac. On Linux it was only
compiled.

## Test your own project

1. Run `hydo init` in your repository. It creates the folder `hydo/` with a `hydo.yaml`, a
   `workload/` folder, and a `checks/` folder.
2. If your system has a `docker-compose.yaml`, run `hydo compose docker-compose.yaml`. It writes a
   first `hydo.yaml` from the Compose file.
3. Read `hydo guide workflow`. It says what to put in each file.
4. Run `hydo build hydo/hydo.yaml --check` after each change. It checks the folder without
   building.
5. Run `hydo build hydo/hydo.yaml`, then `hydo run "disk=none | run"`, then `hydo explore`.

An AI coding tool can write the folder for you with the onboarding skill. In Claude Code, run
`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`. For Codex, Cursor,
GitHub Copilot, or Gemini, run `hydo skill install --agent codex` (or `cursor`, `copilot`,
`gemini`). Any other tool can read the skill from `hydo guide skill`.

## What hydo does

- It injects faults from a seeded schedule:
  - process kills and pauses;
  - network delay, loss, and partitions between services;
  - power cuts, disks that lose acknowledged writes, and disk errors;
  - clock jumps and changes of thread timing;
  - errors from fake outside services. A fake outside service is a program in the virtual
    machine that stands in for a service your system calls, such as a payment API.
- It runs your software unmodified. A service is a Linux program, a set of files, or a Docker or
  OCI image.
- It explores. `hydo explore` generates schedules, runs each failure again to check that it
  reproduces, and shrinks it. On the Redis example on Linux,
  `hydo explore --seeds 1-20 --strategy swarm --workers 2` found 11 failures in 14 seconds. All 11
  are of the expected kinds: a lying disk, disk errors, and the relaxed sync setting. Each one
  reproduced.
- It has ready-made checks: linearizability and list-append checks of recorded histories, and
  readers for MySQL, PostgreSQL, Kafka, Redis, and etcd. Assertion SDKs exist for Rust, Go,
  Python, and JavaScript.

hydo reproduced two known upstream bugs from generated schedules, without being told where to
look:

- etcd 3.5.5: member revisions diverge after an online defragmentation and a crash
  ([etcd issue 14685](https://github.com/etcd-io/etcd/issues/14685)). It is absent in 3.5.6.
- NATS 2.10.22: a JetStream stream is deleted when every node stops shortly after the stream was
  created ([nats-server issue 6888](https://github.com/nats-io/nats-server/issues/6888)). It is
  fixed in 2.10.23.

## Limits

- A failure replays exactly only with the same engine, the same image, the same hydo version, and
  a CPU of the same model. A failure found on Linux does not replay on a Mac.
- Your services run as Linux programs: x86_64 programs on a Linux host, and arm64 programs on a
  Mac.
- The kernel module needs an Intel CPU with VT-x. The KVM engine runs on Intel and AMD CPUs. Its
  fingerprints differ between CPU vendors, and its verdicts do not.
- A guest has at most 3 GiB of RAM on Linux and 4 GiB on a Mac.
- Time inside the virtual machine is virtual, so hydo does not measure performance.
- Anyone who can open `/dev/hvx` can run guests through the kernel module, which runs with the
  rights of the kernel. Give that access only to people you would give sudo.
- With Secure Boot on, Linux loads only signed modules. Sign `hvx.ko` with a key that you enrolled
  as a machine owner key (MOK), or use the KVM engine.

## More

- `hydo guide` lists the topics of the guide built into your `hydo`. Start with
  `hydo guide workflow`.
- The repository has 26 example projects in
  [hydo/onboard/examples/](hydo/onboard/examples/), among them etcd, NATS, TigerBeetle, Docker's
  voting app, and a stack of MySQL, Kafka, and Redis. A release contains only the Redis example,
and on a Mac also `ledger-spin`.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains how hydo is built.
- [CONTRIBUTING.md](CONTRIBUTING.md) says how to build hydo from this repository and how to test
  it.

## License

hydo is licensed under the Apache License 2.0 ([LICENSE](LICENSE)). The kernel module
[hv/hvx/hvx.c](hv/hvx/hvx.c) is licensed under GPL-2.0-only. Its interface header `hvx_abi.h` is
licensed under GPL-2.0-only WITH Linux-syscall-note, or Apache-2.0.
[hv/hvx/LICENSE.md](hv/hvx/LICENSE.md) has the details.

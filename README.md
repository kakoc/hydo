# hydo

hydo tests a distributed system under faults and replays every failure exactly. It runs your
services, their databases, and their client in one virtual machine. The execution is
deterministic: the same image and the same schedule give a bit-identical run. A schedule is a
line of text that names the faults of one run, for example a power cut before write 12. From
many generated schedules, hydo finds the runs that break a property of your system. It shrinks
each failure to a minimal schedule that you can replay.

hydo 0.2.0 is the current release. It is experimental: formats and commands can change between
versions.

## An example: Redis loses an acknowledged write

The release contains an example with Redis 7.2. Its workload writes 20 keys. Its property says
that every write the client saw acknowledged is still there after the run. The schedule
`disk=cut | run C=12 R=everysec` sets Redis to sync its log once a second and cuts the power
before write 12. After an install from a tarball the example is in `~/.hydo/examples/redis`, and
the section [Install](#install) gives its place for the other installs:

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

These are the two result lines of a recorded run on Linux (shortened at `…`):

```
RESULT one fp=1893d8c55d7704d9 wfp=1421b01fdc984975 secs=0.4 stops=23608 typed=21 … ctl=d38223b17189d9a9 verdict=HYDO-ORACLE: OK invariants=1 held (redis.acked_writes_survive: acked=20 all present)
RESULT one fp=3520af95868b4e8f wfp=fce0cfbd850a0e9d secs=0.9 stops=24042 typed=30 … ctl=59ae8f3720816102 verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
```

- `verdict=` is the outcome. The power cut lost key `k:1`, which Redis had acknowledged. This is
  the documented window of Redis's `appendfsync everysec` setting.
- `fp=` is the fingerprint, a hash of the guest's whole final state. Each run of the same schedule
  on the same image gives the same fingerprint.

Each run writes a directory under `hydo-out/runs/`. `hydo repro` runs a recorded run again from a
fresh boot and compares the fingerprints:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=3520af95868b4e8f (recorded 3520af95868b4e8f)  disk=cut | run C=12 R=everysec
             HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
hydo: 1 of 1 reproduced with the recorded fingerprint (consoles under hydo-out/runs/run-…/repro-…)
```

The output comes from the Linux release's quickstart (`hydo/dist/linux/QUICKSTART-linux.md`). The
values are recorded in `docs/REBASELINE.md` section 13.4. On a Mac the same schedule loses key
`k:0`, with the fingerprint `a6870a92a1bbf0ae`.

## Install

hydo runs on an Apple Silicon Mac with macOS 13 or later, and on x86_64 Linux. Each install puts
the `hydo` command on your PATH. Then `hydo doctor` checks the machine and the installation.

### macOS with Homebrew

```sh
brew install kakoc/tap/hydo
hydo doctor
```

The example is then in `$(brew --prefix hydo)/libexec/examples/redis`. Without Homebrew, use the
tarball `hydo-mac-0.2.0.tar.gz` of the same release and its `install.sh`, as for Linux below. The
Mac quickstart is `hydo/dist/mac/QUICKSTART-mac.md`.

### Linux with the kernel module

This install needs an Intel CPU with VT-x, sudo, a C compiler, `make`, and the running kernel's
headers. It builds and loads hydo's kernel module `hvx.ko`.

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/hydo-linux-0.2.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.2.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar xzf hydo-linux-0.2.0.tar.gz
./hydo-linux-0.2.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

The Linux quickstart is `hydo/dist/linux/QUICKSTART-linux.md`, and the release has a copy as its
`README.md`.

### Linux without the kernel module

The KVM engine runs the same images on the KVM that comes with Linux. It needs no module and no
root, only a `/dev/kvm` that your user can open. Install the tarball with `--no-module`:

```sh
./hydo-linux-0.2.0/install.sh --no-module
hydo doctor --backend kvm
```

Then add `--backend kvm` to `hydo run` and `hydo explore`. The KVM engine gives the same verdicts
as the module, with its own fingerprints. It has one virtual CPU. Section 6 of the Linux
quickstart describes it.

### With cargo

```sh
cargo install hydo --locked
hydo setup
hydo doctor
```

`cargo install` builds only the `hydo` command. `hydo setup` downloads the release for your
platform into `~/.hydo/versions/0.2.0`, and checks every file by sha256. The example is then in
`~/.hydo/versions/0.2.0/examples/redis`. This install was tested on a Mac. On Linux it was only
compiled.

## What you get

- Faults from a seeded schedule:
  - process kills and pauses;
  - network delay, loss, and partitions between services;
  - power cuts, disks that lose acknowledged writes, and disk errors;
  - clock jumps and changes of thread timing;
  - errors and lost webhooks from fake outside services.
- hydo runs your software unmodified. A service is a program, a set of files, or a Docker or OCI
  image.
- An explorer: `hydo explore` generates schedules, runs each failure again to check that it
  reproduces, and shrinks it. On the Redis example on Linux,
  `hydo explore --seeds 1-20 --strategy swarm --workers 2` found 11 failures in 14 seconds, and
  each one reproduced (Linux quickstart, section 3).
- Ready-made checks: linearizability and list-append checks of recorded histories, readers for
  MySQL, PostgreSQL, Kafka, Redis, and etcd, and assertion SDKs for Rust, Go, Python, and
  JavaScript.
- A start from Docker Compose: `hydo compose docker-compose.yaml` writes a first `hydo.yaml`.
- An onboarding skill for AI coding tools. In Claude Code, run
  `/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`. For other
  tools, run `hydo skill install --agent codex` (or `cursor`, `copilot`, `gemini`).

hydo has found upstream bugs without being told where to look:

- etcd 3.5.5: member revisions diverge after an online defragmentation and a crash (etcd issue
  14685). It is absent in 3.5.6 (`docs/HISTORY.md` Milestone 60).
- NATS 2.10.22: a JetStream stream is deleted when every node stops shortly after the stream was
  created (nats-server issue 6888). It is fixed in 2.10.23 (Milestone 141).

## Limits

- A failure replays exactly only with the same engine, the same image, and the same hydo
  version. A failure found on Linux does not replay on a Mac.
- The kernel module supports Intel VT-x only, not AMD. The KVM engine has run only on Intel CPUs.
- In 0.2.0 a guest has at most 3 GiB of RAM on Linux and 4 GiB on a Mac.
- Time inside the virtual machine is virtual, so hydo does not measure performance.
- The kernel module runs with the rights of the kernel. Anyone who can open `/dev/hvx` can run
  guests through it, so give that access only to people you would give sudo.
- On a machine with Secure Boot, the module must be signed before it can be loaded.

## More

- `hydo guide` prints the guide built into your `hydo`: the project format, the faults, and the
  commands.
- `hydo/onboard/examples/` has 26 example projects. Among them are etcd, NATS, TigerBeetle,
  Docker's voting app, and a stack of MySQL, Kafka, and Redis.
- `docs/ARCHITECTURE.md` explains how hydo is built.
- `CONTRIBUTING.md` says how to build hydo from this repository and how to test it.
- `hydo/PRODUCT.md` describes hydo from a user's point of view, with its known gaps.

## License

hydo is licensed under the Apache License 2.0 (`LICENSE`). The kernel module `hv/hvx/hvx.c` is
licensed under GPL-2.0-only. Its interface header `hvx_abi.h` is licensed under GPL-2.0-only
WITH Linux-syscall-note, or Apache-2.0. `hv/hvx/LICENSE.md` has the details.

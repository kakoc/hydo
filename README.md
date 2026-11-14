<p align="center">
  <img src="docs/img/hero.svg" width="860" alt="A terminal: a clean Redis run ends OK, a power cut before step 12 loses the acknowledged key k:1, and hydo repro gets the same fingerprint again">
</p>

<h1 align="center">hydo</h1>

> What if the bug that shows up once in a thousand CI runs showed up every time?

hydo runs your whole system in one virtual machine where nothing is left to chance: your
services, their databases, and the client that drives them. Then it breaks things. It kills
processes, cuts the network, pulls the power, corrupts disks, and moves the clock.

The same schedule always gives the same run, bit for bit. A schedule is one line of text, so every
failure hydo finds is a line you can run again tomorrow, or paste into an issue.

- **Rare failures come back.** Qdrant's flaky test
  ([#10200](https://github.com/qdrant/qdrant/issues/10200)) never failed in 2,604 runs on ordinary
  machines. Under one hydo schedule it fails on every run
  ([the runs](hydo/onboard/examples/qdrant-flaky-10200/RESULTS.md)).
- **It finds bugs you did not aim at.** `hydo explore` found etcd's diverging revisions
  ([14685](https://github.com/etcd-io/etcd/issues/14685),
  [the runs](hydo/onboard/examples/etcd/RESULTS.md)) and a deleted NATS stream
  ([6888](https://github.com/nats-io/nats-server/issues/6888),
  [the runs](hydo/onboard/examples/nats-jetstream/RESULTS.md)) with no hand-written schedule.
- **Every failure shrinks** to a minimal schedule: hydo removes faults while the failure stays.
- **Your software runs unmodified:** Linux programs, plain files, or Docker images. A Docker
  Compose file imports with one command.
- **You can see why.** `hydo inspect` opens a recorded run at any point, with its files, logs, and
  processes.

## Install

hydo runs on macOS on Apple Silicon, and on x86_64 Linux. Linux has two engines:

- the KVM engine runs on Intel and AMD with the KVM that comes with the kernel, and needs no root;
- hydo's own kernel module, `hvx`, runs on Intel only and needs sudo once to load it. It counts
  guest time in executed branches, so it can also stop the guest at an exact instruction.

Install hydo in one of these ways. Each block ends with the command that copies the Redis example
of the next section.

With cargo, on a Mac or on Linux (`hydo setup` downloads the release's guest files, the Linux
kernel and tools that the virtual machine boots, and checks each one by sha256):

```sh
cargo install hydo --locked
hydo setup
cp -R ~/.hydo/versions/0.3.2/examples/redis ~/hydo-redis
```

With Homebrew, on a Mac:

```sh
brew install kakoc/tap/hydo
cp -R "$(brew --prefix hydo)/libexec/examples/redis" ~/hydo-redis
```

With the release tarball on a Mac:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.2/hydo-mac-0.3.2.tar.gz
tar xzf hydo-mac-0.3.2.tar.gz
./hydo-mac-0.3.2/install.sh --add-to-path
export PATH="$HOME/.hydo/bin:$PATH"
cp -R ~/.hydo/examples/redis ~/hydo-redis
```

With the release tarball on Linux, for the KVM engine (`--no-module` builds and loads no kernel
module; [docs/INSTALL.md](docs/INSTALL.md) installs the module):

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.2/hydo-linux-0.3.2.tar.gz
tar xzf hydo-linux-0.3.2.tar.gz
./hydo-linux-0.3.2/install.sh --no-module
export PATH="$HOME/.hydo/bin:$PATH"
cp -R ~/.hydo/examples/redis ~/hydo-redis
```

Then run `hydo doctor`. It checks the machine. For the KVM engine your user must be able to open
`/dev/kvm`, and `hydo doctor --backend kvm` checks that. [docs/INSTALL.md](docs/INSTALL.md) has
the checksums.

## Make Redis lose a write

The release ships a Redis example. Its client writes the keys `k:0` to `k:19`, and its check says
that every write Redis acknowledged must survive.

```sh
cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=cut | run C=12 R=everysec"
```

`R=everysec` tells Redis to sync its log once a second. `C=12` cuts the power before step 12, the
write of `k:12`. Redis had acknowledged `k:0` to `k:11`, and the check finds `k:1` missing after
the cut:

```
RESULT one fp=9f8f90a7d14566df … verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
```

`fp=` is the fingerprint, a hash of the machine's whole final state, and `hydo repro` gets the
same one again from a fresh boot. With Redis's default setting, `hydo run "disk=cut | run C=12"`
loses nothing. On a Mac the guest's timing differs, so there the check finds `k:0` missing; the
[Mac quickstart](hydo/dist/mac/QUICKSTART-mac.md) has its values.

You do not have to write schedules. `hydo explore` writes them from seeds, runs them, checks that
each failure reproduces, and shrinks it. The `swarm` strategy gives each schedule a random subset
of the fault kinds:

```sh
hydo explore --seeds 1-20 --strategy swarm --workers 2
hydo report
```

With hydo's kernel module this finds 9 failures, and on a Mac 11; every one reproduces.
`hydo report --html` writes the same report as one web page, with each failure's timeline and the
command that replays it, to attach to an issue.

## Your own project

```sh
hydo init
hydo build hydo/hydo.yaml --check
```

`hydo init` writes `hydo/hydo.yaml`, `hydo/workload/`, `hydo/checks/` and `hydo/regressions/`. It
also adds a hydo section to `AGENTS.md` and three lines to `.gitignore`. `--check` checks the
format only, and warns about the placeholder files that `hydo init` wrote.

You describe your services, the commands that act as their client, and the checks that must hold.
[docs/YOUR-PROJECT.md](docs/YOUR-PROJECT.md) walks through it, including the import of a Docker
Compose file. An AI coding tool can write the folder for you: in Claude Code, run
`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`.

## Learn more

- [The Linux quickstart](hydo/dist/linux/QUICKSTART-linux.md) and
  [the Mac quickstart](hydo/dist/mac/QUICKSTART-mac.md), from the install to the first exploration.
- `hydo guide`, the manual built into the command. Start with `hydo guide workflow`.
- [The example projects](hydo/onboard/examples/) in this repository, among them etcd, NATS,
  TigerBeetle, Qdrant, and a stack of MySQL, Kafka, and Redis. The release carries the Redis one.
- [What hydo does not do](docs/LIMITS.md).
- [How hydo is built](docs/ARCHITECTURE.md) and [how to contribute](CONTRIBUTING.md).

The current release is hydo 0.3.2. It is experimental: formats and commands can change between
versions.

## License

Apache-2.0 ([LICENSE](LICENSE)). The kernel module [hv/hvx/hvx.c](hv/hvx/hvx.c) is GPL-2.0-only;
[hv/hvx/LICENSE.md](hv/hvx/LICENSE.md) has the details.

# hydo

hydo tests a distributed system under faults and replays every failure exactly. It runs your
services, their databases, and their client in one virtual machine, and injects faults such as
crashes and power cuts. The execution is deterministic: the same inputs give a bit-identical run.
hydo finds the runs that break a property your system must keep, and shrinks each failure to the
fewest faults that still cause it. The list below defines the words the rest of this page uses.

The words this page uses:

- Guest: the Linux system that runs inside the virtual machine, with your services in it.
- Engine: the part of hydo that runs the guest. Linux has two engines: hydo's own kernel module
  `hvx.ko` and the KVM engine. The Mac has one.
- Image: what `hydo build` makes from your project's description file, `hydo.yaml`. On Linux it is one disk; on a
  Mac it is a root file system loaded into memory plus a data disk. It holds your services and
  the commands that drive them.
- Workload: the commands that act as your system's client during a run. A workload is split into
  numbered steps, and a step is one part of it, for example one write.
- Invariant: a property that must hold after every run, for example that every acknowledged write
  is still there. A command that you write checks it, or one of hydo's ready-made checks.
- Schedule: one line of text that names the faults of one run, in the form
  `<host tokens> | run <guest tokens>`. Host tokens are faults of the whole machine, such as
  `disk=cut` (the disk keeps only data that was flushed when the power is cut) or `disk=none` (no
  disk faults). Guest tokens place faults before a step, and change settings of your services.
  `hydo guide faults` lists the tokens.
- Verdict: the outcome of a run: `OK`, `VIOLATION` (an invariant failed), or `ERROR` (the run
  could not finish its workload).
- Seed: a number that starts hydo's pseudo-random choices. The same seed gives the same schedules.
- Fingerprint: a hash of the virtual machine's whole final state.

The current release is hydo 0.3.0. It is experimental: formats and commands can change between
versions.

## What hydo does

- It injects faults from a schedule:
  - process kills and pauses;
  - network delay and loss between services (100 % loss is a partition);
  - power cuts of the whole machine, and a disk that acknowledges writes and then drops them;
  - disk errors, and corruption of one service's files;
  - a power loss of one service, while the others keep running;
  - clock jumps and changes of thread timing;
  - errors from fake outside services. A fake outside service is a program in the guest that
    stands in for a service your system calls, such as a payment API. It can serve HTTPS.
- It runs your software unmodified. A service is a Linux program, a set of files, or a Docker or
  OCI image (OCI is the open standard format of container images).
- It explores. `hydo explore` generates schedules from seeds, runs each failure again to check
  that it reproduces, and shrinks it. In the Linux quickstart (section 3), an exploration of the
  Redis example with 20 seeds found 11 failures in 14 seconds. All 11 are of the expected kinds:
  a disk that drops writes, disk errors, and a relaxed sync setting. Each one reproduced.
- It shows why a run failed. A failed run names the failing service and process, and
  `hydo inspect` opens a recorded run at any point to read its files, logs, and processes.
- It has ready-made checks and workloads: checks of recorded histories of operations, readers for
  MySQL, PostgreSQL, Kafka, Redis, and etcd, and `hydo sql-gen`, which
  writes SQL workloads and their checks. A linearizability check finds a read that no order of
  the operations explains; a list-append check finds anomalies between transactions that append
  to lists. Assertion SDKs exist for Rust, Go, Python, and JavaScript (`hydo guide sdks`).

hydo reproduced two known upstream bugs. `hydo explore` found each one, with no hand-written
schedule:

- etcd 3.5.5: member revisions diverge after an online defragmentation and a crash
  ([etcd issue 14685](https://github.com/etcd-io/etcd/issues/14685)). `hydo explore --strategy
  process --new-faults --timing` failed 19 of 100 seeds on 3.5.5 and none on 3.5.6. The runs are in
  [hydo/onboard/examples/etcd/RESULTS.md](hydo/onboard/examples/etcd/RESULTS.md).
- NATS 2.10.22: a JetStream stream is deleted when every node stops shortly after the stream was
  created ([nats-server issue 6888](https://github.com/nats-io/nats-server/issues/6888)). The
  process strategy found it in 14 of 50 schedules, the first at seed 1; it is fixed in 2.10.23.
  The runs are in
  [hydo/onboard/examples/nats-jetstream/RESULTS.md](hydo/onboard/examples/nats-jetstream/RESULTS.md).

## Limits

- A run gives the same fingerprint again when four things are the same:
  - the engine;
  - the image;
  - the CPU model (for the KVM engine, the CPU vendor);
  - the fingerprint epoch, a number that hydo raises when a new version changes fingerprints.
    `hydo repro` says when the epoch differs.
- Across machines, the recorded examples gave the same verdicts. The Redis example gave the same
  verdict on Linux and on a Mac, and on the KVM engine on Intel and AMD CPUs. The quickstarts
  record these runs (Linux sections 2 and 6, Mac section 2).
- A failure found on Linux does not replay on a Mac. Your services run as x86_64 Linux programs
  on a Linux host, and as arm64 Linux programs on a Mac.
- The kernel module needs an Intel CPU with VT-x. The KVM engine runs on Intel and AMD CPUs.
- A guest has at most 8 GiB of RAM, on every engine.
- Time inside the virtual machine is virtual, so hydo does not measure performance.
- Anyone who can open `/dev/hvx` can run guests through the kernel module, which runs with the
  rights of the kernel. Give that access only to people you would give sudo.
- With Secure Boot on, Linux loads only signed modules. Sign `hvx.ko` with a key that you enrolled
  as a machine owner key (MOK), or use the KVM engine.

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

Without Homebrew:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.0/hydo-mac-0.3.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.0/SHA256SUMS
shasum -a 256 -c --ignore-missing SHA256SUMS
tar xzf hydo-mac-0.3.0.tar.gz
./hydo-mac-0.3.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

### Linux

Linux has two engines. Use the KVM engine unless you need one of three things: several virtual
CPUs, a power cut at an exact disk flush, or inserted timer ticks. Those need the kernel module on
your own Intel machine.

- The KVM engine uses the KVM that comes with Linux. It runs on Intel and AMD CPUs, and on a cloud
  VM with nested virtualization (a VM that can run VMs itself). It has one virtual CPU.
- The kernel module `hvx.ko` needs an Intel CPU with VT-x and sudo. It has the PMU branch clock:
  the CPU's performance-monitoring unit counts the guest's conditional branches, so a timer tick
  lands at an exact instruction.

Download and check the release for both engines:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.0/hydo-linux-0.3.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar xzf hydo-linux-0.3.0.tar.gz
```

For the KVM engine, your user must be able to open `/dev/kvm`. Install, then let
`hydo doctor --backend kvm` check it:

```sh
./hydo-linux-0.3.0/install.sh --no-module
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor --backend kvm
```

Only if it says that `/dev/kvm` is not usable, add yourself to the group that owns it, often
`kvm`: `sudo usermod -aG kvm "$USER"`, then log in again.

Add `--backend kvm` to `hydo run`, `hydo repro`, and `hydo explore`, or set `HYDO_BACKEND=kvm`
once.

For the kernel module, the machine also needs a C compiler, `make`, `xz`, `flock`, and the running
kernel's headers. Do not install it while other virtual machines run under KVM: loading `hvx.ko`
unloads the kernel modules `kvm_intel` and `kvm`.

```sh
./hydo-linux-0.3.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

### With cargo, on a Mac

```sh
cargo install hydo --locked
hydo setup
hydo doctor
```

`cargo install` builds only the `hydo` command. `hydo setup` downloads the release for your
platform into `~/.hydo/versions/0.3.0` (`HYDO_HOME` names another folder than `~/.hydo`) and
checks every file by sha256. On Linux, this path was
only compiled, not run; use the tarball there.

## An example: Redis loses an acknowledged write

Each release contains an example that runs Redis 7.2 with a workload of 20 steps. The steps count
from 0, and step 5, for example, writes the key `k:5`. The invariant `redis.acked_writes_survive` says that
every write the client saw acknowledged is still there after the run. The schedule
`disk=cut | run C=12 R=everysec` has two guest tokens. `R=everysec` sets Redis to sync its log
once a second. `C=12` cuts the power before step 12.

Copy the example to a folder of your own. The source depends on the install:

| Install | Command |
|---|---|
| Homebrew | `cp -R "$(brew --prefix hydo)/libexec/examples/redis" ~/hydo-redis` |
| Tarball | `cp -R ~/.hydo/examples/redis ~/hydo-redis` |
| cargo | `cp -R ~/.hydo/versions/0.3.0/examples/redis ~/hydo-redis` |

Then build the image and run two schedules:

```sh
cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

Each run prints one `RESULT` line. `verdict=` is the outcome, and `HYDO-ORACLE:` marks the verdict
of the invariants. These two lines are from a recorded run on Linux, shortened at `…`:

```
RESULT one fp=1893d8c55d7704d9 … verdict=HYDO-ORACLE: OK … (… acked=20 all present)
RESULT one fp=3520af95868b4e8f … verdict=HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
```

The power cut lost the keys from `k:1` on, which Redis had acknowledged (only `k:0` survived);
the verdict names the first. This is the documented window of Redis's `appendfsync everysec`
setting. `fp=` is the fingerprint. With Redis's default setting, `appendfsync always`, the same
cut loses nothing: `hydo run "disk=cut | run C=12"` ends `OK` with `acked=12 all present`.

Each run writes a directory under `hydo-out/runs/`. `hydo repro` runs it again from a fresh boot
and compares the fingerprints:

```
$ hydo repro hydo-out/runs/run-…
REPRODUCED   run  fp=3520af95868b4e8f (recorded 3520af95868b4e8f)  disk=cut | run C=12 R=everysec
             HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:1 lost
hydo: 1 of 1 reproduced with the recorded fingerprint (consoles under …)
```


## Test your own project

1. Run `hydo init` in your repository. It creates `hydo/hydo.yaml`, `hydo/workload/`,
   `hydo/checks/`, and `hydo/regressions/`. It adds `.hydo/`, `hydo/images/`, and `hydo-out/` to `.gitignore`,
   and a hydo section to `AGENTS.md`. Its two placeholder files make every run fail until you
   replace them.
2. If your system has a Docker Compose file, import it instead of the skeleton's `hydo.yaml`. The
   second command downloads the images, once:

   ```sh
   hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images
   sh hydo/fetch-images.sh
   ```

   The import writes a `workload:` and an `invariants:` section with placeholders. Delete both
   sections from `hydo/hydo.yaml`: the commands in `hydo/workload/` are the workload, and the
   checks in `hydo/checks/` are the invariants. Then replace `hydo init`'s placeholders
   `hydo/workload/parallel_driver_todo` and `hydo/checks/final_todo` with your own commands and
   checks. Until you delete the two sections, the check in step 4 fails with `workload.run/process:
   the test composer runs the workload`.

3. Read `hydo guide workflow`. It says what to put in each file.
4. Run `hydo build hydo/hydo.yaml --check` after each change. It checks the folder without
   building.
5. Build and run without faults, twice. Both runs must end `OK` with the same `fp=`:

   ```sh
   hydo build hydo/hydo.yaml
   hydo run "disk=none | run"
   hydo run "disk=none | run"
   ```

6. Explore, then read the findings:

   ```sh
   hydo explore --seeds 1-20 --strategy process
   hydo report
   ```

An AI coding tool can write the folder for you with the onboarding skill. In Claude Code, run
`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`. For Codex, Cursor,
GitHub Copilot, or Gemini, run `hydo skill install --agent codex` (or `cursor`, `copilot`,
`gemini`). Any other tool can read the skill from `hydo guide skill`.

## More

- `hydo guide` lists the topics of the guide built into your `hydo`. Start with
  `hydo guide workflow`. `hydo guide manifest` explains every key of `hydo.yaml`.
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

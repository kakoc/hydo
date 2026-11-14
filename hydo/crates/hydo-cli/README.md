# hydo: deterministic fault testing of whole systems

hydo runs your system inside a deterministic virtual machine and injects faults into it. You
describe the system once: its services, a workload that uses them, and checks of what must
stay true. An example of such a check: every write the client saw acknowledged can still be
read. The workload then runs under process kills and pauses, network faults, power cuts, clock
jumps, disk errors, and a disk that loses writes it acknowledged.

Every run is deterministic: the same *schedule* (a one-line description of a run and its
faults) gives the same run, bit for bit. So when hydo finds a failure, it gives you a short
schedule that replays it exactly, as often as you need while you debug it.

The services run unmodified. They come from their own programs or from Docker images. hydo runs on an
Apple Silicon Mac with macOS 13 or later, with no root needed. It also runs on x86_64 Linux,
with hydo's own kernel module on an Intel machine, or with stock KVM.

## Install

```sh
cargo install hydo
hydo setup
hydo doctor
```

`cargo install hydo` builds only the `hydo` command. `hydo setup` downloads the rest of the
installation from the GitHub release of the same version. That is the guest kernels, the
programs that run inside the virtual machine, the examples, and the engine program. It checks every file by
sha256 and keeps them in `~/.hydo/versions/<version>` (set `HYDO_HOME` to use another
directory). Running it again does nothing. `hydo doctor` checks the machine and the
installation, and names the fix for anything that is missing.

Other ways to install: Homebrew on a Mac (`brew install kakoc/tap/hydo`), or the release
archive for your platform from <https://github.com/kakoc/hydo/releases>.

## A first run

The installation contains a ready example: Redis with its append-only log on a virtual disk,
and a workload that writes 20 keys. Its one check says that every write the client saw
acknowledged is still there afterwards. Copy it, build its guest image, and run two schedules:

```sh
cp -R ~/.hydo/versions/0.3.3/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

- `disk=none | run` runs the workload without faults. The run passes.
- `disk=cut | run C=12 R=everysec` sets Redis to write its log to disk once a second
  (`R=everysec`) and cuts the power after write 12 (`C=12`). `disk=cut` makes the disk lose
  every write it had not flushed. The check fails: a key that the client saw acknowledged is
  gone. This is the loss window that Redis documents for `everysec`.

Each run prints a `RESULT` line with the verdict and a fingerprint of the run (`fp=`). Run
either command again and the fingerprint is the same.

Then let hydo look for failures by itself:

```sh
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

`hydo explore` makes one schedule from each seed, runs them all, runs every failure again to
confirm that it replays, and shrinks one failure of each signature to a minimal schedule. `hydo report`
prints what it found.

On Linux, a run uses hydo's kernel module (`hydo module install` builds and loads it, with
sudo). Without the module, add `--backend kvm` to `hydo run` and `hydo explore` to use stock
KVM instead.

## Test your own system

```sh
cd your-repository
hydo init
hydo build hydo/hydo.yaml --check
```

`hydo init` starts a `hydo/` folder: `hydo/hydo.yaml` names the services and how to start
them, `hydo/workload/` holds the commands that use the system, and `hydo/checks/` holds the
checks. `hydo build hydo/hydo.yaml --check` checks the folder without building an image. If
the system has a Docker Compose file, `hydo compose docker-compose.yaml` writes a starting
`hydo.yaml` from it.

The guide built into `hydo` has every fact you need to fill the folder: start with
`hydo guide workflow`, and `hydo guide` lists the other topics. An AI coding tool can do the
work: `hydo skill install --agent claude` (or `codex`, `cursor`, `copilot`, `gemini`) installs
hydo's onboarding skill where the tool reads skills.

Programs can also describe what must hold from the inside, with hydo's assertion SDKs for Rust
([hydo-assert](https://crates.io/crates/hydo-assert)), Python
([hydo-assert](https://pypi.org/project/hydo-assert/)), JavaScript
([hydo-assert](https://www.npmjs.com/package/hydo-assert)) and Go
([github.com/kakoc/hydo/sdk/go](https://pkg.go.dev/github.com/kakoc/hydo/sdk/go)).
`hydo guide sdks` says how.

## Reference

- `hydo --help` lists every command, and `hydo guide` the guide's topics.
- The project format (`hydo.yaml` and the `hydo/` folder):
  <https://github.com/kakoc/hydo/blob/main/hydo/onboard/MANIFEST.md>
- The faults and the schedule language: `hydo guide faults`.
- What the assertion records mean and how hydo judges them:
  <https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md>
- The source code, the issues and the releases: <https://github.com/kakoc/hydo>

## License

This crate and every other hydo crate are licensed under Apache-2.0 (the file `LICENSE`). A
Linux installation can also build hydo's kernel module, which is GPL-2.0 and is not part of this
crate.

# hydo on a Mac: quickstart

hydo runs your software in a deterministic virtual machine and injects faults: crashes, pauses,
clock jumps, disk errors, a disk that loses acknowledged writes, and power cuts. Because every
run is deterministic, each failure it finds comes back as a short schedule string that replays
bit-for-bit. This guide takes you from installing to your first exploration and a reproduced
failure. It needs an Apple Silicon Mac (M1 or later) with macOS 13 or later, and about ten
minutes. You do not need a Linux machine, root, or any other software beyond Apple's Command
Line Tools, which provide `python3` and `cc` (install them with `xcode-select --install` if
`python3 --version` asks you to).

## 1. Install

Unpack the release and run its installer. It copies hydo into `~/.hydo`, signs the two
virtual-machine binaries on your Mac, and checks the Mac.

```sh
tar xzf hydo-mac-0.1.0.tar.gz
./hydo-mac-0.1.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

The installer changes nothing outside `~/.hydo`. Add `--add-to-path` if you want it to put
`~/.hydo/bin` on your PATH in your shell startup file. `hydo doctor` repeats the checks at any
time.

## 2. Run the bundled example: Redis

The release contains a ready example: Redis 7.2 with its append-only file on a virtual disk, a
workload that stores 20 keys, and one invariant, "every write the client saw acknowledged is
still there afterwards". Copy it somewhere and build its guest image:

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
```

This writes `hydo-out/explore-ob-redis-arm64.cpio.gz` (the guest's root) and
`hydo-out/explore-ob-redis-arm64-disk.img` (its data disk). Building the same `hydo.yaml` again
gives the same bytes.

Run one schedule with no faults, then one with a power cut after write 12 while Redis is set to
sync its log only once a second:

```sh
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

The first prints a `RESULT` line with `verdict=HYDO-ORACLE: OK`. The second prints a violation:
an acknowledged key is gone after the power cut. That is Redis's documented `everysec` window,
found and reproduced deterministically. Run either command again and the fingerprint (`fp=`) is
the same.

## 3. Explore

Let hydo generate schedules, run them, re-run every failure to check it reproduces, and shrink
each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

The report lists every failure kind with its minimal schedule, the class hydo puts it in, and a
command that reproduces it. In this example you should see failures that are all expected kinds:
the disk that loses acknowledged writes (`lying`), injected disk errors (`ioerr`), and the
relaxed `appendfsync` settings. With the default settings and an honest disk Redis loses
nothing.

## 4. Reproduce a failure

Copy a minimal schedule from the report and run it:

```sh
hydo repro "disk=lying:1 dseed=… | run C=9"
```

It ends with the same verdict and the same fingerprint every time. The console of the run is
saved under `hydo-out/runs/`, so you can read what the software printed.

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hvx/onboard/MANIFEST.md` describes every field, and `~/.hydo/examples/redis/hydo.yaml` is
a complete example.

If you use Claude Code, the release includes a skill that reads your repository and writes the
manifest for you. Copy it into your project and ask Claude to onboard the project:

```sh
mkdir -p your-project/.claude/skills && cp -R ~/.hydo/skill/hydo-onboard your-project/.claude/skills/
```

Then review the invariants it proposes: that is the one step that needs your judgement.

**Your programs must be arm64 Linux binaries**, because the guest is arm64 Linux. Three ways to
get them:
- a static build (`GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build`, Rust with
  `--target aarch64-unknown-linux-musl`, or C with a musl cross compiler);
- the files of an arm64 Linux package, for example exported from a Docker image
  (`docker create --platform linux/arm64 IMAGE` then `docker cp`), with the shared libraries
  the program loads;
- the aarch64-linux build from the nixpkgs binary cache, as the Redis example does.

List each file under `root.files:` with a path relative to `hydo.yaml`. `root.nix:` needs a Nix
store on this Mac and is not supported by the Mac build yet.

Then the same three commands as above: `hydo build hydo.yaml`, `hydo run "disk=none | run"`,
`hydo explore`.

## What the Mac version does not do yet

- Network faults (`N=`, `O=`) are not applied: the guest has no `tc` yet.
- Thread-timing exploration (tick jitter) is not available yet.
- A program that busy-waits on a memory flag without any system call can stop a run; hydo marks
  such a run invalid instead of reporting a wrong result.
- One virtual CPU, and guest RAM up to a few GiB.

# hydo on a Mac: quickstart

hydo runs your software in a deterministic virtual machine and injects faults: crashes, pauses,
clock jumps, disk errors, a disk that loses acknowledged writes, and power cuts. Because every
run is deterministic, each failure it finds comes back as a short schedule string that replays
bit-for-bit. This guide takes you from installing to your first exploration and a reproduced
failure. It needs an Apple Silicon Mac (M1 or later) with macOS 13 or later, and about ten
minutes. You do not need a Linux machine, root, a compiler, Python, Go, Rust, Nix or Docker:
the release is complete as it is.

## 1. Install

Unpack the release and run its installer. It copies hydo into `~/.hydo`, signs the
virtual-machine engine (`bin/hydo-hvm`) on your Mac, and checks the Mac.

```sh
tar xzf hydo-mac-0.1.0.tar.gz
./hydo-mac-0.1.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

The installer changes nothing outside `~/.hydo`. Add `--add-to-path` if you want it to put
`~/.hydo/bin` on your PATH in your shell startup file. `hydo doctor` repeats the checks at any
time; on a complete installation it ends with `doctor: all checks passed`.

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

Network faults work the same way. This one drops every reply Redis sends during writes 5 to 9,
so the client times out on those five writes and does not count them as acknowledged:

```sh
hydo run "disk=none | run O=redis@5-10:resp:loss,100%"
```

It ends `OK ... acked=15 all present`: the 15 writes the client saw acknowledged are all there.
Again, a second run gives the same fingerprint.

hydo can also vary thread timing: the host part of a schedule can delay the guest's timer
interrupts by a seeded number of VM exits, which changes where the guest switches threads.
This delays about half of the ticks during the workload by up to 500 exits:

```sh
hydo run "disk=none tj=7 tjpm=500 tjmax=500 | run"
```

The `RESULT` line gains `tjk=` (the first and last tick period touched) and `tjn=` (how many
ticks were delayed). The fingerprint differs from the untimed run, because the timing changed
the guest's state, and it is the same on every run of this schedule. `hydo explore --timing`
adds such timing to half of the generated schedules.

### A program that busy-waits

Some programs wait in a tight loop on a memory value without any system call. A deterministic
machine cannot interrupt such a loop at a fixed point by itself, so hydo watches for it: it
proves that the loop repeats forever, then delivers the next timer interrupt at a point chosen
from the loop's own state, so the result does not depend on when hydo noticed. The second
example spins in every request (`/bin/spin_load`, and a loop that also counts, `/bin/spin_count`,
in every tenth one):

```sh
cp -R ~/.hydo/examples/ledger-spin ~/hydo-spin && cd ~/hydo-spin
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=none | run P=front@10+2"
```

The first ends `OK ... acked=30 each stored once`. On the way it prints that a run ended at a
counter loop and was run again in rewind mode: the counting loop needs the slower mode, which
hydo switches to on its own. The second pauses the web server for two requests, the client's
retry stores entry 10 twice, and the invariant catches it (`entry 10 stored 2 times`). This
example's server is deliberately not idempotent. Each command gives the same fingerprint every
time. A busy-wait run takes about ten seconds instead of one.

## 3. Explore

Let hydo generate schedules, run them, re-run every failure to check it reproduces, and shrink
each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

hydo runs as many virtual machines at once as the Mac's cores and memory allow; add
`--workers 2` to keep it to two. The report lists every failure kind with its minimal schedule,
the class hydo puts it in, and a command that reproduces it. In this example you should see
failures that are all expected kinds: the disk that loses acknowledged writes (`lying-disk`),
injected disk errors (`io-errors`), and the relaxed `appendfsync` settings (`unsafe-knob`). With
the default settings and an honest disk Redis loses nothing.

## 4. Reproduce a failure

Copy a minimal schedule from the report and run it:

```sh
hydo repro "disk=lying:3 dseed=… | run C=20"
```

It ends with the same verdict and the same fingerprint every time. The console of the run is
saved under `hydo-out/runs/`, so you can read what the software printed. To run every failure of
the exploration again and compare each fingerprint with the recorded one:

```sh
hydo repro hydo-out/explore
```

It ends `hydo: N of N reproduced with the recorded fingerprint`.

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hydo/onboard/MANIFEST.md` describes every field, and `~/.hydo/examples/redis/hydo.yaml` is
a complete example.

The release includes an onboarding guide for AI coding tools of any vendor (an Agent Skills
folder, https://agentskills.io): it reads your repository and writes the project's `hydo/`
folder. Start the folder, then give your tool one instruction:

```sh
cd your-project
hydo init                    # hydo/ skeleton, .gitignore lines, a hydo section in AGENTS.md
# then tell your AI tool: "run `hydo guide` and follow it"
```

`hydo skill install --agent claude|codex|cursor|copilot|gemini|all` also copies the skill into the
directory that tool reads skills from (`.claude/skills/` for Claude Code, `.agents/skills/` for
the others), so the tool finds it by itself. Then review the checks it proposes: that is the one
step that needs your judgement.

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

Then the same three commands as above: `hydo build hydo/hydo.yaml` (or `hydo build hydo.yaml`
for a single file), `hydo run "disk=none | run"`, `hydo explore`. `hydo build hydo/hydo.yaml
--check` checks the folder without building.

The release carries every guest file the arm64 build stages, so these work from it as well:
- `network: bridge`, `addr:` and `faults: [freeze]`: each service in its own network namespace,
  link faults between services and a freeze at an exact packet. Such an image is built for the
  release's second guest kernel (`Image-explore-netns`); the build says so, records it in the
  image, and `hydo run` and `hydo explore` boot that kernel by themselves;
- `fakes:` and `proxies:` (fake outside services and protocol faults), and the ready-made store
  readers `hydo-read` and `hydo-check` in invariants and checks.

## What the Mac version does not do yet

- A busy-wait loop is resolved only if it repeats exactly (the same registers and memory each
  time, or only counters that never feed back). Any other loop that never makes a system call
  marks its run invalid instead of giving a wrong result.
- One virtual CPU, and guest RAM up to a few GiB.

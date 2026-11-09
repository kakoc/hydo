# hydo on a Mac: quickstart

hydo runs your software in a deterministic virtual machine and injects faults: crashes, pauses,
clock jumps, disk errors, a disk that loses acknowledged writes, and power cuts. Because every
run is deterministic, each failure it finds comes back as a short schedule string that replays
bit-for-bit. This guide takes you from installing to your first exploration and a reproduced
failure. It needs an Apple Silicon Mac (M1 or later) with macOS 13 or later, and about ten
minutes. You do not need a Linux machine, root, a compiler, Python, Go, Rust, Nix or Docker:
the release is complete as it is.

Words this page uses:

- Guest: the arm64 Linux system that runs in hydo's virtual machine. Guest image: the files that
  hold a guest's programs and data, which `hydo build` writes.
- Schedule string, or schedule: one line of text that names the faults of one run,
  `<host part> | run <guest part>`. The host part sets up the machine, for example `disk=cut` (a
  disk that loses unflushed writes at a power cut) or `dseed=<hex>` (the seed of the disk's
  faults). The guest part places faults before steps of the workload, for example `C=12` (a power
  cut before step 12). `hydo guide faults` lists the tokens.
- Fingerprint: a hash of the guest's whole final state. The same schedule on the same image
  gives the same fingerprint.
- Verdict: the run's outcome: `OK`, `VIOLATION` (an invariant, a property that must always hold,
  failed), or `ERROR` (the run could not finish its workload).
- VM exit: a moment when the guest stops and the engine (`hydo-hvm`) handles something for it.
  hydo's guest time on a Mac counts VM exits. A tick period is the time between two timer
  interrupts of the guest.

## 1. Install

The easiest way is Homebrew:

```sh
brew install kakoc/tap/hydo
hydo doctor
```

The formula (in the tap https://github.com/kakoc/homebrew-tap) installs this release as it is
into `$(brew --prefix hydo)/libexec`, puts `hydo` on your PATH and runs `hydo setup` there. With
Homebrew, `~/.hydo` in the rest of this guide is that `libexec` directory:
`cp -R "$(brew --prefix hydo)/libexec/examples/redis" ~/hydo-redis` copies the first example.
`brew uninstall hydo` removes it again.

Without Homebrew, unpack the release, and run its installer. It copies hydo into `~/.hydo` and
runs `hydo setup` there, then `hydo doctor`.

```sh
tar xzf hydo-mac-0.3.1.tar.gz
./hydo-mac-0.3.1/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

`hydo setup` does three things:

- It checks that this Mac can run hydo: Apple Silicon, macOS 13 or later, and
  Hypervisor.framework.
- It checks every file against the release's `SHA256SUMS`, and against the sha256 pins built into
  `hydo`: the guest kernels, the guest tools, and the guest programs.
- It removes the quarantine attribute that a download leaves on the files.

The release's programs are `bin/hydo`, `bin/hydo-build`, `bin/hydo-fetch-image`, and the
virtual-machine engine `bin/hydo-hvm`, which carries the hypervisor entitlement. They are signed
with the project's Developer ID and notarized by Apple, so `hydo setup` leaves them as they are.
A damaged or incomplete download is refused with the name of each file that does not match. Running
`hydo setup` again does nothing on a complete installation; `hydo setup --check` only reports
(exit status 0: nothing to do, 1: a file is damaged, 3: setup has steps left).

The installer changes nothing outside `~/.hydo` and needs no sudo. Add `--add-to-path` if you
want it to put `~/.hydo/bin` on your PATH in your shell startup file (`~/.zshrc` for zsh,
`~/.bash_profile` for bash). `hydo doctor` repeats the checks at any time, including what
`hydo setup --check` would fix; on a complete installation it ends with
`doctor: all checks passed`.

## 2. Run the bundled example: Redis

The release contains a ready example: Redis 7.2 with its append-only file on a virtual disk, and
a workload that stores 20 keys. Its one invariant says that every write the client saw
acknowledged is still there afterwards. Copy it somewhere and build its guest image:

```sh
cp -R ~/.hydo/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
```

This writes into `hydo-out/`:

- `explore-ob-redis-arm64.cpio.gz`, the guest's root, and `explore-ob-redis-arm64-disk.img`, its
  data disk: together they are the guest image;
- `explore-ob-redis-arm64.cpio.gz.json`, the build record: which hydo built the image and from
  which manifest;
- `explore-ob-redis-arm64-data.img` and the folder `explore-ob-redis-arm64-root/`, the build's
  intermediate files.

Building the same `hydo.yaml` again gives the same bytes. `appendfsync` is the Redis setting that
decides when Redis writes its log to the disk: `always` (the example's default) after every
write, `everysec` once a second.

Run one schedule with no faults. Then run one with a power cut before step 12, while Redis is set
to sync its log only once a second. The workload's 20 steps count from 0, and step 5, for
example, writes the key `k:5`.

```sh
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
```

The first prints a `RESULT` line with `verdict=HYDO-ORACLE: OK`. The second prints a violation:
an acknowledged key is gone after the power cut (`key k:0 lost`). That is Redis's documented
`everysec` window, and the run reproduces it every time. Run either command again and the
fingerprint (`fp=`) is the same. With the default `appendfsync always`, the same cut loses
nothing: `hydo run "disk=cut | run C=12"` ends `OK` with `acked=12 all present`
(`7147e75676d15194`).

`hydo run` exits with status 0 whenever the run finishes, whatever its verdict. A script tells a
pass from a failure by `verdict.kind` in the run's `result.json` (in the folder under
`hydo-out/runs/` that the run prints): `ok`, `violation`, or `error`.

Network faults work the same way. `O=` is a fault in one direction of a service's traffic:
`O=redis@5-10:resp:loss,100%` drops every reply (`resp`) Redis sends from step 5 up to step 10,
that is during steps 5 to 9. The client times out on those five writes and does not count them
as acknowledged:

```sh
hydo run "disk=none | run O=redis@5-10:resp:loss,100%"
```

It ends `OK ... acked=15 all present`: the 15 writes the client saw acknowledged are all there.
Again, a second run gives the same fingerprint.

hydo can also vary thread timing. The host part of a schedule can delay the guest's timer
interrupts by a seeded number of VM exits. That changes where the guest switches threads.
This delays about half of the ticks during the workload by up to 500 exits:

```sh
hydo run "disk=none tj=7 tjpm=500 tjmax=500 | run"
```

The `RESULT` line gains `tjk=` (the first and last tick period touched) and `tjn=` (how many
ticks were delayed). The fingerprint differs from the untimed run, because the timing changed
the guest's state, and it is the same on every run of this schedule. `hydo explore --timing`
gives each generated schedule such timing with a chance of one in two.

### A program that busy-waits

Some programs wait in a tight loop on a memory value without any system call, so the loop makes
no VM exits. A loop that also counts its turns is a counter loop. A deterministic machine cannot
interrupt such a loop at a fixed point by itself, so hydo watches for it. It proves that the loop
repeats forever. Then it delivers the next timer interrupt at a point chosen
from the loop's own state, so the result does not depend on when hydo noticed. The second
example spins in every request (`/bin/spin_load`, and a loop that also counts, `/bin/spin_count`,
in every tenth one):

```sh
cp -R ~/.hydo/examples/ledger-spin ~/hydo-spin && cd ~/hydo-spin
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=none | run P=front@10+2"
```

The first ends `OK ... acked=30 each stored once`. On the way it prints that the first run
ended at a counter loop, so the result is from the run again in rewind mode. Rewind mode is
slower: it keeps checkpoints and replays to the loop, so that the loop's wait ends at a point hydo
can compute. hydo switches to it on its own. The second pauses the example's web server, the service
`front`, for two requests; the client's retry stores entry 10 twice, and the invariant catches it
(`entry 10 stored 2 times`). This example's server is deliberately not idempotent. Each command
gives the same fingerprint every time. A busy-wait run takes about eight seconds instead of two.

## 3. Explore

Let hydo generate schedules, run them, re-run every failure to check it reproduces, and shrink
each failure to a minimal schedule:

```sh
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

hydo runs as many virtual machines at once as the Mac's cores and memory allow; add
`--workers 2` to keep it to two. Each worker boots the guest once and starts every schedule from
a snapshot of it, kept in files. The report lists every failure with its class (the cause) and
says whether it reproduced. For each class of VIOLATION it gives the minimal schedule,
found by shrinking (removing faults while the failure stays), and the
`hydo run --image <image> "<schedule>"` command that replays it. A run that ended `ERROR` gets no
minimal schedule; `hydo repro hydo-out/explore` re-runs it.

In this example the failures are all expected kinds: the disk that loses acknowledged writes
(`lying-disk`), injected disk errors (`io-errors`), and the relaxed `appendfsync` settings
(`unsafe-knob`). With the default settings and an honest disk Redis loses nothing (the
`disk=cut | run C=12` run of section 2). The exploration finds 11 failures. The report's last
table lists the assertions of hydo's SDKs (`hydo guide sdks`); this example has none.
`NOT EXERCISED` means no run reached a property, `[not in catalog]` that no program declared it,
and `setup-complete seen in 0` that no program reported the end of its setup.

## 4. Reproduce a failure

Copy a minimal schedule from the report, the whole string, and run it. This one is the
report's `lying-disk` schedule:

```sh
hydo repro "disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20"
```

It ends with the same verdict and the same fingerprint every time. The console of the run is
saved under `hydo-out/runs/`, so you can read what the software printed. To run every failure of
the exploration again and compare each fingerprint with the recorded one:

```sh
hydo repro hydo-out/explore
```

It ends `hydo: 13 of 13 reproduced with the recorded fingerprint` (the 11 failures and the 2
minimal schedules).

## 5. Onboard your own project

hydo learns your project from one file, `hydo.yaml`: which programs to start, how to tell they
are ready, the workload, the invariants that must always hold, and which faults are allowed.
`~/.hydo/hydo/onboard/MANIFEST.md` describes every field, and `~/.hydo/examples/redis/hydo.yaml` is
a complete example.

The release includes an onboarding guide for AI coding tools of any vendor, as an Agent Skills
folder: a folder of instructions in the open format of https://agentskills.io. The guide reads
your repository and writes the project's `hydo/` folder. Start the folder, then give your tool
one instruction:

```sh
cd your-project
hydo init                    # hydo/ skeleton, .gitignore lines, a hydo section in AGENTS.md
# then tell your AI tool: "run `hydo guide skill` and follow it"
```

`hydo guide workflow` says what goes into each file of the folder, if you write it yourself.

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

List each file under `root.files:` with a path relative to `hydo.yaml`. `root.nix:` (programs
from a Nix store, the folder where the Nix package manager keeps its packages) is not supported
by the Mac build yet.

Then the same three commands as in section 2: `hydo build hydo/hydo.yaml` (or `hydo build hydo.yaml`
for a single file), `hydo run "disk=none | run"`, `hydo explore`. The image of a `hydo/` folder
goes to `.hydo/` instead of `hydo-out/`. `hydo build hydo/hydo.yaml --check` checks the folder
without building.

The release carries every guest file the arm64 build stages, so these work from it as well:
- `network: bridge`, `addr:` and `faults: [freeze]`. With them each service runs in its own
  network namespace, which is its own network stack and address. Faults then hit the links
  between services, and a freeze stops a service at the n-th packet it sends or receives. Such an image is built
  for the release's second guest kernel (`Image-explore-netns`). The build says so and records it
  in the image, and `hydo run` and `hydo explore` boot that kernel by themselves;
- `fakes:` and `proxies:`: fake outside services, and protocol faults (wrong, late, or missing
  answers of those services);
- the ready-made store readers for invariants and checks: `hydo-read` reads a database's
  contents as sorted lines, and `hydo-check` compares two readings.

## What the Mac version does not do yet

- A busy-wait loop is resolved only if it repeats exactly (the same registers and memory each
  time, or only counters that never feed back). Any other loop that never makes a system call
  marks its run invalid instead of giving a wrong result.
- One virtual CPU, and guest RAM up to 8 GiB (`guest.ram_mib`).

## Licenses and source code

hydo is licensed under the Apache License 2.0 (`LICENSE`). The release also contains this
third-party software:

- the two arm64 guest kernels, busybox, and mke2fs;
- the tc, ip, and iptables tools with their libraries;
- the Redis example's binaries;
- the GNU C library and the GCC runtime linked into `guest/hydo_fc`;
- the Rust standard library, and the Rust crates compiled into hydo's programs.

`THIRD-PARTY.md` lists each one with its version, its license, the files it is in, and where its
source is. `licenses/` holds the full text of every license.

The archive `hydo-mac-0.3.1-sources.tar.gz` accompanies this release; it is published next to
the tarball, with the same `SHA256SUMS`. It contains the corresponding source:

- the exact upstream source archives of every third-party component, including every GPL- and
  LGPL-licensed one, among them the Arm GNU Toolchain's source snapshot for the C library in
  `guest/hydo_fc`;
- hydo's kernel patches and configurations;
- the build recipes.

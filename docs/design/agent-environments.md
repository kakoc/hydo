# Deterministic, branchable environments for AI agents (a two-day spike, 2026-11-08)

Status: a measured demonstration and a small opt-in prototype on a branch; not a product
feature. The question came from the user (2026-11-08): can hydo serve as an environment for AI
agents (for training, evaluation and search) that is exactly reproducible and can be branched, so
that several actions can be tried from one state? This document answers it with numbers from the
build machine (an Intel Core i7-13700HX with 24 threads and 31 GB of RAM) on the KVM engine.

## The answer in short

Yes, for what an agent does inside one machine without a network. From one state of a workspace
(a Git repository of a Python project with its tests), hydo ran an agent's actions on 13
branches: the same action on two branches gave the same output and the same fingerprint of the
whole guest state, different actions differed only by what they did, and the same sequence of
actions gave the same final state on two branches, in two sessions and in three fresh boots.
Even the values a program draws from the operating system's randomness, the time and a UUID were
the same on every branch, and a wheel built on two branches had the same bytes (built twice
natively, its sha256 differs).

The costs, without the state fingerprint (below): a branch costs what any call costs, about
35 ms for a trivial command, and 1 MB of memory after a trivial command or about 46 MB after a
test run; running the tests takes 0.3 s against 0.14 s natively, building the wheel 0.95 s
against 0.50 s. With the state fingerprint that every call computes today, every call costs
about 0.8 s more, almost all of it hashing the 739 MB disk image byte by byte. What is missing
for real agent use is mainly the network (agents install packages and call services), an API
made for agents rather than for looking at failed runs, and more than one vCPU.

## What was built (opt-in, nothing recorded moves)

The primitive was already close: `hydo inspect` (Milestone 178, `docs/design/inspect.md`) replays
a recorded run to one of its stops, keeps the VM there as a snapshot, and runs commands from it,
each call from the same state. The spike added:

- **Named branches.** `hydo inspect exec <session> --branch NAME [--from OTHER] -- <command>`
  runs the command from the branch's state and keeps the state after it as the branch (the engine's
  snapshot `b:NAME`). A new branch starts from the session's stop, or from another branch with
  `--from`. A branch call writes the command's output to one fixed file name, so the same command
  from the same state leaves the same state on any branch (calls without `--branch` number their
  output files, so three plain `true` calls left three different fingerprints).
- **The state fingerprint in every answer.** `state_fp` is the engine's fingerprint at the end of
  the call: the console, every vCPU register, the machine and device state, the whole disk with
  its write cache, and all of guest RAM (the value a run's `RESULT` line carries). Equal values
  mean equal states. The results format gained `Inspect.branch`, `Inspect.state_fp` and
  `InspectBranch` (additions; the schema test passes).
- **More snapshots per worker.** The KVM and Mac engines keep 64 named snapshots instead of 8 (an
  unused slot is an empty entry). The host's S0 and S1 and the session's stop take three, so a
  session holds 61 branches (5 on hvx, whose limit was not changed); one more is refused with a
  message. A snapshot cannot be dropped yet.
- **`HYDO_KVM_NO_FP`** (developer): the engine computes no fingerprint (it reads as 0), to measure
  what fingerprints cost.
- **The example** `hydo/onboard/examples/agent-workspace/`: the image, an agent's session as six
  numbered workload steps (`agent-steps.sh`: run the tests, fix the bug, run the tests, commit,
  build the wheel, print what is left), and `branch-demo.sh`, which runs the branches below.

No recorded fingerprint moves: the switch is off by default, a snapshot slot is not part of any
state, and inspection only reads recorded runs. The agent-workspace run gave the same fingerprint
before and after the engine changes.

## The demonstration

**The image.** `hydo/onboard/examples/agent-workspace/hydo.yaml`: Python 3.11 with pytest 8.1.1,
pip, setuptools and wheel, Git 2.44 and make (one Nix store path, 279 MB with its closure), and a
small project, `stockbook` (an inventory ledger with first-in, first-out costing, 20 tests, one
planted bug: a partly shipped lot keeps the shipped quantity instead of the rest). Provisioning
copies the project to the data disk and makes it a Git repository with one commit; that state is
in the image's snapshot S1. No network device. The image is 739 MB (705 MB on disk), 1024 MiB of
guest RAM, one vCPU.

**The session as a run.** `hydo run --backend kvm --image <image> "disk=none | run"` boots,
provisions and runs the six steps; the check (the tests pass and the wheel exists) holds.

**The branches.** `hydo inspect start <run dir> --at run:243` holds the first stop of the run (the
provisioned workspace, before step 0). `branch-demo.sh` then ran, each answer as JSON:

| call | branch | what | result | `state_fp` |
|---|---|---|---|---|
| 4 | tests-a | run the tests | 9 failed, 11 passed | d90bb157d7e24912 |
| 5 | tests-b | run the tests | 9 failed, 11 passed | d90bb157d7e24912 |
| 6 | fix | the right fix (`lot.qty -= take`) | 1 file changed | b0a491ae87e9e890 |
| 7 | fix | run the tests | 20 passed | 130ab7fbc81b9d30 |
| 8 | badfix | a wrong fix (`lot.qty = 0`) | 1 file changed | 7b5eed9bbeada7f2 |
| 9 | badfix | run the tests | 5 failed, 15 passed | 6c861647cb98767d |
| 10 | wheel | build the wheel | wheel sha256 9ff9b269... | a0d6b1c9bfee66fd |
| 11 | commit | an empty commit | `bdbf734 an empty commit` | cda0e922525b172d |
| 12 | entropy | print `random.random()`, a UUID, 8 bytes of `os.urandom`, `time.time()` | 0.9586911503440506 5400d217-... 8f83604a82192741 1767225602.54647 | 2035caf7521eb220 |
| 13 | network | `pip install requests` | no matching distribution (no network) | 901d5ba28b644a28 |
| 14 | fix2 (from fix) | commit the fix | `f677508 ledger: keep the rest...` | 01a39fc4c58a4964 |
| 15 | fix2 | build the wheel | wheel sha256 4d43e42a... | 814244faa1d0590d |
| 16 | again | the right fix | 1 file changed | b0a491ae87e9e890 |
| 17 | again | run the tests | 20 passed | 130ab7fbc81b9d30 |
| 18 | entropy-b | as call 12 | the same four values | 2035caf7521eb220 |
| 19 | wheel-b | build the wheel | wheel sha256 9ff9b269... | a0d6b1c9bfee66fd |

- **(a) The same action on two branches is bit-identical**: calls 4 and 5, 12 and 18, 10 and 19
  give the same output and the same fingerprint of the whole guest state (RAM, disk, registers,
  devices). The randomness the guest's kernel hands out is a function of the run, and so is its
  clock: `date` printed `Thu Jan 1 00:00:02 UTC 2026` on both branches.
- **(b) Different actions differ by their effect**: the right and the wrong fix give different
  test results and states; a commit on the fixed branch gives another wheel than on the stop.
- **(c) The same sequence gives the same final state**: the branch `again` repeated the fix
  branch's two actions and reached its two fingerprints. The whole session as a run, from a fresh
  boot, gave `fp=828dbdc53a6a5d8c` four times (three times before the last engine change, once
  after). The whole demonstration run a second time in a new session (with fingerprints off,
  `HYDO_KVM_NO_FP=1`) printed the same outputs, the same random values and the same wheel bytes.

Natively, on the same machine with the same tools, two builds of the wheel gave two sha256 values
(`abb880c3...`, `9152df0a...`: the files' times are in the archive), and every run of the entropy
line printed other values.

## Measurements

The KVM engine on the build machine; each `hydo inspect` cost is the session's own `wall_ms` (the
client's socket round trip adds 2 ms). Another agent's VM ran on the machine at the same time.
Native = the same command on the build machine itself, median of 5 to 9 runs.

| | in a branch, with `state_fp` | in a branch, without (`HYDO_KVM_NO_FP=1`) | native |
|---|---|---|---|
| `true` (one command, its output read back) | 830-860 ms, 37 stops | 27-46 ms | 4 ms |
| a new branch running `true` | 832-860 ms | 27-39 ms | — |
| an edit (`sed`, `git diff --stat`) | 850-860 ms, 43 stops | 39-56 ms | — |
| `git commit` | 830-880 ms, 41 stops | 59-70 ms | 6 ms |
| the tests (`python -m pytest -q`, 20 tests) | 1,100-1,160 ms, 387 stops | 290-376 ms | 128-146 ms |
| build the wheel (`pip wheel . --no-build-isolation`) | 1,690-1,800 ms, 993-1,186 stops | 898-985 ms | 502 ms |
| `pip install` with no network (fails at once) | 1,371 ms, 801 stops | 511 ms | — |
| hold the stop (`hydo inspect start`: replay of the boot and the run to stop 243) | 2,413 ms | 701 ms | — |
| the whole session from a fresh boot (`hydo run`: boot, provisioning, six steps, check) | 3.6-3.8 s | 2.1-2.2 s | 0.80 s (the six steps only) |

**The state fingerprint costs about 0.8 s per call.** It hashes the whole disk image byte by byte
(FNV-1a, 739 MB in 641 ms on this machine) and all of guest RAM (1 GiB in 174 ms), measured with
the engine's own hash functions on the image. So with fingerprints every call costs at least
0.8 s, and without them a call costs tens of milliseconds plus about twice the command's native
time. The definition of the fingerprint cannot change without moving every recorded KVM value; an
agent environment would use a second, incremental state hash (hashes kept per disk block and RAM
page, updated only for what a call wrote, combined as a tree), which costs in proportion to what
the call changed.

**Memory per branch.** The session's own memory (`/proc/<pid>/smaps_rollup`, anonymous pages)
grew by about 1 MB per branch after `true` (10 branches: 2.8 MB to 12.5 MB) and by 46 MB per
branch after a test run (61 branches: 94 MB to 2,868 MB, linear). A wheel build leaves 150 to
210 MB. A branch keeps the RAM pages and disk blocks its calls wrote: Python's heap and the page
cache of the files it read and wrote count, the rest is shared with the snapshot S1 (a memory file
of 39-90 MB) and the image file (page cache shared by every process; with fingerprints the whole
729 MB of it is read, without them 56 MB).

**How many branches fit.** One session holds 61 branches (the engine's 64 snapshots); more
sessions hold more, each its own process. With about 24 GB free on the build machine, 46 MB per
branch is about 500 test-run branches in memory, or about 150 branches that built a wheel. The
CPU is not the limit for branches that wait for an agent; a branch only costs CPU while a command
runs, and one session runs one command at a time (one VM).

**Where the time goes, without the fingerprint.**

- **The lock-step control protocol.** The host takes every command and every output byte to and
  from the guest at engine stops; a stop is a round trip between the engine and the host
  controller, about 0.4 ms here, and a test run takes 387 stops (the agent's output comes back in
  3,000-byte reads). This is the 30-40 ms of a trivial call and much of the doubling of a test run.
  A channel made for agents (a shared-memory port such as hvx's host-call port, M138, for the KVM
  engine; fewer stops per call) would cut it.
- **One vCPU.** The guest runs one vCPU, so a test suite that runs in parallel natively runs
  serially here. `pytest -n`, `make -j` or a compiler's own threads get no speed-up.
- **The exit-count clock.** Guest time advances only at VM exits, so pure computation takes no
  guest time: pytest reported `20 passed in 0.00s`, and the guest's clock had moved 2.5 s since
  the boot. A command's time limit in guest time therefore does not limit computation (the engine
  ends a run that computes without exits for 10 host seconds as a SPIN). On the other hand, a
  result cannot depend on how fast or loaded the host is.
- **The restore.** Restoring a branch maps the RAM base again, writes the branch's own pages back
  and creates a fresh VM. It was not timed apart: a trivial call on an existing branch cost
  19-43 ms, the same as on the stop.

## What is missing for real agent use

- **The network.** Agents install packages, clone repositories and call services; the image has
  no network at all (`pip install` fails at once). Two ways, both within hydo's model:
  - *A local mirror inside the image.* A package index or a Git server as a service of the image,
    filled with what the task needs, is exact and needs nothing new; it fits evaluation and
    training tasks that are prepared ahead.
  - *Record and replay at the edge.* hydo already serves fake outside services and fault proxies
    in the guest (`fakes:`, `proxies:`, M89): one program that answers HTTP and gRPC and records
    every request. A recorder would forward a request to the real service the first time
    (outside the deterministic part, so that run is the recording), store the exchange keyed by
    the request and its position, and replay it to every later run and every branch. What it
    takes: a host-side forwarder that the guest reaches through the engine (today the guest's
    network is internal), TLS (the guest must trust a local certificate authority, or the
    recording is of the plaintext behind a terminating proxy), a rule for what a request that
    was never recorded gets (an error, or a new recording that makes the branch nondeterministic
    from that point and says so), and recordings stored with the image's state. Until then,
    determinism holds for what happens inside the machine.
- **An interface made for agents.** `hydo inspect` is built to look at a recorded run: a session
  starts from a run directory, the commands run through a shell, and the output comes back through
  files in bounded pieces. An agent environment wants: create an environment from an image (no run
  first), fork, exec, read and write files, drop a branch, list branches with their parents and
  fingerprints, and an incremental state hash, as one small JSON or MCP interface, with branches
  that can be dropped (the engines have no command for it yet).
- **Several vCPUs.** The KVM and Mac engines run one vCPU; hvx runs up to four but its several-vCPU
  mode is not available under lock-step control (open item 30 c). Build-heavy tasks run several
  times slower than on a workstation.
- **Start-up time.** Booting this image to its provisioned state takes about 0.7 s of the 2.1 s run
  (without fingerprints); a session's start is a replay of the recorded boot. A pool would boot
  each image once and keep S1, so a new environment costs a restore (milliseconds); today S1 lives
  in one process.
- **Scale across machines.** A snapshot lives in one engine process on one machine. Moving a branch
  to another machine needs its pages and disk blocks serialized and the same image, kernel and
  engine there; fingerprints compare only on the same kind of host CPU (the KVM engine's rule), so
  a fleet would be of one CPU model, or compare outcomes rather than fingerprints across models.
- **Robustness.** A refused snapshot in the middle of a call left the session unusable (the
  engine stayed in the conversation); the session now refuses a branch beyond the engine's slots
  before asking, but any other engine refusal in a call still ends the session.

## What was not done

- The Mac engine was not measured: the workspace's tools would need an aarch64 Nix closure, and no
  aarch64 Linux machine with Nix is available; the branch code is the same for both engines.
- `cargo test` was not measured; the Python project stands for a test suite and a build.
- No agent (a language model) drove the branches; the actions are a fixed script.

## How to run it

On an x86_64 Linux machine with `/dev/kvm` and Nix, from `hydo/onboard/examples/agent-workspace/`:

```sh
nix-build -E 'with import <nixpkgs> {}; buildEnv { name = "agentws-tools"; paths = [
  (python3.withPackages (p: [ p.pytest p.pip p.setuptools p.wheel ])) gitMinimal gnumake ]; }'
hydo build hydo.yaml        # the store path in hydo.yaml must be the one nix-build printed
hydo run --backend kvm --image hydo-out/explore-ob-agent-workspace-disk.img --out run1 "disk=none | run"
HYDO_BACKEND=kvm HYDO=hydo RUN=run1 OUT=demo sh branch-demo.sh
```

## Files

- `hydo/crates/hydo-cli/src/inspect.rs` (`Branch`, `--branch`, `--from`), `inspect/session.rs`
  (`branch_call`, `max_branches`), `inspect/drive.rs` (`Machine::call_from`, `result_fp`).
- `hydo/crates/hydo-results/src/v1.rs` (`InspectBranch`, `Inspect.branch`, `Inspect.state_fp`),
  `hydo/schema/results-v1.json`.
- `hydo/crates/hydo-kvm/src/serve.rs`, `hydo/crates/hydo-hvm/src/serve.rs` (64 snapshots;
  `HYDO_KVM_NO_FP`), `hydo/crates/hydo-config` (the variable).
- `hydo/onboard/examples/agent-workspace/` (the image, the session, the demonstration).
- `hydo/guide/inspect.md` section 4 (branches, for agents that read `hydo guide inspect`).

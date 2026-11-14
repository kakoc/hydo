# Onboard your system

This page shows how to put your own system under hydo. It ends with a worked example: a small
message queue, from an empty folder to an exploration that finds a lost message.

## What hydo needs from you

hydo runs your system in a virtual machine and breaks it there. It needs three parts, and they
live in a `hydo/` folder in your repository:

1. **How to start your system.** Each long-running process is a service in `hydo/hydo.yaml`,
   with its program, its command line, and a readiness check.
2. **A client that uses it.** Each command in `hydo/workload/` does one operation, for example
   one write or one sent message. The command records what the system confirmed.
3. **The rules that must hold.** A check says what must be true after every run, for example that
   every confirmed write is still there. A check is a command in `hydo/checks/`, or a
   ready-made check in `hydo/hydo.yaml`, such as `type: messages` for a queue.

hydo then composes the client's commands into runs, adds faults (process kills, pauses, power
cuts, clock steps), and runs the checks at the end of each run.

## Start with one command

Run this in your repository:

```sh
hydo onboard
```

`hydo onboard` says what it found, starts `hydo/`, and prints the next step. It never changes a
`hydo/` folder that already exists. Its help text lists its options. The next step
depends on what your repository has.

**A Docker Compose file.** `hydo onboard` imports it with `hydo compose`. The import writes one
service per Compose service, with its image, ports, volumes, readiness check, and restart policy.
It also writes `hydo/fetch-images.sh`, which copies the images in once. The client and the rules
are left for you or your AI tool.

**No Compose file.** `hydo onboard` runs `hydo init`, which writes a `hydo/hydo.yaml` with
`TODO` marks and two placeholder files. The placeholders make every run fail until you replace
them, so an empty test never passes by accident.

**An AI coding tool.** `hydo onboard` installs hydo's onboarding skill for each tool it finds: a
`.claude/`, `.codex/`, `.cursor/` or `.gemini/` folder, or the tool's command on your `PATH`.
The skill is a set of instructions that teaches the tool to write the three parts with you. For
Claude Code the next step reads:

```text
Next: open Claude Code here and run /hydo:onboard
```

You can also add the skill to Claude Code yourself. Type these three lines in Claude Code:

```text
/plugin marketplace add kakoc/hydo
/plugin install hydo@hydo
/hydo:onboard
```

For Codex, Cursor, GitHub Copilot, or Gemini, run `hydo skill install --agent codex` (or
`cursor`, `copilot`, `gemini`) and ask the tool to use the onboard skill.

Without an AI tool, `hydo onboard` prints the three parts to fill in. This is its output in a Go
repository with a Dockerfile and no Compose file:

```text
hydo: no AI coding tool found (no .claude/, .codex/, .cursor/ or .gemini/ folder, and no claude, codex, cursor-agent, gemini or copilot command)

Next: fill in three things in hydo/ (`hydo guide workflow` walks through them):
  1. how to start your system: one service per process in hydo/hydo.yaml (hydo guide services)
  2. a client that uses it: commands in hydo/workload/ that record what the system confirmed
     (hydo guide composer)
  3. the rules that must hold: checks in hydo/checks/, or a `type: messages` check in
     hydo/hydo.yaml for a queue or a log (hydo guide checks, hydo guide messages)
Check the folder after each change: hydo build hydo/hydo.yaml --check
```

## A good first result

Build the image, then run one schedule without faults twice. A schedule is one line that names
the faults of a run; `disk=none | run` names none:

```sh
hydo build hydo/hydo.yaml
hydo run "disk=none | run"
hydo run "disk=none | run"
```

A good first result has two properties:

- Both runs print the same fingerprint, the `fp=` value. The fingerprint is a hash of the whole
  machine's final state, so the same value means that the run repeats exactly.
- The verdict is `OK`, and every check held with something to judge.

When the first run fails, the cause is usually your own folder, not your system:

- A client command that fails or records nothing. Read its log, `/tmp/hydo/compose/log.<entry>`,
  with `hydo inspect` (`hydo guide inspect`).
- A check that is too strict, or that reads its data before the system wrote it.
- A service that never becomes ready. The `hydo: diagnosis:` lines name it.

Fix the folder until the run without faults passes. Only then do faults mean something.

## A worked example: a message queue

The example is in [hydo/onboard/examples/messages-tutorial/](../hydo/onboard/examples/messages-tutorial/).
It was run on the Mac engine. Building the queue needs Go 1.22 or later; `build.sh` builds it for
an arm64 guest on a Mac and for an x86_64 guest on Linux.

### The system

`tinyq` ([tinyq/main.go](../hydo/onboard/examples/messages-tutorial/tinyq/main.go), 316
lines of Go) is a queue with one channel. It answers one line per request over TCP on port 7700:

- `SEND <id> <key>` stores a message and answers with its position in the log. A resent id is
  stored once.
- `RECV` hands out the oldest message that nobody acknowledged.
- `ACK <position>` acknowledges a message, so it never comes back.

The queue writes every message and every acknowledgement to a log file and syncs the file to
disk before it answers. The same program is also the client, so the example needs one binary.
Build it for your machine's guest first:

```sh
cd hydo/onboard/examples/messages-tutorial
sh build.sh
```

### The three parts

**The service.** [hydo/hydo.yaml](../hydo/onboard/examples/messages-tutorial/hydo/hydo.yaml)
copies the binary into the image and starts it with its data on the data disk:

```yaml
root:
  files:
    - {src: bin/tinyq, dst: /bin/tinyq, mode: "0755"}

data:
  size_mib: 64
  dirs: [tinyq]

services:
  - name: tinyq
    cmd: /bin/tinyq serve /data/tinyq 7700
    ready: /bin/tinyq ping 7700
    port: 7700
```

**The client.** Two commands in `hydo/workload/` act as the queue's users.
[parallel_driver_produce](../hydo/onboard/examples/messages-tutorial/hydo/workload/parallel_driver_produce)
sends one message, and several producers run at once. One producer in four sends its message a
second time, as a client does that retries after a lost answer.
[serial_driver_consume](../hydo/onboard/examples/messages-tutorial/hydo/workload/serial_driver_consume)
takes the oldest message and acknowledges it. A command's name says how it runs:
`parallel_driver_` commands run several at once, and a `serial_driver_` command runs alone.

The client records each request and each answer as a history record on the guest console. A
history is the list of what the client asked and what the system confirmed. These are the
records of one send:

```text
HYDO-HIST queue 7 invoke 503901333 send orders m7;k=g1
HYDO-HIST queue 7 ok 506915291 send orders m7;k=g1;o=8
```

`queue` is the history, `7` the client process, the number after `invoke` or `ok` the guest time in
nanoseconds, `m7` the message, `k=g1` its ordering key, and `o=8` the position the queue gave
it. These two lines come from the first run below. `hydo guide messages` lists every record.

**The rules.** A `type: messages` check states what the queue promises. hydo knows no queue and
no protocol: it checks exactly these claims against the history.

```yaml
checks:
  - id: queue
    type: messages
    history: queue
    final: /bin/tinyq drain 100000
    acknowledgement: explicit
    claims:
      delivery: at-least-once
      duplicates: {by: message, window_ms: 600000}
      order: {kind: fifo, scope: key}
      after_ack: no-redelivery
```

Each claim is a rule:

- `delivery: at-least-once`: no message is lost after the queue accepted it.
- `duplicates`: a message sent twice within 10 minutes is stored once.
- `order`: the messages of one key are delivered in the order they were sent.
- `after_ack: no-redelivery`: an acknowledged message never comes back.

`final:` is the drain, a command that hydo runs after the faults end. It takes every message
that is left, so a message that never arrives counts as lost.

### The first run

```sh
hydo build hydo/hydo.yaml
hydo run "disk=none | run"
hydo run "disk=none | run"
```

On the Mac engine both runs printed `fp=be992f7e466b6db5` and the verdict `OK`. `hydo report`
with the run directory shows each claim and how many cases it judged:

```text
messages check queue (history queue):
  no-phantom    held      judged 24, undecided 0
  no-loss       held      judged 24, undecided 0
  duplicates    held      judged 8, undecided 0
  after-ack     held      judged 24, undecided 0
  order         held      judged 21, undecided 0
  sends: 28 ok, 0 failed, 0 unknown (0 recovered); lost 0; unexpected 0, aborted 0
```

`no-phantom` is the claim every messages check makes: nothing is delivered that was never sent.

### The first exploration

The example has a setting that a schedule can change, the token `S`: `S=never` makes tinyq answer
before it syncs its log. The default, `S=always`, syncs first. Explore 20 seeds:

```sh
hydo explore --seeds 1-20 --workers 1
hydo report
```

The exploration ran 20 schedules on the Mac engine and found one failure, which reproduced. It
shrank the failure to one producer and a power cut, and named its class `unsafe-knob`:

```text
- minimal schedule: `disk=cut dseed=362259904816818c | run C=10 S=never W=:p1,r50,cparallel_driver_produce,1.1.0.1802048978`
- verdict: `HYDO-ORACLE: VIOLATION [queue.no-loss] m1 was acknowledged to process 1 at 0.116 s and never delivered in channel orders, which was drained`
```

### Reading the result

The class `unsafe-knob` means that the failure needs the unsafe value of a setting. With `S=never`,
tinyq confirmed message `m1` before the message reached the disk. `C=10` cut the power before
step 10, and the disk kept only what was synced, so `m1` was gone after the restart. The same
power cut with the default `S=always` passes. Run both by hand:

```sh
hydo run "disk=cut | run C=10 S=never"
hydo run "disk=cut | run C=10"
```

The first prints `VIOLATION [queue.no-loss]` with the message it lost; the second prints `OK`.
In your own system the same report points at a real bug: a claim names the rule that broke, and
the minimal schedule replays it. `hydo save hydo-out/explore` keeps the schedule as a regression
test in `hydo/regressions/`.

## Where to read more

- `hydo guide workflow`: every step of onboarding with the commands of your hydo.
- `hydo guide messages`: the messages check, its claims, and its records.
- [YOUR-PROJECT.md](YOUR-PROJECT.md): how to write the `hydo/` folder by hand.

# hydo-agent-proto and hydo-agent — handoff (milestone M117 done; what wave 2 needs)

Steps A1–A3 of `docs/design/guest-control.md` are done on branch `guest-agent-2`
(docs/HISTORY.md "Milestone 117"). Nothing is wired into `hydo-build`, the engines or the
explorer, and no product behaviour changed.

## What exists

- **A1, `hydo/crates/hydo-agent-proto`** (workspace member, no dependencies; README.md is the
  tour): canonical JSON (`json.rs`), the console framing (`frame.rs`), the messages of version 1
  (`msg.rs`: 17 commands with their replies, 9 events, error codes), the envelopes (`line.rs`:
  strict for the agent, lenient for the host), the host session with the version rule
  (`session.rs`), the generated schema (`schema.rs` → `hydo/schema/agent-v1.json`, with the
  additions-only check against `hydo/schema/released/agent-v1.json`), and the transcript format
  and checker (`transcript.rs`). Example transcripts: `hydo/schema/examples/agent/v1/` (INDEX).
- **A2, `hydo/crates/hydo-agent`** (own workspace and lock file): the agent; `build.sh` (static,
  byte-reproducible); `bare-metal.sh` (runs a command on the bare-metal machine from the Mac); native tests in
  `tests/native.rs` (record missing transcripts with `HYDO_BLESS=1`, replay every native one
  twice, the PID-1 check with `-- --ignored`, which needs `sudo -n`).
- **A3**: the packet trigger in the agent (`nfq.rs`, `tree.rs`; the `freeze`/`unfreeze`
  commands), and hydo_fc's command line (`fc.rs`; the binary installed as `/bin/hydo_fc`), proven
  on the bare-metal machine against the C helper (results `/var/tmp/m117-a3`, script `run.sh` there).

## What wave 2 needs to know

1. **Sending.** `Session::command` gives the bytes for the engine's `SEND`; after a restore send
   `Session::sync` first (CAN + sync). A reply or refusal is the proof that the bytes left the
   UART queue: that is when the engine's `ACK <id>` may be sent (the snapshot rule of M118).
2. **Receiving.** Feed every console line (the engine's `EVENT` text, unescaped) to
   `Session::console_line`. `None` is kernel or service text. An error is an error of the run
   (a corrupt frame, a lost or unpaired line).
3. **The host must not send** ops or fields the hello does not list; `Session::command` refuses
   them. New optional command fields come with a feature name (`msg::FEATURES`).
4. **Images.** The agent reads its console from `HYDO_AGENT_TTY` (environment), else
   `hydo.tty=` on the kernel command line, else `/dev/ttyS0`; it sets raw mode without echo. The
   kernel should run with `quiet loglevel=1` (design). It mounts /dev (if /dev/null is missing),
   /proc, /sys, /tmp and runs /bin/hvx_seedrng; nothing else (no `lo` up, no data disk): those
   are commands.
5. **Environment of children** is exactly the command's `env` (nothing inherited); give PATH.
6. **Times.** Deadlines (`kill_after_ms`, a probe's `every_ms`/`deadline_ms`, a freeze's
   `dur_ms`) are CLOCK_MONOTONIC ms in the guest; the agent never reports times except in
   `clock_get` and the freeze trace file.
7. **Booting an agent image (done in M121):** `hydo build --control agent` stages the agent as
   the first process and writes `/etc/hydo/control` = `agent 1`; `hydo_engine::image::control`
   reads it. `Engine::start_controlled` (`--control boot serve`) runs lock-step control from the
   VM's first instruction (stops `phase=boot`); the host takes `SNAP S0` (M121's check: at the
   hello's stop) and `SNAP S1` (after provisioning) where it decides, ENDs the boot, and the
   worker's CTL runs restore that S1 and type nothing (the guest part is `run`).
   `Engine::one_controlled` (`--control boot one`) is the fresh-boot counterpart: the host's
   `SNAP S1` there arms the schedule's host part and starts the workload phase at the same stop.
   Rule used by the check (`hydo-engine/examples/agent_boot_check.rs`): send `sync` right after
   every SNAP of a snapshot point, in the straight run too, so a continuation and the straight
   run type the same bytes at the same stop. docs/HISTORY.md "Milestone 121".

## Left open

- No `froze`/`resumed`/`warn` event and no successful `clock_set` in an example transcript: the
  first needs a netfilter queue (root; the bare-metal machine's unprivileged user namespaces are blocked), the
  second a guest. A transcript from an agent image driven by the controller should add them.
- The controller (B1/B2). (The boot of an agent image in both engines and `hydo-build --control
  agent` (B3) are M121; a successful `clock_set` in a guest is in M121's check transcripts.)

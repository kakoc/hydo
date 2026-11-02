# hydo-agent-proto

The protocol between hydo's host and `hydo-agent`, the small Rust program that runs as the
guest's first process and carries out the host's commands (docs/design/guest-control.md; built
in milestone 117). The specification is the types in `src/msg.rs`, the schema generated from
them (`hydo/schema/agent-v1.json`) and the example transcripts recorded from real runs
(`hydo/schema/examples/agent/v1/`). The crate has no dependencies, so the agent compiles it into
itself and both sides read and write lines with the same code.

## The wire

- Host to agent: one canonical JSON object and `\n`, typed into the serial input:
  `{"h":"redis","id":7,"op":"signal","sig":"KILL","tree":true,"v":1}`. Keys are sorted, numbers
  are integers, `id` grows with every command.
- Agent to host: one framed line on the console per reply, refusal or event:
  `HYDO-AG <len> <crc32> <json>`. Every agent line has `seq` (0 is the hello), so a lost line is
  seen. A reply carries `re` (the command's id), `op` and `ok: true`; a refusal `ok: false`,
  `err` (a code) and `msg`; an event `ev`.
- The agent's first line is the hello: protocol version, agent version, its ops and features.
- After a restore the host sends a CAN byte (0x18) and a `sync` with a nonce: the agent drops a
  partly read line (event `dropped`) and echoes the nonce.

## Commands (version 1)

`sync`, `spawn` (a service under a handle), `signal` (to a handle or its whole process tree,
optionally waiting until every thread stopped), `run` (to its end, output captured, `done`
event), `probe` (repeat until exit 0 or a guest-time deadline, `probe` event), `write`, `read`,
`mkdir`, `mount`, `syncfs`, `clock_set`, `clock_get`, `qlen` (packets in a qdisc, the netem
drain), `tail`/`untail` (new lines of a file as `line` events), `freeze`/`unfreeze` (the packet
trigger of `F=`: `froze` and `resumed` events; since milestone 131 `freeze` takes an optional
`netns`, the network namespace whose netfilter queue it binds, behind the feature
`freeze-netns`). Events: `hello`, `exit`, `done`, `probe`, `line`,
`froze`, `resumed`, `dropped`, `warn`. The agent decides nothing: the only timeouts are the
deadlines a command gives.

## The rules

- The agent refuses an unknown field, op or version and a repeated id, with a refusal, and keeps
  serving. The host ignores unknown fields of agent lines and reports an unknown event as such.
- Additions only within version 1: new ops, events, optional fields and names. A new optional
  command field also gets a feature name in the hello (`msg::FEATURES`), so a host never sends a
  field an older agent of the same version would refuse: `Command::features` lists what a command
  needs, and `Session::command` refuses one whose feature the hello lacks with a message that
  names the feature and says to rebuild the image ("this image's agent (hydo-agent 0.1.0) does
  not offer the feature freeze-netns (...); rebuild the image with this hydo (...), whose agent
  does"). Features so far: `freeze` (M117), `freeze-netns` (M131, hydo-agent 0.2.0), `hostcall` (M138, hydo-agent 0.3.0: the agent can send its lines and read the host's on hvx's host-call port; the engine offers the port and the agent takes it on its own, so no command field depends on it). `tests/schema.rs` fails when the
  generated schema differs from the committed one (`HYDO_BLESS=1` rewrites it) or is not an
  additive change of `hydo/schema/released/agent-v1.json` (a release copies it there with
  `HYDO_RELEASE_SCHEMA=1`). A breaking change is version 2, read beside version 1.
- The host reads every released version (`msg::HOST_PROTOCOLS`) and refuses a newer agent with a
  message naming both: "this image's agent (hydo-agent 0.9.0) speaks agent protocol 2; this hydo
  (0.1.0) knows agent protocol 1..1; upgrade hydo".

## Using it from the host (wave 2)

```rust
use hydo_agent_proto::msg::{Command, Signal, Sig};
use hydo_agent_proto::{Incoming, Session};

// the engine reports console lines (lock-step EVENT lines, unescaped)
let mut s = Session::from_hello(hello_json)?;          // the version rule
let o = s.sync(nonce)?;                                // first after a restore: CAN + sync
// o.bytes go into the engine's SEND; o.id is the command's id
let o = s.command(Command::Signal(Signal { h: "redis".into(), sig: Sig::Kill, tree: true, wait_stopped: None }))?;
for line in console_lines {
    match s.console_line(line) {
        None => {}                                     // kernel or service text
        Some(Ok(Incoming::Reply { id, body })) => {}   // command id was carried out (ACK it to the engine)
        Some(Ok(Incoming::Refused { id, error })) => {}
        Some(Ok(Incoming::Event(ev))) => {}            // exit, done, probe, line, froze, ...
        Some(Ok(Incoming::UnknownEvent { .. })) => {}  // a later addition
        Some(Err(e)) => {}                             // corrupt, lost or unpaired line: an error of the run
    }
}
```

`Session` holds no clock and makes no decision; `Session::position` / `resume_at` carry the
command and line numbers across a snapshot. `transcript` reads and checks transcript files.

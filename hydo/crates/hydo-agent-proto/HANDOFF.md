# hydo-agent-proto and hydo-agent — handoff (work in progress, milestone M117)

This branch (`guest-agent`) starts steps A1–A3 of `docs/design/guest-control.md` (the design is
the authority; read its sections "What stays in the guest", "The channel", "The protocol",
"Determinism, replay, snapshots", "How each current feature maps" and "Implementation plan").
Only a start of A1 exists. Nothing here is compiled yet: there is no `Cargo.toml` and no `lib.rs`,
and the crate is not a workspace member.

## What exists (written, not compiled or tested)

- `src/json.rs` — the canonical JSON reader and writer, without dependencies, so the guest agent
  and the host share one implementation: integers only, keys written sorted, duplicate keys and
  nesting deeper than 32 refused; with unit tests.
- `src/frame.rs` — the console framing `HYDO-AG <len> <crc32> <json>`, a CRC-32, and a reader that
  splits the console into agent lines, corrupt lines and other text; a CAN byte (0x18) discards a
  partly read input line, for resynchronisation; with unit tests.
- `src/field.rs` — macros meant to generate each message's encoder, decoder and JSON Schema from
  one field list, so the codec and the schema cannot drift apart.

## What is left

**A1 — the protocol crate `hydo-agent-proto`:**
1. `Cargo.toml` and `lib.rs`; add the crate to the workspace.
2. The message types (`src/msg.rs`): the hello (protocol version and supported commands), every
   command and reply the design lists, and the events the agent reports.
3. The host-side session with the version-skew rule: the host keeps every old protocol version
   and refuses a newer one with a message naming both versions.
4. The generated schema `hydo/schema/agent-v1.json`, with a test that fails if it differs.
5. The additions-only check against `hydo/schema/released/agent-v1.json` (`hydo_results::compat::breaking`
   can be used as a dev-dependency).
6. Permanent example transcripts in `hydo/schema/examples/agent/v1/` that must parse forever.
7. A short README.

**A2 — the agent `hydo-agent`:** one static Rust program (musl, x86_64 and aarch64, std + libc)
that runs as PID 1 and executes the design's primitives (start a process, signal a process tree,
probe until success or a guest-time deadline, files, mount the data disk, set the clock, run the
image's pinned `tc`/`ip`, stream a log file); no threads, timers or randomness of its own; idle in
a blocking `read`. Plan: its own Cargo workspace like `hydo/crates/hydo-read`'s (depending only on
`libc` and the protocol crate), static musl builds with M113's recipe (`hydo/grpcgen/rustc-stable`
and `hydo-elf-comment`, so the bytes do not depend on the build host). Proof: native tests on Linux
with the agent as a normal process against a pipe in a scratch root; two runs over one transcript
give byte-identical output; two builds give the same bytes.

**A3 — the packet trigger** (port of `hydo/onboard/hydo_fc.c`, the freeze at a packet `F=`).
Proof plan: put the new program into copies of `explore-ob-redis-fc-disk.img` on the bare-metal machine with `debugfs`, and compare their
FROZE lines with `hydo_fc`'s.

**Docs:** HISTORY "Milestone 117", AGENTS.md lines, a `docs/BUILD-PINS.md` entry.

## How it fits with the other steps

A4 (hvx lock-step control in `hv/hvx/hvx_explore.c`) and A5 (hvm lock-step in
`hv/hvm/explore/serve.c`) treat the agent's bytes as opaque; they are on their own branches. Wave 2
(the host-side controller and `hydo build --control agent`) needs A1's types and A2's binary.

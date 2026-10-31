# hydo-world in Rust — handoff (work in progress, milestone M111)

This branch (`port-world`) ports `hydo-world` from Go (`hydo/onboard/world/`, about 2,300 lines plus
`world_test.go`) to Rust. It is unfinished: the crate exists and most modules are written, but
nothing is compiled into the binary yet and nothing has been run. The Go version is unchanged and
is still what `hydo-build` stages into images.

## The original plan

`hydo-world` is the in-guest program that:

- serves `fakes:` — fake HTTP and cleartext-HTTP/2 gRPC services with routes, templated answers,
  idempotency keys and webhooks back to the system;
- runs `proxies:` — `tcp`, `http` and `grpc` fault proxies in front of real services;
- applies `H=` protocol faults (`status=`, `retry-after=`, `delay=`, `drop`, `reset`; webhooks
  `dup`, `reorder`, `late=`, `lost`; `p=`, `effect=`; on gRPC proxies also `rpc=`, `call=`,
  `cut=`, `hold=` with `dir=`, `reset=conn`);
- records every request in `/tmp/hydo/world.log`, prints the summary before the verdict, carries
  the world across a power cut (at most 3000 bytes on the acknowledged-ids line), and offers
  `hydo-world check …` and `hydo-world event …`.

References: `docs/HISTORY.md` M89, M90, M95, M96; `hydo/onboard/MANIFEST.md` ("Fake outside
services and protocol faults"); the Go source and `world_test.go`; how `hydo/crates/hydo-build`
stages it (`HYDO_WORLDBIN`, and the arm64 variant `HYDO_WORLDBIN_ARM64`).

The steps were:

1. A Rust crate `hydo/crates/hydo-world` building one static binary for
   `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (rust-lld; pin the toolchain in
   `docs/BUILD-PINS.md`). Deterministic inside the guest: no randomness except the schedule's hash,
   no wall-clock dependence beyond what the Go version has.
2. Identical behaviour to Go for the same inputs: the same `world.log` records (ignoring the `t=`
   time field), the same summary lines, the same check outputs and exit codes, the same carried
   world text.
3. Port the Go unit tests, and write a Go-against-Rust cross-check that feeds both the same
   recorded request sequences while the Go source is still present.
4. Switch `hydo-build` to stage the Rust binary on both architectures; delete the Go source only
   after the proofs.
5. Real runs, each schedule twice identical, same verdicts and `world.log`/check outputs as Go:
   on the bare-metal machine `examples/shop` buggy/fixed with `H=payments.charge@0-1:drop` and
   `H=payments.charge@6-9:reset,p=50`, a power-cut schedule, `examples/redis/hydo-proxy.yaml`,
   `examples/stack-grpc` (cur and planted, `H=orders.place@10-13:drop`) and `examples/redis-bridge`;
   on the Mac the Mac shop example. The binary is inside the images, so the fingerprints change:
   record the new ones next to the old in the examples' `RESULTS.md` and in HISTORY.
6. Write the "Milestone 111" section at the end of `docs/HISTORY.md` and update the hydo-world
   lines in `AGENTS.md`.

## Where it stands

Workspace: `hydo-world` is a member in the root `Cargo.toml`; `Cargo.lock` is updated. The
dependencies resolve and build with Rust 1.90: tokio (current-thread runtime; `rt`, `net`, `time`,
`io-util`, `sync`), h2 0.4, http 1, bytes 1, httparse 1, libc, and the workspace's serde_json and
sha2.

`src/main.rs` is still an empty `fn main() {}` and declares none of the modules, so the workspace
builds but **none of the modules below is compiled or tested yet**.

| File | What it holds | Go source |
|---|---|---|
| `goish.rs` | Go-compatible helpers: record escaping, `%q` quoting, header-key canonicalisation, status texts, the HTTP date, query and path parsing, Go-style error texts | various |
| `gojson.rs` | request-body JSON read and printed as Go's `encoding/json` does | various |
| `config.rs` | loading `world.json` with Go's field matching and defaults, route matching, extractors, the protobuf field reader, answer templates | `config.go` |
| `rules.rs` | parsing `H=` tokens (including the gRPC-only keys) and the fault decision; unit tests against Go's real output for four tokens and ten error messages | `rules.go` |
| `rt.rs` | waiting helpers and the socket options Go sets | — |
| `http1.rs` | the HTTP/1.1 server and client | `serve.go` |
| `h2x.rs` | helpers for cleartext HTTP/2 | `grpc.go` (part) |
| `world.rs` | the record log, webhooks, control commands, the carried world at a power cut, the console summary | `world.go` |
| `check.rs` | `ctl` and every side-effect check | `check.go` |
| `serve.rs` | the listeners, the fake and HTTP-proxy handler, the TCP proxy, the control socket | `serve.go`, `main.go` |
| tests | ports of the Go unit tests: carried-world round trip, too-long carry, rule parsing, fault decisions | `world_test.go` |

## What is left

1. **The gRPC proxy (`grpc.rs`)**, the port of `grpc.go` (628 lines): the streaming HTTP/2 proxy
   with the gRPC `H=` rules (drop, status with and without effect, delay, stream and connection
   reset, cut and hold by direction, selection by route, `rpc=`, `call=`, `p=`/`seed=`) and the
   `kind=call` records. Without it `examples/stack-grpc` cannot run on the Rust binary.
2. **Wiring:** declare the modules in `main.rs` and dispatch the commands as Go's `main.go` does
   (`serve`, `ctl`, `event`, `check`). Then `cargo test --locked`, `cargo clippy --all-targets --
   -D warnings` and `cargo fmt --check` must pass.
3. **The Go-against-Rust cross-check harness** (step 3 above).
4. **Builds and staging:** a build script for both musl targets, the aarch64 musl linker setting in
   `.cargo/config.toml`, the `docs/BUILD-PINS.md` entry, and switching `hydo-build` to stage the
   Rust binary.
5. **Real runs and new fingerprints** (step 5 above).
6. **Docs:** HISTORY M111 and the AGENTS.md lines.

## Notes for whoever continues

- **A behaviour quirk to copy or to fix deliberately:** when the real service answers a gRPC call
  with an error status and no messages, the Go proxy forwards that status in the first headers
  instead of as trailers. A gRPC client then probably reports INTERNAL instead of the real status.
  This was not tested. Copy it to stay identical, or fix it and record the change.
- **Two planned differences from Go** (not proven yet): webhooks send `User-Agent: hydo-world`
  instead of Go's default agent name, and the HTTP proxy adds no `User-Agent` when the client sent
  none. Either keep Go's behaviour for identical records, or record these as deliberate changes.
- **Running the examples on the bare-metal machine:** it has no Nix store, so `stack-grpc` cannot be
  rebuilt there. For an A/B test, replace `/bin/hydo-world` inside copies of
  `~/hydo/build/explore-ob-stack-grpc-{cur,planted}-disk.img` (partition 1 starts at sector 2048).
- **Go on the bare-metal machine:** it has no Go installation of its own. A pinned Go 1.26.5 is in
  `~/hydo-rp` there (from the paused release work); the Mac has Go 1.27.3.
- **Comparing records:** `world.log` lines carry a time field (`t=`); compare without it.

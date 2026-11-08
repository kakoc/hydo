# A message server with planted bugs (design notes)

Status: design notes for step 4 of `docs/design/messaging.md`'s proof plan. Nothing is built.

The messaging checker (Milestone 196) is tested on recorded and generated histories. Step 4
proves it on runs: a small server written for the purpose, a client that records through the
Rust SDK, and one flag per planted bug. Each bug must break exactly one declared claim under the
faults that expose it, and the server without a bug must pass the same schedules.

## The server, `plantq`

`plantq` is a Rust program of a few hundred lines in
`hydo/onboard/examples/messages-planted/server/`, with its own Cargo workspace, like `hydo-read`.
It is not a model of any product: it implements exactly the promises the example declares.

- **Storage.** One append-only log file per channel under `/data/plantq`, written with `write`
  and `fsync` before a send is acknowledged. A record is a send, a delivery, an acknowledgement,
  a nack, an extension or a move to the dead-letter channel. On start the server replays the log.
- **Protocol.** One line per request over TCP on port 7700, one line per answer. The requests are
  `SEND`, `RECV`, `ACK`, `NACK` and `EXTEND`. The client needs no library.
- **Groups.** Each channel has named groups. Within a group, an ordered key delivers one message
  at a time: no later message of the key leaves while an earlier one is out and unacknowledged.
- **Leases.** A delivery holds a lease of the length the request asks for. The server measures
  the lease on `CLOCK_MONOTONIC` and answers with `n=` (the attempt count), `h=` (a handle) and
  `v=` (the lease).
- **Deduplication.** A send's deduplication id is remembered for a window, stored in the log.
- **Dead letters and delays.** After five deliveries without an acknowledgement, a message moves
  to the dead-letter channel. A send may ask for a delay.

## The client and the declaration

The client `plantq-client` (Rust, in the same workspace) records every operation with
`hydo_assert::history::messages`. It has the subcommands `work` (the composer's driver), `drain`
(the check's `final:`) and `ping`. Each worker holds a message for a time drawn from `HYDO_SEED`,
sometimes longer than the lease, so that leases end inside the runs.

The example declares the design's third example plus `dead_letter` and `delay`:

```yaml
checks:
  - id: pay
    type: messages
    history: pay
    final: /bin/plantq-client drain
    claims:
      delivery: at-least-once
      duplicates: {by: dedup-id, window_ms: 300000}
      order: {kind: fifo-blocking, scope: key, batch: ordered}
      lease: {ms: per-delivery}
      after_ack: no-redelivery
      processing: once
      dead_letter: {after: 5, to: pay-dead}
      delay: per-message
```

## The planted bugs

The flag `--plant <bug>` turns on one bug. Each row names the claim it must break and the faults
that expose it.

| bug | what the server does wrong | claim | faults |
|---|---|---|---|
| `lease-half` | ends a lease at half its length | `lease` | none needed |
| `ack-before-sync` | acknowledges a send before `fsync` | `no-loss` | `C=`, `L=` |
| `group-parallel` | lets a later message of a key leave while an earlier one is out | `order` | none needed |
| `ack-in-memory` | keeps acknowledgements in memory only | `after-ack` | `K=` of the server |
| `dedup-forget` | keeps the deduplication ids in memory only | `duplicates` | `K=` of the server |
| `wallclock-lease` | measures leases on `CLOCK_REALTIME` | `lease` | `T=` with `clock_steps: strict` |

## The proof

For each bug, on the KVM engine first (no module, a 4 MiB console), then on hvx with small
message counts, then on the Mac:

1. `hydo explore --strategy process --more-faults --storage-faults` finds a violation of the
   bug's claim and of no other claim.
2. The explorer shrinks it, and the minimal schedule reproduces twice with one fingerprint.
3. The server without a bug passes the same seeds and the minimal schedules.
4. The measures move as expected: the duplicate rate rises under kills of the server, and the
   latency rises under pauses.

The `wallclock-lease` bug passes under the default `clock_steps: undecided` and fails with
`strict`. That run shows that the default does not report a system whose durations depend on the
wall clock.

## Open questions

- The console holds about 40,000 messages on KVM and the Mac, and about 600 on hvx. The hvx
  runs need workloads of a few hundred messages until the history channel through the agent
  exists.
- The example needs the pinned Rust toolchain to build `plantq` from source, as `hydo-read` does.
  Its bytes then go into `docs/BUILD-PINS.md`.

# hydo — a deterministic KVM hypervisor

`hydo` is a small virtual machine monitor (VMM) built directly on Linux KVM whose
defining property is **determinism**: given the same initial image and the same
seed, a guest executes bit-for-bit identically on every run, in every process, on
every boot. That property is what makes a deterministic platform
so useful — once execution is deterministic you can record it, replay it exactly,
snapshot it, branch it, and reproduce any bug you find on the first try. `hydo` is
a from-scratch, working demonstration of that core idea, with a verification
harness that *proves* the determinism rather than asserting it.

This is not a large, mature testing product;
it is a focused, honest implementation of the mechanism at its heart, small enough
to read end to end.

## What determinism buys you, and why it is hard

A normal hypervisor is not deterministic. Run the same program twice and it reads
a different `RDTSC`, gets different `RDRAND` bytes, takes timer interrupts at
different instructions, and races differently between CPUs. That is fine for
running software but ruinous for *testing* it: a bug that appears once may never
appear again, and you cannot bisect what you cannot reproduce.

A deterministic hypervisor removes every source of nondeterminism the guest can
observe. `hydo` handles each source explicitly:

| Source of nondeterminism | How `hydo` makes it deterministic |
|---|---|
| Time (`RDTSC`, timers) | A **virtual clock** that counts VM exits, not wall-clock seconds. The guest reads "time" from a paravirtual port; it advances with work done, identically every run. |
| Randomness (`RDRAND`/`RDSEED`) | The guest draws random words from a **seeded PRNG** (SplitMix64) through a paravirtual port. Same seed → same stream. |
| Interrupt timing | Timer interrupts are injected on a **deterministic clock schedule** — either every *N* VM exits, or (with `--insn-clock`) every *N* retired guest instructions, which is instruction-precise. Never wall-clock. |
| Device I/O | All I/O is mediated by the VMM and served synchronously; with one vCPU there is no completion race. |
| SMP / scheduling | A **single vCPU**. Multi-CPU memory-ordering nondeterminism is out of scope (as it is for most such systems without heavy machinery). |
| Split-instruction state | A port `IN`/`OUT` spans two `KVM_RUN` calls; `hydo` **drains** the pending completion before any snapshot so a restored VM never re-executes it. |
| Pending interrupts | Captured via `KVM_GET_VCPU_EVENTS` so a mid-flight interrupt survives snapshot/restore. |

## The verification harness — the actual proof

`hydo verify` is the point of the project. It runs four independent checks and
fails loudly if any does not hold:

1. **replay** — two full runs from the same image produce the identical 256-bit
   state fingerprint and the identical checkpoint trace.
2. **round-trip** — snapshotting the final state and restoring it into a fresh VM
   reproduces the same fingerprint (the snapshot is lossless).
3. **branch** — pause mid-run, snapshot, and resume from that snapshot in a
   *fresh* VM; it reaches the identical final state as the original that ran
   straight through. This is the time-travel / exploration capability.
4. **seed-sensitivity** — a different PRNG seed yields a *different* final state,
   confirming the guest actually consumes the mediated randomness (so the
   determinism is real, not merely the absence of variation).

The **state fingerprint** is a BLAKE3 hash over the complete machine: all general
and control registers, segment state, FPU/SSE state, pending-event state, every
byte of guest RAM, and all device state (console output, PRNG state, virtual
clock, checkpoints, timer state). Meaningful fields are hashed canonically so that
structure padding — whose bytes the kernel may leave unspecified — cannot make two
equal states hash differently.

```
$ hydo verify --guest build/timer64.bin --mode long --mem 16 --timer 30 --seed 3
  [PASS] replay             run1=7a1702580cf5ca83 run2=7a1702580cf5ca83 (2 checkpoints, 344 ticks, 11 irqs)
  [PASS] round-trip         restored=7a1702580cf5ca83 original=7a1702580cf5ca83
  [PASS] branch             cut@172/344 exits  straight=7a1702580cf5ca83 branched=7a1702580cf5ca83
  [PASS] seed-sensitivity   seed 0x3=...  vs seed 0xdeadbeec=...
  DETERMINISTIC: all 4 checks passed.
```

## Three clock strategies

`hydo` can measure guest progress three ways; interrupts are scheduled against
whichever is active, and all three are deterministic:

- **Exit clock (default)** counts VM exits. The guest runs freely between exits;
  fast, but the granularity is however often the guest happens to exit — it cannot
  place an interrupt inside a tight compute loop that never exits.
- **Instruction clock (`--insn-clock`)** single-steps the guest with KVM hardware
  single-step and counts *retired instructions* (one per `KVM_EXIT_DEBUG`). It is
  **skid-free and instruction-precise** — an interrupt scheduled "every 200
  instructions" lands at exactly instruction 400, 600, 800, … — but pays one VM
  exit per instruction. The counting rule is exact because under single-step a
  port `IN`/`OUT` produces its IO exit *before* retiring and a `DEBUG` exit *when*
  it retires, so counting only `DEBUG` exits counts each instruction once.
- **PMU fast-path (`--insn-clock --fast`)** gives the same instruction-precise
  clock at near-native speed. It free-runs the guest with a hardware
  retired-instruction counter armed to interrupt just before each target, then
  single-steps only the final ~256 instructions to land exactly. On a 3-million-
  instruction guest it is about **33× faster** than pure single-step (0.10 s vs
  3.37 s) and produces the bit-identical result.

The instruction clock reproduces its injection points bit-for-bit. For the timer
guest at period 200:

```
$ hydo run --guest build/timer64.bin --mode long --timer 200 --insn-clock
  retired guest instructions: 1425
  timer interrupts delivered: 7
  injection offsets (insns): [218, 400, 600, 800, 1000, 1200, 1400]
```

The first injection is deferred from 200 to 218 because the guest had not yet run
`sti` at instruction 200 (interrupts were disabled); once enabled, every later
injection lands on the exact 200-instruction boundary. That deferral is itself
deterministic and identical on every run.

### How the PMU fast-path stays exact and reliable

This is the production technique (rr), and getting it *deterministic*
took solving several real hardware quirks, all handled in `step_insn_fast`:

- **Reproducibility.** On this i7-13700HX the raw retired-instruction counter is
  bit-reproducible run to run (measured with `--pmu`), so no conditional-branch
  fallback is needed. It undercounts by exactly one per VM exit (it freezes just
  before the exiting instruction retires), so the true clock is `PMU count + IO
  exits handled` — which reconstructs the single-step ground truth exactly.
- **Non-deterministic skid.** The overflow interrupt lands 20–30 instructions
  *past* the target, and that overshoot varies run to run — so the fast-path never
  injects at the overflow. It free-runs to a safe margin before the target, then
  single-steps to the exact instruction (deterministic regardless of skid).
- **One counter at a time.** Two simultaneous instruction counters get
  time-multiplexed onto the CPU's single fixed counter and corrupt each other, so
  the period is changed in place with `PERF_EVENT_IOC_PERIOD`, never by opening a
  second counter.
- **Descheduling detection.** The perf event intermittently gets bumped off the
  PMU mid-run (~10–20%), which silently freezes the count. Each read checks
  `time_running == time_enabled`; if the event was ever descheduled the run is
  discarded and retried from a fresh VM. A completed run is therefore always
  fully-scheduled and deterministic.
- **Clean pause boundaries.** Pending IO is drained before any snapshot, so a
  branch resumes without re-executing a split `IN`/`OUT`.

The fast-path needs `perf_event_paranoid ≤ 1` (counting guest instructions is a
privileged operation); at the default level 2 it is denied and `hydo` falls back
to single-step, which needs no privilege and is equally exact, just slower.

## Architecture

```
src/
  main.rs      CLI: run / verify / resume, argument parsing
  vmm.rs       VM + vCPU lifecycle, real/long-mode setup, the exit-dispatch
               run loop, snapshot/restore, deterministic timer injection
  memory.rs    mmap-backed guest RAM registered with KVM
  serial.rs    16550 UART at 0x3f8 (console; output is hashed state)
  pv.rs        hydo paravirtual device: deterministic time/random, checkpoints, exit
  det.rs       SplitMix64 PRNG and the virtual clock
  pmu.rs       guest-only perf counters (optional instruction-clock fast-path)
  state.rs     full-state Snapshot: canonical fingerprint + disk save/load
  verify.rs    the determinism harness (replay / round-trip / branch)
guests/        guest payloads assembled to flat binaries by build-guests.sh
```

The run loop is the heart of it. For each VM exit it advances the virtual clock,
decides deterministically whether to inject the timer interrupt, dispatches the
exit to a device, and — when asked to pause at a tick budget — drains any pending
I/O so the pause always lands on a clean instruction boundary.

## Paravirtual device ABI

The guest reaches deterministic services through I/O ports (the port is in `DX`):

| Port | Direction | Meaning |
|---|---|---|
| `0x3f8` | out (8-bit) | UART: write a console byte |
| `0x520` | in (32-bit) | next deterministic PRNG word (advances the PRNG) |
| `0x524` | in (32-bit) | low 32 bits of the virtual clock |
| `0x528` | out (32-bit) | request shutdown with this exit code |
| `0x52c` | out (32-bit) | record a checkpoint value into the trace |

Vector `0x20` is the deterministic timer interrupt (long-mode guests install an
IDT gate for it).

## CLI

```
hydo run    --guest <blob> [--mode real|long] [--mem MiB] [--seed N]
            [--timer TICKS] [--insn-clock] [--fast] [--pmu]
            [--snapshot FILE] [--snapshot-at TICK]
hydo verify --guest <blob> [--mode real|long] [--mem MiB] [--seed N]
            [--timer TICKS] [--insn-clock] [--fast]
hydo resume --snapshot FILE
hydo trace  --guest <blob> [--mode real|long] [--steps N]     # single-step (exit, rip) trace
```

`--insn-clock` switches to the instruction clock (interrupts fire at exact
instruction offsets); add `--fast` for the near-native-speed PMU implementation.
`--pmu` reports guest-only retired instructions/branches/cycles for a run. Both
require `perf_event_paranoid ≤ 1`.

Cross-process time travel, end to end:

```
# Freeze a running VM to disk mid-run, in one process:
hydo run --guest build/timer64.bin --mode long --mem 16 --timer 30 --seed 9 \
         --snapshot mid.snap --snapshot-at 150

# Thaw and finish it in a completely separate process — identical final state:
hydo resume --snapshot mid.snap
```

## Building and running

`hydo` requires Linux with KVM (Intel VT-x or AMD-V) — it cannot run on macOS. The
repository is developed locally and synced to a KVM host to build and run:

```
./dev.sh "./build-guests.sh && cargo build --release && \
          ./target/release/hydo verify --guest build/det64.bin --mode long --mem 16"
```

`dev.sh` rsyncs the tree to `$HYDO_REMOTE` (the build machine) and runs the
given command there. `build-guests.sh` assembles each `guests/*.s` into a flat
binary with GNU `as` + `ld` + `objcopy`, linking `.text` at the load address
encoded in the guest's first-line `# load:` comment.

## Guest authoring

Guests are flat binaries loaded at a fixed guest-physical address. Real-mode
guests (`# load: 0x1000`) are the simplest; long-mode guests (`# load: 0x100000`)
run 64-bit with paging that `hydo` sets up (a 4-level identity map of the low
1 GiB, a 3-entry GDT, and an IDT the guest fills in). See `guests/hello.s` (bring-up),
`guests/det.s` / `guests/det64.s` (determinism device), and `guests/timer64.s`
(deterministic interrupts) as worked examples.

## Honest limitations

- **Single vCPU only.** True SMP determinism requires controlling memory-ordering
  and inter-processor timing; that is a much larger undertaking and is not here.
- **Cooperative guests.** The guest draws time/randomness from the paravirtual
  device rather than executing `RDTSC`/`RDRAND` directly. A fully transparent
  system would trap or rewrite those instructions; `hydo` controls them by
  construction instead, which is sufficient to demonstrate and verify the model.
- **PMU fast-path needs privilege and retries.** The near-native instruction clock
  (`--fast`) counts guest instructions on the PMU, which requires
  `perf_event_paranoid ≤ 1`; without it, `hydo` uses single-step (exact, no
  privilege, slower). The perf event also gets descheduled off the PMU on a
  minority of runs; `hydo` detects this and retries, so results are always
  deterministic but an occasional run costs an extra attempt.
- **Whole-RAM snapshots.** Snapshots copy all guest RAM; dirty-page tracking would
  make them incremental. Not needed to prove determinism.
```

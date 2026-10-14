# hvx VM-exit histogram — performance diagnostic

## What was added (fingerprint-neutral instrumentation)

`struct hvx_vcpu` gained three **write-only, kernel-internal** counters (never read
into guest state / snapshot / fingerprint, so guest determinism is unchanged):

- `u64 exit_hist[64]` — VM-exits indexed by **basic exit reason** (bits 15:0).
- `u64 exit_total`    — total VM-exits.
- `u64 run_ns`        — wall-nanoseconds spent inside `hvx_run_vcpu`.

`hvx_run_vcpu` bumps `exit_hist[basic]`/`exit_total` once per VM-exit (right after
`basic = reason & 0xffff`, so it counts *every* exit incl. the in-kernel-handled ones
that VMRESUME), and accumulates `run_ns` around the run. `hvx_dump_exit_stats()` prints
a per-vCPU histogram (count + %, labelled by reason name) to dmesg from `hvx_release`
(VM teardown). No ABI change, no snapshot field, no stats ioctl.

Inertness verified: all M3–M24 hand-written fingerprints unchanged (iso
`7e586473f6e494d7`, pci `3f95ef833747f3ec`, fault `91170d4dbf101c5b`, net
`0e2b7acf2f4f94ee`, bug_f0 `1083e400458728ff`, cpuid exact, scale M28 PASS); real
full-state boots unchanged (no-disk `640ece2f900fe5b3`, disk `36583d533d7c989b`); JVM
replay/round-trip/branch all bit-identical (`04dd552a5607340d`, MILESTONE 31 PASS).

## Measured histograms (L1, nested; hvx_linux free-run boots)

### JVM solo boot (c1sync) — 640 MiB, ext4 ro root + rw data disk
wall 3.20 s · run_ns 2746.6 ms · total **402,015** exits · 146,366 exits/s · icount 399,280

| reason | name          | count   |    %  |
|-------:|---------------|--------:|------:|
|     16 | RDTSC         | 246,914 | 61.4% |
|     30 | IO (16550)    | 123,468 | 30.7% |
|     32 | WRMSR         |  17,565 |  4.3% |
|      7 | INT-WINDOW    |   6,030 |  1.4% |
|     48 | EPT-VIOLATION |   2,844 |  0.7% |
|      1 | EXT-INT       |   2,769 |  0.6% |
|     12 | HLT           |   1,611 |  0.4% |
|     10 | CPUID         |     772 |  0.1% |
|     31 | RDMSR         |      42 |  0.0% |

RDTSC + IO = **92.1%** of all exits.

### Light no-disk baseline boot — 256 MiB, boot-to-shell + one echo
wall 2.96 s · run_ns 2929.1 ms · total **476,967** exits · 162,836 exits/s · icount 458,752

| reason | name          | count   |    %  |
|-------:|---------------|--------:|------:|
|     16 | RDTSC         | 295,779 | 62.0% |
|     30 | IO (16550)    | 113,702 | 23.8% |
|     32 | WRMSR         |  35,196 |  7.3% |
|      7 | INT-WINDOW    |  15,279 |  3.2% |
|     12 | HLT           |  12,254 |  2.5% |
|      1 | EXT-INT       |   2,936 |  0.6% |
|     48 | EPT-VIOLATION |   1,368 |  0.2% |
|     10 | CPUID         |     411 |  0.0% |
|     31 | RDMSR         |      42 |  0.0% |

## Diagnosis

**RDTSC is the dominant VM-exit (~61–62%) in both the JVM and a plain boot; UART/IO is
second (~24–31%). Together ~92% of all exits.** RDTSC dominance is *inherent to the
stack*, not JVM-specific: Java `System.nanoTime()`/timestamps → glibc
`clock_gettime(CLOCK_MONOTONIC)` → vDSO → `rdtsc` in userspace (clocksource=tsc; no PV
clock advertised), and every such read is a hardware VM-exit → kernel handler →
VMRESUME (~6–7 µs each here, ~10× inflated by nested L2-under-L1 double world-switches).

Scaling: a JVM emits ~90k RDTSC per second of guest boot. A 30-min Kafka boot ⇒ ~10^8
RDTSC exits ⇒ ~11+ min of pure RDTSC-exit overhead, plus a comparable UART/IO chunk —
consistent with the observed ~30 min. QEMU `-icount` computes each RDTSC inline in
software (no world switch), which is why hardware VT-x loses to software here.

## Proposed determinism-preserving fixes (NOT implemented)

1. **Cut per-exit COST (biggest lever).** RDTSC *count* is guest-driven and cannot be
   reduced without breaking determinism (hardware TSC-offset would return the
   non-deterministic host counter — trapping RDTSC is hvx's founding purpose). But the
   ~6–7 µs/exit is ~10× inflated by running nested; on **L0/bare-metal** each VM-exit is
   far cheaper, recovering most of the gap. Determinism-neutral (deployment change).
2. **Cut the UART/IO bucket (24–31%).** Give the emulated 16550 a real TX FIFO with
   THRE-interrupt batching so the serial driver issues far fewer port accesses per
   console byte (byte stream — hence fingerprint — unchanged), and lower runtime console
   verbosity (`loglevel`, drop `earlyprintk` after early boot) for non-verify runs.
3. **Coarsen the deterministic idle-heartbeat + LAPIC TSC-deadline granularity.** A
   thread-park-heavy service (Kafka) takes many HLT→timer-inject→ISR/scheduler cascades,
   each a burst of RDTSC; a coarser tick reduces the cascade count over a long run.

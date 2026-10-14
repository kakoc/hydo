# DESIGN_NEXT — the next hvx milestone

Design spec for the highest-value milestone after M24 (virtio-net fault injection, in
progress). Read-only design pass; grounded in `hvx/hvx.c`, `hvx/hvx_abi.h`,
`hvx/hvx_linux.c`, `hvx/hvx_disk_boot_verify.c`, `CLAUDE.md` (M1–M24), and the
deterministic-simulation-testing literature, FoundationDB's among it (sources at the
end). Written in the style of `hvx/PHASE6_DESIGN.md`.

---

## TL;DR — the recommendation

**Milestone 25 — the FLAGSHIP end-to-end demonstration: a real application running as
the L2 guest whose *committed* data is lost by a SEEDED disk fault, where the failure is
detected by an in-guest invariant checker (a test oracle) and then reproduced BIT-FOR-BIT
by replay and by branch-from-snapshot.**

This is the actual value of deterministic simulation testing — *find a real bug through
seeded fault injection, then reproduce it perfectly* — demonstrated on real software for
the first time in this project. Everything the mechanism needs already exists (real-Linux
boot, virtio-blk disk, the M22 seeded disk-fault engine, snapshot/branch, the canonical
fingerprint harness); this milestone is mostly the *assembly* of those primitives around a
real application plus a real bug plus an oracle. It requires **no new VMX controls, no new
EPT/MMIO decode, no kernel run-loop changes** in the minimal landing — so it carries almost
none of the nested-L1 hazard that every prior device milestone did.

The other three candidates are evaluated and declined at the end; the short version:
candidate 4 (fast instruction clock) is hardware-BLOCKED on this hybrid box and its only
consumer (divergence bisection) is moot because free-run is already deterministic;
candidates 2 and 3 (fuller TCP stack / multi-VM) are larger, riskier, and are far more
compelling built *on top of* a working flagship than before one exists.

---

## 1. Goal, and why it is the right next step

**Goal.** Boot a real, unmodified-as-far-as-possible application as the L2 guest, give it a
virtio-blk backing disk, drive it with a workload that records a set of *committed* writes,
then check an invariant — *every write the application acknowledged as durable is still
readable with its committed value*. Run it once with faults disabled (the invariant holds,
and the run is deterministic). Then install a **seeded** disk-fault schedule (M22) and show
that under seed *S* the invariant is **violated** — a genuinely-committed key is lost — and
that this failing run:

1. **replays** bit-for-bit (two fresh runs reach the identical full-state fingerprint and
   the identical oracle verdict);
2. **branch-from-snapshots** bit-for-bit (rewind to the instruction just before the fault,
   restore into a fresh VM, finish, and land on the identical failing state);
3. is **seed-sensitive** (a different seed loses a different key, or none — proving the
   fault is really consumed, so the determinism is real rather than the mere absence of
   variation);
4. and **does not fire at all with faults disabled** (proving the application is correct in
   the absence of the fault, so the fault is what surfaces the latent bug).

Those four checks are deliberately the same four `hydo verify` / `src/verify.rs` runs
(replay / round-trip / branch / seed-sensitivity) that the whole project has used as its
proof-of-determinism, now applied to a *bug* on *real software* rather than to a toy guest.

**Why now.** Milestones 1–24 built a complete deterministic machine: it owns time
(RDTSC/TSC-deadline), interrupts (instruction-timed injection), CPUID/MSR, the LAPIC/IOAPIC,
a real Linux boot to an interactive shell, SMP, a virtio-blk disk WITH seeded faults, a
virtio-net device WITH seeded faults, and snapshot/branch/replay with a canonical
fingerprint. **Every one of those is a mechanism. None of them is yet a demonstration that
the mechanisms, composed, catch a real bug in real software and reproduce it.** That
composition *is* what deterministic simulation testing is for, and the reason FoundationDB could ship a
distributed database — "any test run that found a correctness issue could be reproduced …
so the underlying bug could be identified and fixed." The project has spent 24 milestones
earning the right to show this; M25 is where it pays off. It is also, bluntly, the most
compelling single thing to put in front of anyone: not "we can inject a deterministic disk
fault," but "we found a durability bug in a real program, here is the seed, and here is the
identical failure reproduced on demand and rewound to the exact moment it happened."

---

## 2. The load-bearing decisions

Three decisions shape the whole milestone. Get them right and the rest is plumbing.

### 2.1 The software under test (SUT): a small, real, raw-block KV store with a *real* durability bug

The demonstration lives or dies on the honesty of the bug. Options considered:

- **SQLite** (real, famous, statically linkable). Rejected as the *first* landing: it needs a
  filesystem on the disk (so >1 MiB and a mkfs step), it is large, and its durability is
  robust enough that surfacing a failure needs a contrived `PRAGMA synchronous=OFF` — which
  invites the objection "you turned durability off." Keep it as the **F4 stretch** once the
  disk cap is raised (§2.3).
- **Redis with `appendfsync everysec`** (a real, documented durability window). Rejected for
  the first landing: large, and it wants a client/socket, pulling in the net device before
  the disk story is even proven.
- **`hydokv` — a purpose-built ~200-line C key/value store with one deliberate, well-known,
  realistic durability bug.** RECOMMENDED as the primary SUT. It does **raw block I/O on
  `/dev/vda` with `O_DIRECT`** (no filesystem — matching the existing M21 `/init` that already
  does `dd if=/dev/vda`), so it fits the 1 MiB disk, needs no `mkfs`, and adds no new machinery.
  The bug is one that FoundationDB's BUGGIFY and Jepsen hunt for real: **it acknowledges a
  write as committed before the write is durably on the device** (e.g. it updates its
  in-memory index and returns success, batching the actual barrier/flush and only issuing it
  every N writes — a real "ack-before-fsync" optimization gone wrong). Under a fault that
  models "an un-flushed write lost on power failure," an acked-committed key vanishes.

  This is honest because (a) the bug is a real class, (b) the app is *correct without the
  fault* (F1 proves it), and (c) the fault models a real hardware behavior, not an impossible
  one (§2.2). The design keeps `hydokv` single-threaded with **at most one outstanding I/O**,
  which both matches hvx's one-completion-per-kick device model and keeps the SUT itself
  trivially deterministic.

  The follow-on (F4) swaps `hydokv` for an **unmodified real tool** so the bug is
  demonstrably pre-existing rather than authored; `hydokv` exists to prove the *loop* first.

### 2.2 The fault must faithfully model a crash, and the read must bypass the page cache

This is the subtle correctness point that makes the difference between a real durability
demonstration and a trivial one.

The M22 `HVX_FAULT_DROP` class "acks `VIRTIO_BLK_S_OK` but does not persist the data" — i.e.
it models a device (or a power-loss window) that *loses an un-synced write*. `HVX_FAULT_TORN`
models a multi-sector write clipped to its first sector — exactly a crash mid-write across a
sector boundary. Both are faithful crash/power-loss models.

But a dropped *physical* write is **invisible to a naive read-back** because Linux serves the
read from the **page cache**, which still holds the value the guest wrote. The dropped write
only becomes observable once the cache is bypassed. Two faithful ways to expose it, both used:

- **`O_DIRECT` I/O in `hydokv`** — every read is a real device read, so a DROP-faulted write is
  observable on the next read with no crash needed, *and* it is faithful because `O_DIRECT` is
  exactly how a database manages its own data file. This is the simplest F2 demonstration.
- **Crash-and-recover** — model a crash by having the store **close and re-open `/dev/vda`**
  (or re-exec a fresh instance) and rebuild its index by replaying the on-disk log. This drops
  the page cache and is the most vivid demonstration ("the process crashed and came back and a
  committed key is gone"). A stronger variant uses the hvx snapshot machinery as the crash: take
  the disk via `HVX_GET_DISK` as the *sole* durable artifact, discard the RAM, boot a fresh VM,
  `HVX_SET_DISK` the extracted image, and run recovery — this also exercises disk-snapshot
  round-trip. Offered as an F2 sub-variant.

**The design must state the fault-class ↔ real-world mapping explicitly** in the demo output,
so the result reads as "a power-loss-lost write surfaced an ack-before-fsync bug," not "we
made the disk return the wrong bytes." Anti­thesis and FDB inject only faults that *can*
happen in production; this milestone must hold to the same bar.

### 2.3 One small, contained ABI/kernel change is worth pre-approving: raise `HVX_DISK_MAX`

`HVX_DISK_MAX` is `1 << 20` (1 MiB) and the image is a single `kvmalloc`. The minimal
`hydokv` demo fits inside 1 MiB (a few hundred small records + a superblock). But headroom —
and the F4 SQLite-on-a-filesystem stretch — wants a few MiB. Raising `HVX_DISK_MAX` to, say,
`16 << 20` is a **one-constant change** plus a check that the `kvmalloc` and the
snapshot/`HVX_GET_DISK` copy handle the larger size (they already `kvmalloc`; the fingerprint
already hashes the whole image length-prefixed). Flag it as in-scope-if-needed; the minimal
landing does **not** require it, so it stays optional and does not perturb any existing
fingerprint (no VM that keeps a ≤1 MiB disk changes).

---

## 3. Mechanism, in detail — reusing existing hvx machinery

Nothing below is new VMX/EPT/kernel work in the minimal landing. Each row names the exact
existing primitive it stands on.

| Need | Existing hvx primitive reused |
|---|---|
| Boot the SUT (bzImage + initramfs) | `hvx_boot.h` recipe (M16) via `HVX_CREATE_VM`; `hvx_linux.c` free-run loop (M14/M15) |
| SUT filesystem/binary | a NEW `build/initramfs-kv.cpio.gz` staged by `build-linux.sh`, modeled byte-for-byte on the M21 `initramfs-disk.cpio.gz` (virtio module stack + raw-`/dev/vda` `/init`) |
| Backing disk (the store's data file) | `HVX_SET_DISK` with `HVX_DISK_NO_IDMAP` (M21) — a real OS builds its own page tables, so the module must not poke them |
| Real Linux binds the disk over MSI-X | the M21 MSI-X path (`vblk_msix_irq`, host bridge at 00:00.0, the HLT-jumps-to-completion fix) — unchanged |
| The seeded fault schedule | `HVX_SET_FAULT` / `struct hvx_fault` (M22): `seed`, `classes = HVX_FAULT_DROP` (and/or `HVX_FAULT_TORN`), `probability` — the whole schedule is a pure function of `(seed, per-request index)` computed by `vblk_fault_decide` in `virtio_blk_process` |
| Install the policy from the loader | `hvx_linux.c` env-gated fault policy (M22): `HVX_FAULT`, `HVX_FAULT_CLASSES`, `HVX_FAULT_PROB` — already wired, inert when unset |
| Snapshot / branch / replay | `HVX_GET_SNAPSHOT` + memcpy(RAM) + `HVX_GET_DISK`; restore is the inverse (M7/M20/M21). The fault policy + PRNG counters already ride in `struct hvx_snapshot` (`fault_seed`, `fault_req_seq`, `fault_kick_seq`, `fault_count`) |
| Canonical fingerprint | the FNV-1a over console + snapshot (meaningful fields, fixed order) + 256 MiB RAM + disk image, exactly as `hvx_disk_boot_verify.c` (M21/M22) already computes it |
| The oracle's output channel | the serial console (`HVX_GET_CONSOLE`) — the harness watches for the verdict marker, exactly how M16 watches for `hydo-boot-marker` |

**The new artifacts** (all *userspace* + build scripts; the `.c` kernel module is untouched
in the minimal landing):

1. **`guests/hydokv.c`** (or `hvx/sut/hydokv.c`) — the ~200-line raw-block, `O_DIRECT`,
   single-outstanding-I/O KV store with the ack-before-durability bug. Statically linked.
2. **A `/init`** (in the new initramfs) that: brings up nothing networked, loads the virtio
   module stack (as M21's disk `/init` does), runs the **workload** against `/dev/vda`, then
   runs the **oracle**, and prints a machine-checkable verdict line — `HYDO-ORACLE: OK` or
   `HYDO-ORACLE: VIOLATION key=<k> expected=<v> got=<g>` — followed by a stop marker
   `hydo-flagship-marker: oracle complete`.
3. **`build-linux.sh`** extension — stage `build/initramfs-kv.cpio.gz` **byte-reproducibly**
   (fixed mtimes + sorted names + `gzip -n`, and the pinned busybox — the exact discipline
   M21/M23 already established, because the initramfs is loaded into hashed guest RAM and any
   drift moves the fingerprint).
4. **`hvx/hvx_bug_verify.c`** — the verify harness, modeled on `hvx_disk_boot_verify.c`: boots
   the KV image with the disk attached, optionally installs the seeded fault, runs to
   `hydo-flagship-marker`, parses the `HYDO-ORACLE:` verdict, and computes the full-state
   fingerprint. It runs the four checks of §1.
5. **`hvx/run-flagship-demo.sh`** — host-side runner (scoped kills only, per the L1 safety
   rules), staging + boot + grep-the-verdict + run the harness, mirroring `run-fault-demo.sh`.

### The workload and oracle (the "test")

Modeled on FoundationDB workloads (a workload drives operations; an oracle checks an
invariant against a reference model):

- **Populate.** Write `K` records `put(key_i, val_i)` where `key_i`/`val_i` come from a
  **seeded in-guest PRNG** (or fixed constants) so the workload is itself deterministic.
  For each `put` the store returns "committed"; the workload records `key_i → val_i` in a
  **reference model** kept in guest RAM (the ground truth of what was acknowledged).
- **Crash / cache-bypass.** Either `O_DIRECT` throughout (§2.2), or close+re-open `/dev/vda`
  and replay the log, so subsequent reads hit the real device.
- **Verify (oracle).** For every key in the reference model, `get(key)` from the store and
  assert it equals the committed value. On the first mismatch, print the `VIOLATION` line.

The invariant is **read-committed-durability**: *a key the store said was committed must be
readable with its committed value after a cache-drop.* This is the simplest possible
distributed-systems-flavored oracle and is directly falsified by a lost committed write.

---

## 4. Phased sub-plan (smallest testable first)

- **F0 — the oracle + fault + branch loop, in a HAND-WRITTEN guest (no Linux, seconds to
  run).** Extend the `hvx_fault_test.c` pattern (PIO-doorbell virtio-blk, no PCI, no boot):
  a tiny in-guest "store" writes a record, the seeded DROP fault drops it, a read-back
  detects the loss, and the test asserts (a) the oracle fires, (b) replay is byte-identical,
  (c) branch-from-snapshot mid-flight reproduces. This proves the *entire logical loop*
  (record-committed → fault → oracle-detects → reproduce) with **zero boot cost**, before any
  real-Linux work. Fastest possible signal; also a permanent regression test.
- **F1 — `hydokv` on real Linux, NO fault.** Boot `initramfs-kv.cpio.gz` with the disk
  attached, run the workload + oracle, assert `HYDO-ORACLE: OK`, and have `hvx_bug_verify`
  show two fresh boots identical + branch identical → a **new no-fault baseline
  fingerprint**. Proves the app is correct and deterministic on hvx. Expect the usual
  real-Linux walls (module load order, `O_DIRECT` alignment) — decode-and-fix per the
  established discipline.
- **F2 — install the seeded DROP fault (THE milestone).** With `HVX_FAULT=<S>` +
  `HVX_FAULT_CLASSES=DROP`, the oracle prints `VIOLATION` (a committed key lost); two
  fault-injected boots + a branch taken *across the faulted write* all fingerprint-identical
  → a **new VIOLATION baseline**. This is the demonstration. Include the fault-class ↔
  power-loss mapping in the demo narration (§2.2). Sub-variant: snapshot-as-crash (§2.2).
- **F3 — seed sweep + TORN.** A host-side loop over a handful of seeds shows which seeds
  trigger the violation and which do not (the "explore the state space, every seed a
  different corner" story from FDB/BUGGIFY), each reproducible; add `HVX_FAULT_TORN` as a
  second, distinct bug surface (a corrupt record rather than a missing one).
- **F4 (stretch) — an unmodified real SUT.** Raise `HVX_DISK_MAX` (§2.3), stage a small
  filesystem image, and run SQLite (or another off-the-shelf tool) so the surfaced bug is
  demonstrably pre-existing, not authored. Same four checks.
- **F5 (stretch) — the network flagship, once M24 lands.** A tiny client/server protocol
  where an M23/M24 network fault (drop / corrupt / reorder / duplicate) surfaces a protocol
  bug, reproduced bit-for-bit. This is the natural bridge toward candidates 2 and 3 and
  should reuse this milestone's oracle/verify scaffolding wholesale.

Land F0–F2 as the milestone; F3 is a cheap add; F4–F5 are explicitly deferred.

---

## 5. Determinism considerations — every new input a function of the clock or the seed

The determinism budget is small because hvx already closed the classic sources; the new
surface is the SUT's own behavior. Each input, and why it stays deterministic:

- **The fault schedule** — a pure function of `(fault_seed, per-request index)` and, for
  reorder, `(fault_seed, per-kick index)`, computed by the M22 splitmix64 engine. Already
  proven deterministic (M22 fingerprints, and the real-Linux I/O-error boot
  `b49eb759b9d607aa`). The per-request index is deterministic because the SUT's I/O pattern
  is deterministic.
- **The workload's keys/values** — drawn from a seeded in-guest PRNG or fixed constants;
  no wall-clock, no un-mediated randomness.
- **The SUT's control flow** — it reads only `/dev/vda` (deterministic device) and writes the
  console (captured). It must take no un-mediated nondeterministic input. Watch items:
  - **`getrandom()` / `/dev/urandom`.** If libc/busybox/the app pulls from the kernel CRNG,
    the returned bytes are deterministic **only if all the CRNG's entropy inputs are** — which
    under hvx they are (no exposed `RDRAND`/`RDSEED`, a deterministic timer, a deterministic
    boot). M16 already proved the *entire* boot is bit-identical, which *is* proof that the
    kernel RNG state is deterministic under hvx. So this is expected to be fine — **but the
    harness must prove it (fingerprint equality), not assume it.** If a divergence ever
    appears, pin the SUT to fixed keys and a seeded userspace PRNG.
  - **Timestamps in records.** If `hydokv` stamps records with a time, it must use the virtual
    TSC (deterministic) — or, simpler, not stamp at all. Design choice: no timestamps.
  - **Single outstanding I/O.** Keeping the SUT to one in-flight request removes any
    completion-ordering nondeterminism and matches the one-completion-per-kick device model;
    M22's REORDER is the *only* sanctioned source of completion reordering, and it is seeded.
- **The crash/recovery** — in-guest close+reopen (or snapshot-as-crash) re-reads the same
  deterministic disk; no new input.
- **The M16 clock-creep discipline is inherited unchanged.** The disk completion fires at
  `kick_icount + HVX_VBLK_LATENCY + Σdelays` — a pure function of the instruction clock — and
  the MSI-X vector/dest come from the guest-programmed (deterministic) table. Nothing in this
  milestone touches the run-loop, so the discipline holds by construction.

### Fingerprint / verify story

Reuse the `hvx_disk_boot_verify.c` fingerprint verbatim: FNV-1a over the reconstructed
**console** bytes + the `HVX_GET_SNAPSHOT` struct (meaningful fields, fixed little-endian
order, no raw padding) + all 256 MiB of **guest RAM** (via mmap) + the **disk image**
(`HVX_GET_DISK`). The M22 fault policy/PRNG state already lives in the snapshot, so a branch
reproduces the schedule. Two additions:

1. The harness parses the `HYDO-ORACLE:` verdict from the console and asserts it is a function
   of the seed (OK with faults off / VIOLATION under seed S / a *different* verdict-or-key
   under a different seed).
2. New named baselines recorded in `CLAUDE.md`, exactly like the existing ones: the no-fault
   OK fingerprint and the seed-S VIOLATION fingerprint, both whole-run and branch, and the
   assertion that the no-disk / with-disk / iso / pci / net baselines are all **unchanged**
   (the module is untouched; a KV VM simply attaches a disk + a fault policy, both of which
   are inert-by-default paths that preserve every prior fingerprint).

---

## 6. Risks

- **"You wrote a buggy app on purpose."** The central credibility risk. Mitigations:
  (a) make the bug a documented real class (ack-before-fsync / torn record — both found in
  real systems); (b) F1 proves the app is *correct without the fault*, so the fault is what
  surfaces it; (c) state the fault ↔ power-loss mapping explicitly; (d) land F4 (an unmodified
  real SUT) as the follow-on so the loop is proven on code nobody wrote for the demo.
- **Page cache hides the dropped write** (§2.2). If the read-back is served from cache the
  oracle wrongly passes. Mitigation is load-bearing: `O_DIRECT` and/or close+reopen so the
  read is a real device read. The design treats this as a correctness requirement, not a
  detail.
- **Hidden nondeterminism in the guest userspace** (`getrandom`, timestamps, threads). M16's
  whole-boot determinism makes this *expected-fine*, but the harness must *prove* it via
  fingerprint equality; the fallback (fixed keys, seeded userspace PRNG, single-threaded,
  no timestamps) is cheap and should be the default posture.
- **Disk size (1 MiB).** The minimal `hydokv` fits; F4 needs the §2.3 `HVX_DISK_MAX` bump.
  Contained, optional, and it perturbs no existing fingerprint.
- **Nested-L1 hazards — essentially none new.** No new VMX controls, no new EPT/MMIO windows,
  no run-loop change; the milestone reuses the proven M21/M22 disk-boot path. The
  A/D-bit page-table gotcha (the M22 post-fix, `pdpt[3] |= A|D`) is already handled *and* is
  irrelevant here because the real-Linux boot passes `HVX_DISK_NO_IDMAP` (the module never
  pokes the guest's tables). The only operational cost is boot time — but the whole milestone
  runs in **fast free-run** (`HVX_RUN`, no MTF single-step), exactly like M16/M21/M22, so it
  is minutes, not the astronomical single-step path. Expect the usual "boot to a wall, decode
  the fault, fix the initramfs/`O_DIRECT` alignment" iterations, not VMX debugging.
- **Coordination with the in-progress M24.** M24 (net faults) touches `virtio_net_*`,
  `hvx_abi.h`, `hvx_linux.c` (net env-gates), `build-linux.sh`/`deploy-hvx.sh`. This milestone
  is deliberately **disk-first** to stay clear of that work: it touches `virtio_blk` only
  (unchanged), adds a *new* initramfs + a *new* verify harness + a *new* SUT file, and its
  loader env-gates (`HVX_FAULT`, already M22) do not overlap M24's net gates. Rebase the
  `build-linux.sh` / `deploy-hvx.sh` list additions on top of M24. The net flagship (F5)
  waits for M24 to land.

---

## 7. Honest scope / what to defer

- **In scope for the landing (F0–F2):** `hydokv` + the workload/oracle + the seeded DROP
  fault + `hvx_bug_verify` + the four determinism checks + the demo runner. TORN (F3) is a
  cheap add.
- **Deferred:** the unmodified-real-SUT / SQLite-on-a-filesystem story (F4, needs the disk-cap
  bump + a filesystem image); the network flagship (F5, needs M24); multi-outstanding-I/O and
  a real filesystem's own barrier semantics; larger disks; write-fault classes exercised
  through a *mounted* filesystem rather than raw `O_DIRECT`.
- **Explicitly NOT in scope:** any fast-instruction-clock work (candidate 4 is
  hardware-blocked, M17), any multi-VM topology (candidate 3), any new VMX control or run-loop
  change. If F1/F2 appear to *need* a kernel change, stop and reconsider — the whole premise
  is that the mechanisms are already in place and this milestone composes them.

---

## 8. Why not the other candidates

**Candidate 4 — a fast instruction clock for instruction-exact bisection.** Declined: it is
BLOCKED at the hardware level on this box and its only consumer is moot here. M17 established
with direct evidence that the L0 CPU is a hybrid i7-13700HX, that KVM hardcodes guest-vPMU-off
on hybrid silicon (`enable_pmu` stays `N` even after a successful reload with `enable_pmu=1`),
and that the nested L1 therefore has **no architectural PMU at all** (`CPUID.0AH = 0`, every
PMU MSR `#GP`s). There is no counter to count with — not a config toggle. The VMX-preemption
timer that *is* available counts scaled TSC ticks, not retired instructions, so it cannot
substitute. And critically, the *purpose* of a fast clock — bisecting the first diverging
instruction between two runs — is moot on this box because M16 already proved free-run is
**fully deterministic**: there is no divergence to bisect. The only real unblocks are a
non-hybrid host (which the project does not have) or patching host KVM (forbidden: no host
root, and the gate exists precisely because hybrid vPMU counts are unreliable). Re-designing
this would reproduce M17's own conclusion.

**Candidate 2 — a fuller network stack (ICMP echo / TCP handshake / two guests talking).**
Declined as the *next* step, not on merit but on sequencing. Real ICMP/ARP already works
(M23's stretch: real Linux answers a scripted ARP, `fd5663b60bf9412e`). A TCP handshake to a
host-side peer is mostly host-side protocol code that adds no new *determinism* machinery
(the frames are already deterministic), and two L2 guests talking is really candidate 3. More
importantly, a fuller network stack is far more compelling demonstrated as *a bug the network
faults surface in a real client/server protocol* — which is precisely F5, built on this
milestone's oracle/verify scaffolding. Do the flagship first; then F5 gives the network work
a payoff instead of a plumbing exercise.

**Candidate 3 — multi-VM / true distributed determinism.** Declined as too large and too
early, though it is the most realistic topology and the right *eventual* direction.
Today hvx is one VM per fd; SMP (M19) is multiple vCPUs *sharing* one address space and one
device model. Multi-VM means N independent guests (separate RAM/EPT/device models) whose
virtio-net devices are cross-connected through a **deterministic switch**, all serialized on
one execution context and interleaved by a **global** instruction (or exit) quantum — a
substantial new scheduler with subtle new determinism questions (whose turn delivers a frame,
at what instant on the *receiver's* clock, how a global quantum composes with each guest's
free-run cadence). It is a multi-milestone effort and it is far more convincing once there is
a single-guest flagship proving the fault-injection-plus-reproduce loop actually catches
bugs. Build the proof on one node; then scale the topology. (When it comes, it should reuse
this milestone's oracle model and the M23/M24 net device as the inter-node link.)

---

## Sources

- FoundationDB — *Simulation and Testing* (https://apple.github.io/foundationdb/testing.html)
  and the FDB paper (https://www.foundationdb.org/files/fdb-paper.pdf) — single-threaded
  deterministic cluster simulation; `deterministicRandom()`; BUGGIFY fault points; injected
  machine/disk/network faults incl. "corruption of unsynchronized writes when machines
  reboot" (the exact durability class this milestone surfaces).
- Pierre Zemb — *Diving into FoundationDB's Simulation Framework* and *Designing Rust FDB
  Workloads That Actually Find Bugs* (https://pierrezemb.fr/posts/diving-into-foundationdb-simulation/,
  https://pierrezemb.fr/posts/writing-rust-fdb-workloads-that-find-bugs/) — the workload +
  oracle + reference-model pattern this milestone's test adopts.
- Phil Eaton — *What's the big deal about Deterministic Simulation Testing?*
  (https://notes.eatonphil.com/2024-08-20-deterministic-simulation-testing.html) — accessible
  framing of DST + fault injection.

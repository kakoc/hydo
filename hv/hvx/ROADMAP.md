# hvx — ROADMAP: the loopback-first deterministic multi-service stack

This document frames every future hvx milestone as a deliberate step up one staircase
toward a single destination. It is forward-looking and motivating, but grounded: each
rung names what it concretely requires, what it unlocks, and its key risk. The milestone
*history* (M1–M25) lives in `CLAUDE.md`; the *system reference* lives in
`ARCHITECTURE.md`; the *flagship design rationale* lives in `DESIGN_NEXT.md`. This file
is the map from here to there.

Status anchor: **M1–M27 complete.** M26 (unmodified SQLite on a real ext2 fs) and M27 (Rung 2
below — unmodified Redis, co-located client↔server over `127.0.0.1` loopback, reproducing the
documented `appendfsync` AOF durability window under a faithful volatile-cache power loss)
have both landed and are verified in L1. All claims below are grounded in the code as of M27.

---

## 1. The destination

**Run a real, production-shaped software stack — a gRPC service + Kafka + Redis + MySQL —
as multiple services co-located in ONE deterministic L2 guest, communicating over the
`127.0.0.1` loopback, and use hvx's seeded disk/clock fault injection to find real
durability, crash-consistency, and timing bugs across that stack and reproduce each one
bit-for-bit.**

This is the value of deterministic simulation testing applied to a real multi-service
system: not "we can inject a deterministic fault," but "we found a whole-system bug, here
is the seed, and here is the identical failure reproduced on demand and rewound to the
exact instruction it happened."

### Why "loopback-first" is the right destination

In a fully-deterministic VM, **inter-service traffic over loopback is deterministic for
free.** Loopback (`lo`) is not a device — it is an in-kernel memcpy driven entirely by the
guest's own deterministic clock and scheduler. So when N services live in one guest and
talk to each other over `127.0.0.1`, every request, response, retry, and timeout between
them is already a pure function of the instruction clock. No new networking machinery, no
virtual switch, no cross-guest scheduling quantum is required to make the *interactions*
reproducible. That is the whole insight: the hard part of distributed determinism (the
network) is bypassed by co-location, leaving only each service's own nondeterminism to
tame — which is exactly what M1–M25 already built the tools for.

### What the destination gives

- **Whole-system determinism** — the entire multi-service stack, boot to bug, is
  bit-for-bit reproducible (as M16 proved for a single-service boot: fingerprint
  `640ece2f900fe5b3` reproduces exactly).
- **Snapshot / branch / replay on the running stack** — rewind to any instruction, restore
  into a fresh VM, and re-run the identical continuation (M7 / M16 / M25).
- **Disk + clock fault injection that already exists** — the M22 seeded disk-fault engine
  (delay / error / drop / torn / reorder, keyed by a splitmix64 PRNG over the request
  index) applies unchanged to any service's data disk; the deterministic virtual TSC /
  TSC-deadline clock (M4–M6, M15) applies to any service's timing.
- **Reproduction of the bug classes that matter** — durability bugs (ack-before-fsync, the
  M25 demonstration), crash-consistency bugs (torn writes, lost un-synced writes), and
  timing bugs (an interrupt or timeout landing at a different instruction) — each found by
  a seed and reproduced perfectly.

### What the destination explicitly DEFERS

**Inter-node network-partition faults are NOT part of the loopback-first destination.**
Network faults belong at the *node / pod boundary*;
same-node / loopback traffic is never faulted, because a partition between two processes on
the same machine is not a thing that happens. Faulting the network *between* services
therefore requires the multi-node frontier — separate guests through a fault-injecting
switch — which is a distinct, later, parallel track (see §4). Loopback-first deliberately
sidesteps it: it gets whole-system determinism + disk/clock faults *now*, and leaves
network-partition faults for when the multi-node topology exists.

---

## 2. Where we are (M1–M25)

Twenty-five milestones built a **complete deterministic machine** and then proved it
catches a real bug. The full timeline is in `CLAUDE.md`; the summary:

| Band | Milestones | What it delivered |
|---|---|---|
| **VMX bring-up** | M1–M3 | Own VMCS, EPT, `/dev/hvx` userspace device (a mini-KVM ioctl API) |
| **Determinism engine** | M4–M9 | Deterministic RDTSC, the MTF instruction clock, instruction-clock TSC, snapshot / branch / replay, the canonical fingerprint + four-check verify harness |
| **Real-Linux road** | M10–M16 | PIO/serial console, 256 MiB RAM, deterministic CPUID/MSR, sregs persistence, and finally a real Linux 6.6.35 kernel booting to the busybox shell **bit-for-bit deterministically** (`640ece2f900fe5b3`), branch-from-snapshot included |
| **Fast clock** | M17 | **BLOCKED by hardware** — the L0 CPU is a hybrid i7-13700HX and KVM hardcodes guest-PMU-off on hybrid silicon; no vPMU in L1, so no fast instruction clock on this box (not a config toggle) |
| **Interactivity + SMP** | M18–M19 | Serial input (interactive shell), deterministic multi-vCPU via an MTF instruction-quantum round-robin |
| **Devices** | M20–M21, M23 | Deterministic virtio-blk disk over MSI-X (real Linux reads `/dev/vda`); deterministic virtio-net (real Linux answers a scripted ARP) |
| **Fault injection** | M22, M24 | Seeded, deterministic disk faults and network faults |
| **Flagship** | M25 | A real durability bug (`hydokv`'s ack-before-fsync) found by a seeded DROP fault and reproduced bit-for-bit: F1 no-fault OK (`62ae248b01bf3f70`), F2 seeded VIOLATION (`7ade5f37d16a347c`), F3 seed-sensitive (`832cc63372b1458d`) |

**The headline:** hvx already does, end-to-end, on real Linux, what the whole project set
out to do — *find a real bug through seeded fault injection, then reproduce it perfectly.*
Everything below is about widening that from one purpose-built 230-line store (`hydokv`) to
a real, multi-service, production-shaped stack. The mechanisms are done; the remaining work
is **scale, real software, and taming each real service's nondeterminism** — not new VMX.

---

## 3. The staircase

Each rung is a milestone-sized step. The ordering is by dependency: every rung stands on
the ones below it. "Requires" is concrete; "Risk" is the thing most likely to cost real
time.

### Rung 1 — M26: an unmodified real service under fault (SQLite) — *IN PROGRESS*

- **Goal.** Repeat the M25 find-and-reproduce loop on **code nobody wrote for the demo** —
  an off-the-shelf SQLite — so the surfaced durability bug is demonstrably pre-existing,
  not authored. This is DESIGN_NEXT's F4 stretch.
- **Requires.** (a) Raise `HVX_DISK_MAX` from 1 MiB — a one-constant, fingerprint-neutral
  bump (no VM that keeps a ≤1 MiB disk changes). (b) A **real filesystem** on the disk
  (ext4 / vfat): SQLite needs files, so `mkfs` + `mount`, and `mkfs` stamps a UUID +
  superblock/inode timestamps that must be **pinned/baked** or the fingerprint drifts. (c)
  A static `sqlite3` **CLI** built reproducibly (SQLite ships as a library; the shell binary
  must be obtained/built with the pinned musl toolchain). (d) The honest durability window —
  `PRAGMA synchronous=NORMAL` — so a seeded DROP fault surfaces a genuinely-lost commit.
- **Unlocks.** Credibility (the bug is in third-party code) **and** the
  **filesystem-determinism discipline** — pinned mkfs metadata, a mounted-rw filesystem
  whose writes stay byte-reproducible — that every larger-rootfs rung above depends on.
- **Key risk.** **Mounted-filesystem write nondeterminism.** A rw filesystem writes
  inode/superblock times and allocation metadata; these must be a pure function of the
  (deterministic) virtual clock. M16 proved the boot's clock/CRNG are bit-identical, so this
  is *expected* to hold — but it is unproven at the filesystem layer, and one un-pinned byte
  breaks the whole-image fingerprint. If it drifts, hunt the nondeterministic byte (the
  established boot-to-a-wall / diff-the-two-runs discipline).

### Rung 2 — a real single service, co-located and loopback-driven (Redis single-node durability)

- **Goal.** Run a real, off-the-shelf **network** service (Redis) as the L2 guest, drive it
  from an **in-guest client over `127.0.0.1`**, and surface its real durability window (the
  `appendfsync everysec` / ack-before-fsync gap) with a seeded DROP fault — reproduced
  bit-for-bit and branch-from-snapshot. This is the destination's shape, with one service.
- **Requires.** (a) A static **Redis** built reproducibly (larger than SQLite — jemalloc,
  more libc surface; pinned toolchain). (b) **Loopback networking inside the guest** — bring
  up `lo`; no virtio-net needed (this is the loopback-first thesis in miniature: the client
  and server talk over an in-kernel path that is already deterministic). (c) An in-guest
  **client + workload + oracle** (a tiny client or `redis-cli`) that records acked writes and
  checks read-committed-durability after a crash/recover — the M25 workload/oracle model,
  reused. (d) Enough RAM headroom (Redis + client + rootfs may press the 256 MiB cap).
- **Unlocks.** The **first loopback-driven real service** — proof, on real software, that
  "co-located over loopback is deterministic for free." The M25 `hvx_bug_verify` oracle/verify
  scaffolding carries over wholesale; the only new work is taming Redis.
- **Key risk.** **Service nondeterminism.** Redis touches time (key expiry, RDB/AOF
  timestamps → must map to the virtual TSC), `getrandom` (dict/hash seed → deterministic
  under hvx *if* the CRNG is, which M16 makes likely but the harness must *prove* via
  fingerprint equality), and **fork** (RDB snapshot / AOF rewrite runs in a child). On a
  single vCPU the fork child is serialized and thus deterministic in principle, but
  fork-time state duplication must reproduce exactly; if it doesn't, pin Redis to a
  no-background-save configuration for the first landing.

### Rung 3 — guest RAM + real rootfs scale-up

- **Goal.** Raise the physical ceiling so a multi-GB rootfs and multiple heavyweight
  services (a JVM, MySQL) fit. Today: **256 MiB RAM** (`MAX_GPAGES = 65536`) and a **single
  `kvmalloc`'d disk** (1 MiB, → a few MiB after M26). The stack above needs GBs of both.
- **Requires.** (a) Raise `MAX_GPAGES` past 256 MiB — the EPT already fans out page-by-page
  (one EPT PT per 512 pages) and guest RAM is already a `kvmalloc_array` of order-0 pages, so
  this is a quantitative scale-up, but the snapshot memcpy and the fingerprint now walk GBs.
  (b) Raise `HVX_DISK_MAX` to multi-GB — but the disk is one **contiguous** `kvmalloc`, which
  fails at GB scale, so the disk backing likely must become a **page array** like guest RAM (a
  real refactor of `HVX_SET/GET_DISK` and the fault path). (c) A real multi-GB rootfs (a real
  distro or a Nix/buildroot rootfs) staged **byte-reproducibly** — Rung 1's
  filesystem-determinism discipline at scale.
- **Unlocks.** The capacity for the JVM (Rung 5) and the full stack (Rung 6). Purely a
  scale-up, but a **hard prerequisite** for everything above it.
- **Key risk.** (a) **GB-scale contiguous disk `kvmalloc` fails** — the page-array refactor
  is the real cost here. (b) **Verify cost grows linearly** — the fingerprint hashes *all* of
  RAM + the whole disk on every one of the four checks, and the snapshot memcpy is GB-scale,
  so each verify run becomes minutes. (c) A real distro's boot is more instructions — still
  minutes under free-run, not astronomical, but every wall-decode iteration costs more.

### Rung 4 — multiple services co-located over loopback

- **Goal.** TWO+ real services in ONE guest talking over `127.0.0.1` (e.g. a service +
  Redis, or a service + MySQL), with the whole system deterministic and a seeded disk/clock
  fault on *any* service reproducing a whole-system bug bit-for-bit.
- **Requires.** (a) A rootfs carrying **both services + a deterministic launcher** — a
  minimal init or a scripted start (systemd is heavy and adds its own nondeterminism; prefer
  a scripted launch for the first landing). (b) **Deterministic startup ordering + readiness**
  — no un-mediated wall-clock retry loops; under hvx even a retry loop reproduces because the
  clock is deterministic, but the ordering must not depend on host timing. (c) Each service's
  nondeterminism tamed **simultaneously** (this compounds — every added service adds its own
  time/RNG/thread surface). (d) A **whole-system oracle** checking a cross-service invariant
  (e.g. a value acked by service A is durable in B).
- **Unlocks.** The destination's **core shape** — a multi-service stack whose inter-service
  traffic is deterministic for free, where a fault on any one service surfaces and reproduces
  a whole-system bug. This is where loopback-first pays off.
- **Key risk.** (a) **Compounding nondeterminism** — one un-tamed source in any service
  breaks the whole-system fingerprint, and there is no partial credit. (b) **Single-vCPU
  contention** — all services share one time-sliced core (see §5); a service that busy-waits
  or polls wastes the whole VM's time and slows every verify run. (c) Startup-ordering
  determinism under whatever init is chosen.

### Rung 5 — the JVM / Kafka

- **Goal.** Run a JVM-based service (Kafka, via a single-process KRaft mode to avoid a
  separate ZooKeeper) deterministically. The JVM is the single hardest nondeterminism surface
  in the whole stack.
- **Requires.** (a) A **JRE in the rootfs** (hundreds of MiB — needs Rung 3's scale-up). (b)
  **Taming JVM nondeterminism**, each source mapped: JIT compilation (background compiler
  threads, timing-driven → likely `-Xint` interpreter-only or a fixed compile policy for the
  first landing); GC (background threads → a deterministic collector such as SerialGC or
  Epsilon); `System.nanoTime`/`currentTimeMillis` (→ the virtual TSC, already deterministic);
  `SecureRandom`/`java.util.Random` (SecureRandom reads `/dev/urandom` → deterministic under
  hvx if the CRNG is — prove it); identity hash codes (address-dependent → deterministic
  under a deterministic allocator with no ASLR). (c) Kafka's own **log durability** (`acks=all`,
  `log.flush.*`) — which *is* the ack-before-fsync class, a natural M22 fault target.
- **Unlocks.** Proof that hvx tames even the JVM — the deepest taming challenge — and a
  marquee target (Kafka log durability + timing) running deterministically.
- **Key risk.** **JVM background threads + JIT + GC.** On a single vCPU these are serialized
  (one core is exactly what makes this tractable), but their
  scheduling interleaving must reproduce, and the JVM has *many* potential entropy sources —
  any one un-mediated breaks the fingerprint. Plus the JVM is slow to warm up under free-run
  and hungry for heap (RAM cap).

### Rung 6 — the full gRPC + Kafka + Redis + MySQL stack (the destination)

- **Goal.** All four services co-located in one deterministic L2 guest over loopback, with
  disk + clock fault injection applying to any of them, and any surfaced bug (durability,
  crash-consistency, timing) reproduced bit-for-bit + branch-from-snapshot.
- **Requires.** Everything above — GB rootfs, RAM/disk scale-up, JVM taming, the
  multi-service launcher — plus (a) **MySQL** built/tamed (InnoDB redo-log durability — another
  ack-before-fsync target); (b) the **gRPC** service + a client/workload driving it (HTTP/2
  over loopback — deterministic for free); (c) a **whole-system oracle** checking a
  cross-service invariant (e.g. a write acked by gRPC is durable in MySQL AND published to
  Kafka AND cached in Redis); (d) the fault schedule targeting any service's disk.
- **Unlocks.** **THE product** — whole-system determinism + snapshot/branch/replay +
  disk/clock fault injection + reproduction of durability/crash-consistency/timing bugs, on a
  real production-shaped stack.
- **Key risk.** **The sum of all the above.** Every service's nondeterminism must be tamed at
  once; scale (GBs of RAM + disk, long free-run boots, linear-cost fingerprints); and the
  single-vCPU constraint means the whole stack runs on one time-sliced core — realistic for a
  determinism testbed, not for throughput.

---

## 4. The long-term / parallel track (explicitly future)

Two large efforts sit *beside* the staircase, not on it. Both are deliberately deferred
until the loopback-first stack proves the loop on real software.

### 4a. Inter-node virtual network WITH fault injection

The destination faults **disk and clock**, never the loopback network (§1). Faulting the
network *between* services is the node-boundary model, and it requires the
multi-node topology hvx does not yet have:

- **What it is.** N **independent** L2 guests (each its own RAM / EPT / device model — hvx is
  one VM per fd today; M19 SMP is multiple vCPUs *sharing* one guest, which is a different
  thing) cross-connected through an **hvx-level deterministic, fault-injecting virtual
  switch**. Faults are **asymmetric per-direction** (drop / delay / corrupt / duplicate /
  reorder / one-way partition), applied at the **node boundary**.
- **What already exists toward it.** The M23 virtio-net device is the inter-node link, and the
  **M24 net-fault engine already implements per-direction (RX/TX) asymmetric
  drop/delay/corrupt/duplicate/reorder** keyed by a seeded PRNG — the fault *primitives* are
  done.
- **What is missing (the hard part).** The multi-guest **topology + scheduler**: a global
  instruction (or exit) **quantum** interleaving N guests deterministically, answering
  "whose turn delivers a frame, at what instant on the *receiver's* clock, how a global
  quantum composes with each guest's free-run cadence." This is DESIGN_NEXT's declined
  candidate 3 — a substantial new deterministic scheduler with subtle new determinism
  questions, a multi-milestone effort, and far more convincing once the single-guest stack
  proves the fault-injection-plus-reproduce loop catches real bugs.

### 4b. Autonomous discovery / fuzzing (the other half of the product)

The product is two halves: **(a) deterministic reproduction + fault injection** — which hvx
*does* today — and **(b) autonomous exploration of the state space** — which hvx does *not*
yet do.

- **What hvx does today.** You hand it a seed; it reproduces. The M25 "seed sweep" (F3) is a
  *manual* loop over a handful of seeds. Fault schedules are seeded and deterministic, but the
  seeds are chosen by a human.
- **What the autonomous half would add.** A **property-based workload generator** (à la
  FoundationDB workloads) that drives the system into new states; a **coverage / novelty
  signal**; and a **search over the seed + fault-schedule space** that *finds* the bug rather
  than being handed it. This turns hvx from "reproduce this failure" into "go find failures."
- **Why it's deferred.** It is a large research effort, and it is the *upper* half of the
  product — the deterministic substrate (which hvx has, end to end) is its prerequisite. Build
  the reproducible substrate on a real stack first; then automate the search over it.

---

## 5. Standing constraints (every rung inherits these)

These are not risks to be retired — they are the fixed shape of the platform, and every
rung above is designed around them.

- **Single-vCPU, free-run only — SMP is hardware-blocked.** The MTF instruction clock is
  exact but slow (one VM-exit per instruction, ~10^8 for a boot), so deterministic SMP (M19)
  is fine for small hand-written guests but far too slow to boot real multi-core Linux. The
  fast (PMU) instruction clock that would fix this is **BLOCKED at the hardware level (M17)**:
  the L0 CPU is a hybrid i7-13700HX, KVM hardcodes guest-PMU-off on hybrid silicon, the nested
  L1 has no architectural PMU at all, and it is **not a config toggle** (reloading `kvm` with
  `enable_pmu=1` leaves it `N`). So **the whole stack runs single-vCPU in FAST free-run**
  (`HVX_RUN`, no single-step) — which is exactly the single-core model: one
  time-sliced core with a deterministic clock time-slices all services deterministically. This
  is a feature, not a workaround, but it caps throughput and means any busy-waiting service
  taxes the whole VM.
- **Free-run speed is the operating mode.** Everything runs at native-ish free-run speed and
  is deterministic per M16 — verify runs are minutes, not the astronomical single-step path.
  Instruction-exact **divergence bisection** stays out of reach (needs the M17-blocked fast
  clock) and is **moot** here anyway: free-run is already fully deterministic, so there is no
  divergence to bisect.
- **Byte-reproducible builds are load-bearing on every rung.** The initramfs / rootfs / every
  binary is loaded into **hashed guest RAM**, and the disk into the **hashed disk image**, so
  any byte of build drift moves the fingerprint. The discipline — pinned toolchain store paths
  (busybox, musl), fixed mtimes, sorted names, `gzip -n`, and (from Rung 1) pinned mkfs
  UUID/timestamps — must extend to every new binary and rootfs. This has already bitten twice
  (the busybox store-path pin drifted as the shared box's nix store churned), so treat it as
  recurring maintenance, not a one-time setup.
- **The nested-L1 safety model is absolute.** All hvx work happens inside a throwaway nested
  L1 (L0 = host, never touched; L1 = our root VM; L2 = the guest). We have no host root, so
  this is enforced by construction: never `insmod`/`sudo` on the host. The box is **shared**
  with unrelated `qemu-system` TCG jobs — **scoped kills only** (`pkill -f l1-hvx`, never a
  broad `pkill qemu-system`). Restart L1 on any wedge via `run-l1.sh`. For these
  userspace-heavy rungs, expect **boot-to-a-wall / decode-the-fault / fix-the-rootfs**
  iterations (module load order, `O_DIRECT` alignment, filesystem metadata), not VMX debugging.

---

## 6. One-line summary of the climb

M25 proved the loop on one 230-line store. **M26** proves it on unmodified SQLite. Then the
climb is: **one real loopback service (Redis) → scale RAM+disk+rootfs → several services over
loopback → the JVM/Kafka → the full gRPC+Kafka+Redis+MySQL stack** — each rung a real service
and its tamed nondeterminism, none of it new VMX. The network-partition faults *between*
nodes, and the autonomous *search* for bugs, are the two big parallel tracks that come after
the loopback stack proves itself.

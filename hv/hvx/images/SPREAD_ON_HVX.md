# spread on hvx — port plan (deterministic consistency fuzzing of a real service)

Goal: run the unmodified `spread` queue (a self-hosted SQS-FIFO queue at
`~/spread`; multi-broker, brokers coordinate only through a shared backend) ON hvx, then port the
consistency-fuzzing *upper half* that the sibling `repeat` project built (structure-aware workload
generator + black-box mini-Jepsen oracle + search over seed/fault schedules). hvx is the substrate
(fast, real Linux, real dynamically-linked services, seeded disk/clock fault injection,
snapshot/branch/replay); it explicitly lacks that upper half (ROADMAP §4b). This is that port.

## Why the templates are M30 + M31/M32 (not Redis)
spread is a **dynamically-linked Rust binary with a glibc/nix closure (~100 MB)** and runs as
**multiple co-located processes over `127.0.0.1` loopback**. So the models are:
- **M31/M32** — a real dynamically-linked glibc **nix-closure** service (OpenJDK JRE) staged on an
  **ext4 read-only root** at identical `/nix/store/…` paths (zero patching), booted deterministically.
  Requires `HVX_VM_X86_64_V2` (nixpkgs userland is ≥ x86-64-v2; the synthetic CPUID otherwise makes
  glibc abort "CPU ISA level is lower than required").
- **M30** — a multi-process launcher over loopback (start each `&`, poll readiness, run client).
- Redis (M27) is *statically* linked and single-process — the wrong template for the closure/multi-proc parts.

## What we REUSE from `repeat` (already built, carries over)
- **`spread-drv`** (`~/repeat/harness/spread-drv/main.go`) — a **static** (`CGO_ENABLED=0`) Go HTTP
  client **and** consistency oracle: drives sends/receives/nacks over HTTP, and checks at-least-once
  / FIFO / no-split-brain / no-phantom, printing `BUG-ASSERT`/`RPT sat`/metric lines. This IS the
  "in-guest HTTP driver + oracle" the recon flagged as missing. It runs as a static ELF in the guest.
- The **op-program workload grammar** (`propose_spread_seq` in `~/repeat/harness/src/main.rs`) — S/R/N/P
  send/receive/nack/concurrent + fault ops. Port the generator; the oracle logic already lives in spread-drv.
- The **metric-gated drain** discipline (drain until `spread_queue_messages==0`, not empty-receive
  guessing) — hard-won; keep it. See `~/repeat/harness/notes/spread-native-tier.md`.
- `TOKIO_WORKER_THREADS=1`, `randomize_va_space=0` — determinism preconditions we already apply.

## Fault model on hvx (better than repeat's SIGKILL/SIGSTOP)
- Co-located brokers on the **`fs` backend** coordinate through a shared directory on a **rw data
  disk**. hvx's **seeded disk-fault engine** (M22/M26: DELAY/ERROR/DROP/REORDER/TORN/DROP_UNFLUSHED,
  pure fn of `(seed,index)`) faults THAT disk — directly testing durability/crash-consistency of the
  shared backend. This replaces repeat's process-signal faults for the durability class.
- Process crash/pause faults (broker kill) are still doable guest-internally (the `/init` launcher or
  the driver can `kill` a broker pid; deterministic on single vCPU). Loopback is never faulted by hvx
  (by design — partitions are node-boundary), which matches: same-guest brokers can't be partitioned.

## Staged plan

### Phase 1 — one `fs`-backed broker, deterministic on hvx  ← START HERE
Smallest testable target (project's F0→F1→F2 discipline). Prove every new-for-spread piece in isolation:
Rust nix-closure staging, `HVX_VM_X86_64_V2`, `hvx_seedrng` (Rust `getrandom` hangs on the jitter-free
CRNG otherwise), an in-guest HTTP driver, loopback, the `fs` backend on a rw disk, whole-boot determinism.
- Build a **spread rootfs** the M31 way: ext4 **ro-root** carrying `spread`'s full closure
  (`nix-store -qR $(realpath ~/spread/target/release/spread) | cp -a … /nix/store/…`) + a small ext2
  **rw data disk** for `--data-dir`. Snapshot excludes the ro-root closure (keeps fingerprint small).
- `/init` (or `/sbin/init`): run `hvx_seedrng` once; `ifconfig lo 127.0.0.1 up`; export
  `TOKIO_WORKER_THREADS=1`; `randomize_va_space=0`; start `spread --backend fs --data-dir /data
  --listen 127.0.0.1:8080 &`; poll `/healthz`; run the in-guest driver (`spread-drv`, static Go) to
  send a few messages and receive them; print `HYDO-ORACLE: OK|VIOLATION …` + a stop marker.
- Boot: `hvx_linux <bzImage> <initramfs>` with `HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT`, the rw disk
  via `HVX_DISK=`, kernel cmdline selecting the phase.
- **Success = whole-boot determinism**: two boots → identical fingerprint; round-trip (snapshot→restore
  identical); branch-from-snapshot identical. (Adapt `hvx_redis_verify.c`, or a new `hvx_spread_verify.c`.)
- Watch: byte-reproducible rootfs (pinned store paths, fixed mtimes, `mkext*` UUID/time pinning);
  tokio thread interleaving (pin to 1 worker first); idle-heartbeat for `accept`/`epoll_wait` on lo.

### Phase 2 — two brokers + the consistency oracle + seeded disk faults
- Second broker sharing the same rw data disk (`fs` backend) — the M30 launcher shape. Multi-broker
  coordination through the shared log; loopback for the client.
- Drive with `spread-drv` op-programs; assert the mini-Jepsen invariants. Apply hvx **seeded disk
  faults** on the shared backend disk; a surfaced durability/consistency bug must replay + branch bit-for-bit.
- Success = a fault-free clean run + a seeded-fault run whose verdict + fingerprint reproduce exactly.

### Phase 3 — the autonomous search (the upper half)
- Feed a **distinct workload per run** without rebuilding the rootfs. Options (recon §2): vary the
  **kernel cmdline** per boot (only `boot_params` rebuilt), or stream bytes over **`HVX_PUT_INPUT`**
  (transient UART input ring, snapshot-excluded). A per-restore snapshot loop (`repeat`'s `snap-run`
  shape) is constructible via `HVX_GET/SET_SNAPSHOT` + disk save/restore. **Gap: hvx has no
  synchronous one-shot fuzz-input port (repeat's fw_cfg); build a small one if driving per-restore.**
- Search over `(workload seed, fault seed, fault schedule)` guided by the oracle + reachability markers
  + spread's **in-code invariant assertions** (a tripped `debug_assert` → a crash hvx reproduces for free).
- **Open gap — coverage signal:** hvx has NO edge/coverage bitmap (repeat's TCG plugin has no KVM
  equivalent; the fast PMU clock is hardware-blocked, M17). Defer coverage-guided search; start with
  oracle+reachability+assertion-driven search. Revisit a coverage source (guest KCOV, or novelty from
  the state fingerprint / exit stats) later.

## Build machine workflow (all on the build machine; hvx runs in a nested L1)
- Sync the Mac's tree to the build machine: `cd ~/mine/rust/my_projects/hydo && ./dev.sh "<cmd>"` (rsync, excludes target/build/.git; runs on the build machine, CC=clang).
- L1 (the nested dev VM) is UP (`~/hydo/hv/hvx/run-l1.sh` on the build machine, idempotent = wedge recovery; template needed staging to `~/l1/cloud-init/`). L1 is reached through its forwarded port 2222 with the key `~/l1/id_l1`.
- Build rootfs/kernel: `./dev.sh "hv/hvx/images/build-linux.sh"` → `~/hydo/build/{bzImage, initramfs-*.cpio.gz, *-disk.img}`.
- Build + load hvx: `~/hydo/hv/hvx/deploy-hvx.sh [load]` (on the build machine → into L1).
- **Safety: scoped kills only — `pkill -f l1-hvx`, NEVER a broad `pkill qemu-system` (a sibling `repeat` TCG job shares this machine). Never touch L0.**

## Status
- 2026-08-23: recon complete; L1 up + verified (gcc 13, headers, /dev/kvm, ~3.5 GB free).
- 2026-08-23: **Phase 1 DONE — boot+serve + full determinism (replay + both stretch goals).**
  ONE unmodified `spread` broker (`fs` backend) boots under hvx on real Linux, is driven by the
  in-guest static-Go driver+oracle, and serves a create/send×2/receive/FIFO workload:
  `HYDO-ORACLE: OK sent=2 received=2 fifo=ok`. Whole-boot fingerprint (guest RAM + rw data disk +
  console) is **`36fd5ff53bb262ab`**, IDENTICAL across all three checks:
  REPLAY (two fresh boots), ROUND-TRIP (snapshot→restore in place), and BRANCH (snapshot at a
  pre-initramfs kernel marker, continue twice). ro-root hash `4530f4220ed0c473`, EXCLUDED from the
  snapshot + stable. Rootfs is byte-reproducible (sha256(spread-disk.img)=`32607b20…`,
  sha256(spread-ext4.img)=`a195dbb6…` identical across independent builds).
  - Files added to the hydo tree: `hv/hvx/images/spread-drv/{main.go,go.mod}` (the repeat driver + a new
    single-broker `smoke` oracle subcommand); the spread `§2m` block in `build-linux.sh`;
    `build-spread.sh` (fast isolated builder that evals `§2m` — single source of truth);
    `hv/hvx/hvx_spread_verify.c` (clone of `hvx_jvm_verify.c`, `FINAL_MARKER` retargeted);
    `hv/hvx/run-spread-demo.sh`; `hv/hvx/images/native-spread-smoke.sh` (F0 native sanity); `hvx_spread_verify`
    wired into `hv/hvx/deploy-hvx.sh`.
  - Reproduce (on the build machine): `cd ~/hydo && hv/hvx/images/build-spread.sh` → `bash hv/hvx/deploy-hvx.sh` →
    `bash hv/hvx/run-spread-demo.sh all` (serve + replay + roundtrip + branch; `seed` mode is skipped,
    it is JVM-key-oracle-specific — Phase 2 adds a spread fault oracle).
  - Determinism keys that held: `hvx_seedrng` (else Rust `getrandom()` hangs), `HVX_VM_X86_64_V2`
    (glibc userland), `HVX_VM_IDLE_HEARTBEAT` (broker parks in accept/epoll), `TOKIO_WORKER_THREADS=1`,
    `randomize_va_space=0`, loopback up. spread's entropy-derived `writer_id`/message-ids are
    reproducible because the CRNG + virtual clock are deterministic (the fingerprint proves it).
  - Phase 2 next (two brokers + consistency oracle + seeded disk faults on the shared `fs` data disk).
- 2026-08-23: **Phase 2 DONE — two brokers + the mini-Jepsen oracle + seeded disk faults, all
  deterministic.** TWO co-located `fs`-backed brokers (B1 :8080, B2 :8081) SHARE one rw ext2 data
  disk, coordinate through the shared directory log, and are driven by the full `spread-drv`
  op-program runner (metric-gated drain + at-least-once/FIFO/no-split-brain/no-phantom):
  `HYDO-ORACLE: OK consistent 2brokers`. Whole-boot fingerprint **`7b576827407c0644`** IDENTICAL
  across REPLAY + ROUND-TRIP + BRANCH; ro-root `81203333c65e3342` stable/excluded. Seeded hvx DISK
  DROP fault (30%) on the shared data disk REPRODUCES bit-for-bit (seedA `9b2b4ef8f30e2a66` twice)
  and is seed-SENSITIVE (seedB `3f966b6255c45996`), distinct from the clean baseline — deterministic
  under fault. Details + the crash-op stall finding: `hv/hvx/images/SPREAD_PHASE2_NOTES.md`. Files added:
  `build-linux.sh` §2n, `build-spread2.sh`, `hv/hvx/run-spread2-demo.sh`, `hv/hvx/images/native-spread2-smoke.sh`,
  `fault` mode in `hv/hvx/hvx_spread_verify.c`. Reproduce: `hv/hvx/images/build-spread2.sh` -> `bash hv/hvx/deploy-hvx.sh`
  -> `bash hv/hvx/run-spread2-demo.sh all`.
  - FINDING: the broker-crash op `X 2` stalls the fs-backend survivor (busy-spins waiting out the
    dead broker's shared-log lease -> the metric-gated drain never drains -> a ~4 s boot runs
    indefinitely at 100% vCPU). So the crash op is OFF by default (`spread_crash=0`), opt-in via the
    cmdline token for a future failover experiment that first bounds the lease/drain. Not a harness
    bug — spread's fs failover behavior.
  - Phase 3 next (the autonomous search: per-run workload/fault-seed variation + oracle/assertion-guided search).
- 2026-08-23: **Phase 3 COMPLETE — the deterministic snapshot-restore search loop runs.** hydo-16
  fixed the deep-restore wall (fuzz-port/virtio-blk-doorbell collision, fingerprint-neutral); I
  resolved the determinism mismatch by fingerprinting the finding-relevant state (console = oracle
  output + p2 = durable queue/log), excluding volatile kernel RAM (RAM-diff proved the only delta was
  8 bytes of kernel printk/scheduler bookkeeping; console + p2 byte-identical). Proofs: same program
  -> IDENTICAL fp; distinct programs -> distinct fps; Phase-2 baseline still `7b576827407c0644`.
  First campaigns: no-fault 30 programs -> 30/30 consistent (avg 0.247 s/input); seeded DROP fault
  (0x51a0, 30%) 30 programs -> 30/30 consistent (avg 0.245 s), deterministic under fault. Per-input
  ~0.25 s (~16x vs a ~4 s boot). Details: `hv/hvx/images/SPREAD_PHASE3_NOTES.md` (top). NB: `hvx.c`/`hvx_abi.h`
  are hydo-16's (the fuzz port + collision fix); my work is the harness/driver/rootfs (§2o,
  `hvx_spread_search.c`, `fuzz-read.c`, `run-spread3-search.sh`).
- 2026-08-23: (superseded) **Phase 3 — machine BUILT, BLOCKED on a missing hvx primitive (the plan's noted gap).**
  Built the whole serial-fed snapshot-restore search loop: driver `looprun` (serial-fed op-program +
  `SPREAD-READY-FOR-INPUT`/`SPREAD-RUN-DONE` brackets), `/init spread_mode=search`, and the host harness
  `hv/hvx/hvx_spread_search.c` (boot-once-to-READY -> file-backed snapshot -> per-program restore + feed +
  run + verdict-parse + fingerprint, with a seeded `propose_spread_seq` generator + a determinism proof
  + optional DROP fault). Native F0 PASSES. Under hvx the loop is **blocked**: with the idle heartbeat
  ON (which the workload's metric-gated drain REQUIRES — its 500 ms timer-sleeps only fire if the clock
  advances on idle), `HVX_RUN` never returns while the guest is permanently idle on the serial input
  read — confirmed by instrumentation (`HVX_RUN` returns once at it=0/THROTTLE then the 2nd call never
  returns). So the host loop cannot interleave draining the READY marker + feeding the program. Heartbeat
  OFF returns from idle but stalls the drain. A guest busy-poll workaround passes natively but did not
  clear it under hvx. Exactly the plan's gap ("hvx has no synchronous one-shot fuzz-input port; build a
  small one"). **Fix = a synchronous input port (fw_cfg-equivalent: ~1 ioctl + 1 port handler in hvx.c +
  a ~30-line guest reader); everything else built plugs straight in.** Diagnosis + port design:
  `hv/hvx/images/SPREAD_PHASE3_NOTES.md`.

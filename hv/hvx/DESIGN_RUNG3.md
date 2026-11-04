# DESIGN — Rung 3: RAM + disk + real-rootfs scale-up

Design spec for **Rung 3 of `hv/hvx/ROADMAP.md`** — raising the physical ceiling (guest RAM
past 256 MiB, backing disk past 64 MiB) and moving from an in-RAM initramfs to a real
multi-GB ext4 root disk, so heavier services (a JVM, MySQL) and *multiple co-located*
services fit on the loopback-first staircase. Read-only design pass; grounded in `hv/hvx/hvx.c`,
`hv/hvx/hvx_abi.h`, `hv/hvx/hvx_boot.h`, `hv/hvx/hvx_disk_boot_verify.c`, `hv/hvx/ARCHITECTURE.md`,
`hv/hvx/ROADMAP.md`, and `CLAUDE.md` (M1–M27, esp. **M11** — the guest-RAM page-array refactor,
which is the template for the disk refactor here). Written in the style of
`hv/hvx/PHASE6_DESIGN.md` / `hv/hvx/DESIGN_NEXT.md`.

Status anchor: treat the codebase as complete through **M27**. Two agents work concurrently:
a harness-hardening agent editing `build-linux.sh` / `hvx.c` (the small-image mkfs-pinning
fix — Rung 1's ext2/SQLite discipline), and this design pass (docs only). **This spec touches
no source file.** Where it extends the harness agent's mkfs-pinning work to multi-GB ext4,
that is called out explicitly (§3.4, §4) so the two efforts compose rather than collide.

---

## TL;DR — the recommendation

Rung 3 is a **scale-up, not new VMX** — but it is not a pure constant bump. Three concrete
structural facts in the code turn "raise two `#define`s" into a real milestone:

1. **The disk is one contiguous `kvmalloc`** (`struct hvx_vblk.disk`, `hvx.c:438`; cap
   `HVX_DISK_MAX = 64 << 20`, `hvx_abi.h:435`). A GB-scale image cannot be one physically- or
   even virtually-contiguous allocation reliably, and every `HVX_SET/GET_DISK` copies the
   *whole* image through `copy_from/to_user`. **The load-bearing piece is refactoring the disk
   to a page array**, exactly as M11 did for guest RAM, plus adding a **disk `mmap`** so the
   GB-scale image is loaded and fingerprinted zero-copy instead of double-buffered.

2. **The EPT top structure is three fixed pages** (`ept_pg[0..2]` = PML4/PDPT/PD,
   `build_ept`, `hvx.c:1054`) with a **single EPT PD**, so it addresses **at most 512 EPT PT
   pages = 1 GiB**. `MAX_GPAGES = 65536` (256 MiB) hides this today. Raising RAM past 1 GiB
   requires the EPT PD level to **fan out** to multiple PD pages hung off multiple PDPT
   entries — a small dynamic-allocation refactor mirroring the disk one.

3. **Past ~3.9 GiB, guest RAM collides with the MMIO hole.** The IOAPIC (`0xfec00000`), LAPIC
   (`0xfee00000`) and virtio BARs (`0xfe000000`/`0xfe020000`) live in the 4th GiB and are
   **deliberately EPT-unmapped** so accesses trap (`ARCHITECTURE.md:183`, `hvx.c` EPT-violation
   decoders). A flat guest-RAM GPA space that reaches `0xfe000000` would back those GPAs with
   RAM and silently break every MMIO trap. Beyond ~3 GiB the RAM must **split around the hole**
   (low RAM below it, high RAM above 4 GiB) with a matching **split e820** — a real, but
   bounded and well-understood (this is exactly how a real PC lays out memory), change.

**Recommended landing:** do the disk page-array refactor + disk mmap (unblocks a multi-GB
data disk and a multi-GB *rootfs* disk at once), raise RAM to **1 GiB with the single-PD
structure untouched** (a pure `MAX_GPAGES` bump — fingerprint-neutral), then fan out the EPT
PD to reach **~3 GiB** (enough for Redis + a client + a real rootfs, and for the first JVM
experiments), and boot a **real ext4 root disk via `switch_root`**. Defer the >3.9 GiB
PCI-hole split to a follow-on rung (it is only needed once a single service wants ≥4 GiB of
RAM — the JVM at Rung 5, not Rung 3). Keep everything in **fast free-run** (`HVX_RUN`), the
standing operating mode (`ROADMAP.md §5`).

The determinism budget is the hard part, and it is mostly about **cost, not correctness**:
mkfs at scale (a journal is a new reproducibility surface), fingerprinting GBs on every one
of the four verify checks, and snapshotting GB-scale RAM+disk when **L1 has only 4 GiB total**
(`run-l1.sh:81`, `-m 4096`). The L1 RAM ceiling is the single item that **needs an L0-side
change to confirm** and is flagged throughout.

---

## 1. Goal and scope

**Goal.** Raise the platform ceiling so the rungs above fit:

- **Guest RAM** from 256 MiB toward multiple GB (target this rung: **up to ~3 GiB**, the
  no-PCI-hole regime).
- **Backing disk** from 64 MiB toward multiple GB, via a page-array backing that scales.
- **Rootfs** from an in-RAM initramfs (which must fit under the 1 GiB identity-map cap,
  `hvx_boot.h:107` `IDENTITY_MAP_CAP`) to a **real ext4 root disk** on `/dev/vda`, so a real
  distro / buildroot / Nix rootfs carrying heavyweight services + their dependencies fits.
- Keep the whole thing **bit-for-bit deterministic** and the **verify harness tractable**
  (minutes, not hours) at the new scale.

**Non-goals for this rung** (deferred, with the trigger that pulls each one in):

- The **>3.9 GiB PCI-hole split** (needed only when one service wants ≥4 GiB RAM — the JVM,
  Rung 5). Designed here (§3.2.3) but not landed.
- Any **fast-instruction-clock / single-step** work — hardware-blocked (M17), and moot under
  free-run determinism.
- **Multi-VM** topology, **network-partition** faults — the parallel track (`ROADMAP.md §4`).
- New VMX controls or run-loop changes. If a phase appears to need one, stop and reconsider
  (same discipline as `DESIGN_NEXT.md §7`).

---

## 2. The load-bearing decisions

Four decisions shape the whole rung. Get them right and the rest is plumbing.

### 2.1 The disk becomes a page array — mirror M11, and add a disk `mmap`

This is the piece the roadmap already flags (`ROADMAP.md` Rung 3, risk (a)). Today
(`hvx.c:437-439`):

```c
struct hvx_vblk {
    void *disk;        /* backing image (kvmalloc); NULL => device absent */
    u64   disk_size;   /* image size in bytes */
```

`kvmalloc` falls back to `vmalloc`, so 64 MiB works today, but a GB-scale single allocation is
fragile (large vmalloc under memory pressure fails; and every `HVX_SET/GET_DISK` moves the
*entire* image through `copy_from/to_user`, `hvx.c:4752`, `:4796`). **The M11 lesson is
authoritative** (`CLAUDE.md` M11; `ARCHITECTURE.md:163`): guest RAM was refactored from one
high-order block to `kvmalloc_array` of **individually-allocated order-0 pages** precisely
because tail pages of a non-compound allocation are not individually refcounted, which
corrupts page state (`BUG: Bad page state`) when the mmap fault handler hands a page to
userspace. The disk must follow the same recipe.

**Decision.** Back the disk with `unsigned long *disk_pages` (a `kvmalloc_array` of order-0
`__get_free_page` pointers) + `unsigned int n_disk_pages`, exactly parallel to
`guest_pages[]`/`n_guest_pages`. Add a **disk `mmap` window** (a fault handler indexing
`disk_pages[idx]`, mirroring `hvx_vm_fault`, `hvx.c:4260`) so userspace loads the rootfs into
the disk and fingerprints it **zero-copy** — never a GB-scale `copy_*_user`. This is the same
double-win M11 got for RAM.

### 2.2 RAM scale-up is a bump to 1 GiB, then an EPT-PD fan-out to ~3 GiB

`MAX_GPAGES = 65536` (`hvx.c:363`) is a cap; a VM created at 256 MiB stays byte-identical, so
raising the cap is **fingerprint-neutral** for every existing VM (the M16/M25/M26 baselines are
preserved — nothing that keeps its `mem_size` changes). But the EPT structure (`build_ept`,
`hvx.c:1054`) uses **one PD page** (`ept_pg[2]`, 512 entries), so it tops out at 512 PT pages =
**1 GiB**. Two-step decision:

- **Step 1 (to 1 GiB):** raise `MAX_GPAGES` to `262144` (1 GiB). No structural change — the
  single PD already holds up to 512 PT entries. `n_ept_pt = ceil(n/512)` reaches 512, exactly
  filling the PD.
- **Step 2 (to ~3 GiB):** the EPT PD must **fan out**. Replace the fixed `ept_pg[0..2]` with a
  dynamically-sized PD level: `n_ept_pd = ceil(n_ept_pt / 512)` PD pages, wired into that many
  PDPT entries (`ept_pg[1]` stays the single PDPT; entries `[0..n_ept_pd)` point at the PD
  pages). This is the identical "array of order-0 pages hung off the next level up" pattern as
  the disk and RAM refactors — small, local, and testable in isolation.

Keep **4 KiB EPT leaves throughout** (the roadmap and M11 both note guest RAM is deliberately
non-contiguous order-0 pages, which 2 MiB EPT leaves cannot map — `CLAUDE.md` M11). The cost
is more EPT PT pages (768 PTs for 3 GiB, ~6 MiB of page tables) — negligible.

### 2.3 A real ext4 root disk via `switch_root`, not a bigger initramfs

The initramfs is loaded into **hashed guest RAM** and must fit under `IDENTITY_MAP_CAP` = 1 GiB
(`hvx_boot.h:430-436`). That is the wrong vehicle for a multi-GB rootfs (it would consume RAM,
cap at 1 GiB, and force every rootfs byte through the RAM fingerprint). **Decision:** keep a
*tiny* initramfs (the existing M21 module-loading `/init`) and have it **mount the real ext4
root off `/dev/vda` and `switch_root` into it**. The heavy rootfs then lives on the **disk**
(hashed as the disk image, which we are already making a scalable page array), not in RAM. The
cmdline gains `root=/dev/vda rootfstype=ext4 rootflags=...`. This reuses the proven M21 virtio
disk-bind path unchanged and matches how a real distro boots.

### 2.4 Determinism at scale is a cost problem solved by mmap + dirty-tracking

Correctness is *expected* to hold — M16 proved the entire boot (clock + CRNG) is bit-identical,
and Rung 1 is proving a *mounted rw filesystem* stays byte-reproducible right now. The new
problem is **cost**: the fingerprint FNV-hashes all of RAM + the whole disk on **each of the
four verify checks** (`hvx_disk_boot_verify.c:214-223`), and a snapshot double-buffers RAM+disk
in host RAM. At GB scale both grow linearly and the snapshot buffer can **exceed L1's 4 GiB**.
The decisions:

- **Fingerprint via `mmap` for both RAM and disk** (RAM already is; §2.1 adds it for the disk),
  so hashing is zero-copy over the page cache, not a `copy_to_user` into a GB scratch buffer.
- **Optionally hash only the touched subset** (§3.4.2) — most of a multi-GB guest RAM stays the
  zero page during a boot; a determinism-preserving "hash a canonical zero-baseline once, then
  fold in only written pages" scheme keeps verify sub-linear in *allocated* rather than *sized*
  memory. This is a **harness** optimization (no kernel change) and is optional for the landing.
- **Snapshot/branch feasibility is gated by L1 RAM** (§3.4.3). Naive double-buffering of a
  4 GiB guest + 4 GiB disk needs ~8 GiB of save buffers **plus** the live VM — impossible in a
  4 GiB L1. This is the one item that **needs an L0-side change** (raise `-m` in `run-l1.sh`) or
  a smarter snapshot (file-backed or COW).

---

## 3. Mechanism, in detail

### 3.1 Disk-backing refactor — the load-bearing piece

**Current shape (all flat-buffer):**

| Site | Today | Change |
|---|---|---|
| `struct hvx_vblk` (`hvx.c:438`) | `void *disk; u64 disk_size;` | add `unsigned long *disk_pages; unsigned int n_disk_pages;` (keep `disk_size` for the exact byte length; the last page is partial) |
| `HVX_SET_DISK` (`hvx.c:4713`) | `kvmalloc(dk.len)` + one `copy_from_user` | allocate `ceil(len/PAGE)` order-0 pages into `disk_pages[]`; either page-wise `copy_from_user`, **or** (preferred) let userspace fill the disk via the new `mmap` and pass only `len` |
| `HVX_GET_DISK` (`hvx.c:4788`) | one `copy_to_user` of `disk` | page-wise `copy_to_user`, **or** obsolete it for the harness in favor of the disk `mmap` |
| `vblk_disk_rw` (`hvx.c:1785`) | `hvx_guest_rw(vm, gpa, (u8*)disk + doff, len, …)` | split `[doff, doff+len)` at page boundaries; per fragment copy to/from `disk_pages[doff>>12] + (doff & 0xfff)` |
| write-cache commit `vblk_unsync_commit` (`hvx.c:1875`) | `memcpy((u8*)disk + doff, …)` | same page-split write into `disk_pages[]` |
| free path (`hvx.c:4074`) | `kvfree(disk)` | free each `disk_pages[i]`, then `kvfree(disk_pages)` — mirror `hvx_vm_free_pages`'s guest-page loop (`hvx.c:4085`) |

**The page-indexing helper.** The cleanest refactor introduces one helper that both
`vblk_disk_rw` and `vblk_unsync_commit` call, symmetric to `hvx_guest_rw` (`hvx.c:1760`):

```
disk_rw(vm, doff, buf, len, to_disk):
    for each page-aligned fragment of [doff, doff+len):
        page = disk_pages[doff >> 12]; off = doff & (PAGE-1); n = min(len, PAGE-off)
        memcpy(to_disk ? page+off <- buf : buf <- page+off, n)
```

`vblk_disk_rw` then does two page-indexed copies (guest RAM via `hvx_guest_rw`, disk via
`disk_rw`) with a small bounce for the cross-boundary case, or — cleaner — copies directly
guest-page ↔ disk-page since both sides are now page arrays. Keep the existing bounds checks
(`doff > disk_size`, `doff+len > disk_size`, `hvx.c:1790`) verbatim; only the backing indexing
changes. **Nothing about the deterministic completion timing, the seeded fault decisions
(`vblk_fault_decide`), or the MSI-X path changes** — those operate on `(doff, len, req_seq)`,
not on how bytes are stored, so every M20–M26 fingerprint is preserved by construction.

**The disk `mmap`.** Guest RAM mmaps at `vm_pgoff == 0` and bounds to `n_guest_pages`
(`hvx_mmap`, `hvx.c:4279`). Reserve a **disjoint pgoff window for the disk** — e.g. the disk
maps at `vm_pgoff >= HVX_DISK_MMAP_PGOFF` (a large fixed sentinel, well above any real RAM page
index), and `hvx_mmap` dispatches on the range: `pgoff < n_guest_pages` → RAM (`hvx_vm_ops`),
`pgoff >= HVX_DISK_MMAP_PGOFF` → a new `hvx_disk_ops` whose `.fault` indexes
`disk_pages[pgoff - HVX_DISK_MMAP_PGOFF]` (byte-for-byte the `hvx_vm_fault` pattern with
`get_page`, `hvx.c:4269`). The harness then:

- **loads** the rootfs by `mmap`ing the disk window and writing the ext4 image in (no
  `HVX_SET_DISK` copy for the bulk — `HVX_SET_DISK` shrinks to "allocate `len` pages + set
  `disk_size` + do the one-time `pdpt[3]` idmap poke for hand-written guests, `hvx.c:4768`");
- **fingerprints** the disk by hashing the mmap in place (no `diskbuf` scratch, no
  `HVX_GET_DISK` copy — kills the GB-scale double-buffer in `hvx_disk_boot_verify.c:63,218`);
- **snapshots** the disk by `memcpy(save, disk_mmap, disk_size)` and restores by the inverse —
  the same shape M7/M16 use for RAM, and the same L1-RAM caveat (§3.4.3).

`HVX_GET/SET_DISK` remain for compatibility (small disks, hand-written tests) but are no longer
on the GB-scale hot path.

**Write-cache interaction (M26).** The volatile write cache (`unsync_data`, `hvx.c:500`) is a
separate small buffer (`HVX_UNSYNC_MAX * HVX_UNSYNC_ENTBYTES` = 256 × 8192 = 2 MiB) and is
**unaffected in size** — it holds at most 256 un-flushed writes, independent of disk size. Only
its **commit target** changes: `vblk_unsync_commit` (`hvx.c:1884`) must write through the new
page-split `disk_rw` instead of `memcpy((u8*)disk + doff, …)`. `vblk_unsync_stash` /
`_overlay` / `_invalidate` are unchanged (they copy guest RAM ↔ the cache, not the disk).
Because the cache is empty at every flush/snapshot boundary (M26 note, `hvx.c:495`), the
page-array disk backing does not perturb the flush-aware DROP semantics.

**Cap.** Raise `HVX_DISK_MAX` (`hvx_abi.h:435`) to a multi-GB value (e.g. `4UL << 30` or
`8UL << 30`). Fingerprint-neutral: `HVX_SET_DISK` still allocates exactly `dk.len` (rounded to
pages), and the fingerprint hashes `disk_size` bytes — every VM keeping a ≤64 MiB disk is
byte-identical (the same argument the M26 note makes for the 1 MiB→64 MiB bump, `hvx_abi.h:430`).

### 3.2 Guest RAM scale-up

#### 3.2.1 To 1 GiB — a pure cap bump (fingerprint-neutral)

Raise `MAX_GPAGES` from `65536` to `262144`. `create_vm` (`hvx.c:4162`) already computes
`n = ceil(mem_size/PAGE)`, `npt = ceil(n/512)`, allocates `guest_pages[]` and `ept_pt[]` as
`kvmalloc_array`s, and page-populates them (`hvx.c:4175-4194`); `build_ept` already loops
`k < n_ept_pt` filling `l2[k]` (`hvx.c:1062`). At 1 GiB, `n_ept_pt = 512` exactly fills the
single PD. **No structural change; no existing fingerprint moves** (a VM created at 256 MiB is
untouched). The only new cost is allocation time and the larger `guest_pages[]` array (256 K
pointers = 2 MiB) — trivial.

#### 3.2.2 To ~3 GiB — EPT PD fan-out

The single PD (`ept_pg[2]`) caps at 512 PT pages = 1 GiB. To reach 3 GiB:

- Compute `n_ept_pd = ceil(n_ept_pt / 512)` (3 for 3 GiB).
- Allocate the PD level dynamically: either extend the fixed `ept_pg[]` to hold PML4 + PDPT +
  `n_ept_pd` PD pages, or (cleaner, and matching the `ept_pt[]` idiom) add an `ept_pd[]`
  `kvmalloc_array`. Wire PDPT entry `p` (`ept_pg[1][p]`) to PD page `p` for `p < n_ept_pd`.
- `build_ept`'s inner loop becomes: for `k in [0, n_ept_pt)`, place `l2 = ept_pd[k / 512]`,
  index `l2[k % 512]` → `ept_pt[k]` → 512 guest pages. PML4[0]→PDPT unchanged.
- Free the PD pages in `hvx_vm_free_pages` alongside `ept_pt`/`guest_pages` (`hvx.c:4092`).

`invept_flush(vm->eptp)` after (re)build is unchanged (`hvx.c:886`). This is a self-contained,
independently-testable refactor (a `hvx_bigmem_test.c`-style guest that writes/reads a GPA above
1 GiB — the M11 test already does this pattern at 200 MiB, `hvx_bigmem_test.c`).

#### 3.2.3 Beyond ~3.9 GiB — the MMIO-hole split (designed, deferred)

`hvx`'s guest-physical space is **flat**: GPA page `i` → `guest_pages[i]`, contiguous from 0.
The IOAPIC (`0xfec00000`), LAPIC (`0xfee00000`), and virtio BARs (`0xfe000000`/`0xfe020000`)
sit in the 4th GiB and are **EPT-unmapped on purpose** so accesses trap and are decoded
(`ARCHITECTURE.md:183-185`; the EPT-violation decoders in `hvx.c` around `:2500`, `:2698`,
`:3341`). If `mem_size` reaches `0xfe000000` (≈3.968 GiB), those GPAs get backed by real RAM in
`build_ept` and **every MMIO trap silently breaks** — a guest LAPIC/virtio access would read
RAM instead of faulting. This is the classic PCI hole.

The fix (deferred to the rung that needs ≥4 GiB — the JVM at Rung 5):

- **Reserve `[HOLE_BASE, 4 GiB)`** (HOLE_BASE ≤ `0xfe000000`, conventionally 3 GiB `0xc0000000`)
  as an unbacked GPA range: `build_ept` leaves those EPT leaves empty; guest RAM above the hole
  is placed at **GPA ≥ 4 GiB**. Concretely, `guest_pages[i]` no longer maps to GPA `i<<12`
  linearly — introduce a `gpa_of_ram_page(i)` mapping that skips the hole (low pages below
  HOLE_BASE identity, high pages remapped to ≥4 GiB). `gpa_host` (`hvx.c:995`) and the mmap
  fault handler invert it.
- **Split the e820** (`hvx_boot.h:443-461`) from 2 entries into 3+: low RAM `[1 MiB, HOLE_BASE)`
  RAM, `[HOLE_BASE, 4 GiB)` reserved, `[4 GiB, 4 GiB + high)` RAM — the real-PC layout. The
  kernel then never places allocations in the hole.
- EPT PDPT gains entries for the 4th-GiB-and-above PD pages; the PML4 may need a second entry
  once GPAs exceed 512 GiB (far off).

This is bounded and well-understood, but it is a distinct chunk of work (GPA remapping touches
`gpa_host`, the mmap window, e820, and the fingerprint's RAM-walk order). **Recommendation:
land Rung 3 at ≤3 GiB (no hole), and pull the split in only when a service demands ≥4 GiB.**

#### 3.2.4 Fingerprint-neutrality summary

Every RAM change is a cap/structure change that is inert unless a VM asks for more memory. A VM
created at 256 MiB allocates 65536 pages, builds a single-PD EPT, and produces the identical
bytes — so **M16 (`1bdf266a…`), M25/M26, and the no-disk/iso/pci/net baselines are all
preserved**. A VM created at 3 GiB is a *new* configuration with its own new baseline; it does
not collide with any existing one.

### 3.3 A real multi-GB rootfs

**Boot-protocol changes** (all in the loader / `hvx_boot.h` recipe + `build-linux.sh` staging;
no kernel-module change):

- **Cmdline** (`hvx_linux.c` DEFAULT_CMDLINE): add `root=/dev/vda rootfstype=ext4 rw`
  (or `ro` + a remount) and keep the deterministic-boot flags (`console=ttyS0`, `nokaslr`,
  `x2apic_phys`, etc. per `PHASE6_DESIGN.md`). `nokaslr` stays load-bearing for determinism.
- **The tiny initramfs `/init`** (staged like M21's `initramfs-disk.cpio.gz`): load the virtio
  module stack (as it already does), `mount -t ext4 /dev/vda /newroot`, then `switch_root
  /newroot /sbin/init` (or a scripted launcher — see below). This keeps the RAM-resident
  initramfs small (fits the 1 GiB identity cap trivially) while the real, heavy root lives on
  the disk.
- **The ext4 rootfs image** (`build/rootfs.ext4.img`, staged by `build-linux.sh`): built
  **offline and deterministically** (§3.4.1), populated with the service binaries + their
  shared-library / runtime dependencies, and loaded into the disk via the new disk `mmap`
  (bulk) + `HVX_SET_DISK` (allocate/attach). Attach with `HVX_DISK_NO_IDMAP` (M21) — a real OS
  builds its own page tables (`hvx.c:4767`).

**Direct-mount alternative.** If the kernel is built with `virtio_blk` + `ext4` **in-tree**, it
can mount `root=/dev/vda` directly with **no initramfs** and run `/sbin/init` off the disk. This
is simpler but gives up the initramfs's explicit module-load ordering, which has been the
reliable path since M21. **Recommendation:** land `switch_root` first (reuses the proven M21
`/init`), keep direct-mount as a later simplification.

**Launcher.** For Rung 3 itself the rootfs need only boot to a shell / run one service; the
multi-service *launcher* is Rung 4's concern (`ROADMAP.md` Rung 4). Prefer a **scripted launch**
(a shell `/init`-style script), not systemd, for the same determinism reasons the roadmap gives
(systemd adds its own time/RNG/ordering surface, `ROADMAP.md` Rung 4).

### 3.4 Determinism at scale — the hard part

#### 3.4.1 mkfs pinning at scale (extends the harness agent's current work)

Rung 1's discipline (pin the mkfs UUID + superblock/inode timestamps so the image is
byte-reproducible, `ROADMAP.md` Rung 1 risk) must extend from a small ext2 image to a **multi-GB
ext4 with a journal**. The new reproducibility surfaces:

- **UUID + hash seed:** `mkfs.ext4 -U <fixed-uuid> -E hash_seed=<fixed-uuid>` (the htree
  directory-index seed is random by default and lands in the superblock).
- **Timestamps:** build under `faketime` / a fixed `SOURCE_DATE_EPOCH`; ext4 stamps
  superblock `s_mkfs_time`/`s_wtime`, inode times, and journal times.
- **Lazy init OFF:** `-E lazy_itable_init=0,lazy_journal_init=0` — otherwise the inode table and
  journal are zeroed **lazily by a kernel thread at first mount**, i.e. writes happen at a
  clock-dependent instant inside the deterministic VM. Forcing eager init makes the on-disk
  bytes a pure function of `mkfs` (offline, outside the VM) rather than of first-mount timing.
- **The journal itself:** a mounted rw ext4 writes journal commits with commit sequence numbers
  and timestamps. Under hvx these derive from the deterministic virtual clock (M16), so they are
  *expected* reproducible — but **unproven at the journal layer**, and one un-pinned journal byte
  breaks the whole-disk fingerprint. **Two mitigations, cheapest first:** (a) for the first
  landing, mount the root **read-only** (`ro`) with a separate small **rw data disk** for the
  service under test — the root stays a static, offline-built, trivially-reproducible image and
  only the small data disk changes (and that small disk is already the M22/M25/M26 fault target);
  (b) if a rw root is required, either use **ext2** (no journal) for the root or hunt the
  nondeterministic journal byte with the established boot-to-a-wall / diff-two-runs discipline.
  Recommendation: **ro root + rw data disk** — it sidesteps the journal question entirely for
  Rung 3 and matches how the fault-injection story already works (faults hit the data disk).

This composes with the harness-hardening agent's in-flight small-image mkfs fix: same flags,
larger image, plus the lazy-init and journal considerations above.

#### 3.4.2 Fingerprint cost — mmap + optional dirty-page hashing

The fingerprint FNV-hashes **all of RAM + the whole disk** on each of the four verify checks
(`hvx_disk_boot_verify.c:214-223`). At, say, 3 GiB RAM + 4 GiB disk that is ~28 GiB of hashing
per verify run (4 checks × 7 GiB) — minutes, and it grows linearly. Two levers:

- **Zero-copy via `mmap` (do this).** RAM is already mmap'd (`b->mem`, hashed in place). Add the
  disk `mmap` (§3.1) and hash the disk in place too, deleting the `HVX_GET_DISK` → `diskbuf`
  GB-scale copy. This roughly halves memory traffic and removes a GB allocation from the harness.
- **Hash only the touched subset (optional, harness-only).** Most of a multi-GB guest RAM stays
  the **zero page** through a boot; hashing GBs of zeros every check is pure waste. A
  determinism-preserving scheme: fold a **canonical hash of an all-zero image of the exact size**
  in once (a constant, cheap to compute analytically), then **XOR/replace** the contributions of
  only the pages that differ from zero. Detecting "differs from zero" cheaply is the catch —
  either scan for non-zero runs (still O(size) but memory-bandwidth-bound, ~fast), or have the
  kernel expose a **dirty bitmap** (EPT A/D bits, or a per-page "written" flag maintained in the
  mmap fault path) so the harness hashes only dirtied pages. **This is optional for the landing**
  — start with mmap + full hash (correct, just slower), add dirty-tracking only if verify time
  becomes the bottleneck. Any such optimization must be **proven equal** to the full hash on a
  small case before trusting it (the whole point is determinism; a clever hash that skips a
  changed page is a silent divergence).

Keeping FNV-1a is fine — it is not a security hash, only a divergence detector
(`hvx_verify.c` / M9 note), and it is memory-bandwidth-bound, so a fancier hash buys little.

#### 3.4.3 Snapshot / branch cost — and the L1 RAM ceiling (needs an L0-side change)

A full snapshot is `HVX_GET_SNAPSHOT` (tiny struct) + `memcpy(save_ram, mmap, mem_size)` +
`memcpy(save_disk, disk_mmap, disk_size)`; branch-restore is the inverse (M7/M16 shape). At GB
scale this is the dominant cost **and a hard feasibility wall**:

- **L1 has 4 GiB total** (`run-l1.sh:81`, `-m 4096`), shared with the L1 kernel, the module, and
  the harness. A 3 GiB guest + 4 GiB disk **live** already presses that; adding **save buffers**
  for a branch (another 3 GiB + 4 GiB) is impossible. **The naive double-buffer branch does not
  fit.**
- **This is the one item that needs an L0-side change to confirm:** raising `-m 4096` in
  `run-l1.sh` (host-side, in the shared L0 KVM). The physical host has 31 GB (`CLAUDE.md`), but
  L1 is a QEMU VM with a subset, and **the build machine is shared** — a large `-m` must be sized against
  the other tenants (never a broad `pkill`; scoped to `l1-hvx`). Flag for the user: **decide the
  L1 RAM budget before landing GB-scale snapshot/branch.** Rough sizing: for a snapshot+branch of
  an `R`-GiB guest + `D`-GiB disk you want L1 ≳ `2*(R+D)` + headroom. For a 3 GiB guest + 4 GiB
  disk that is ≳ 14 GiB of L1 — well within the 31 GB host if the other tenants allow it, but a
  deliberate L0-side allocation, not a default.
- **Alternatives that avoid the ceiling** (if L1 RAM cannot be raised enough):
  - **File-backed save.** Write the snapshot RAM/disk to a file in L1's filesystem instead of a
    RAM buffer — bounded by L1 disk, not L1 RAM, at the cost of I/O time. Simplest, no kernel
    change.
  - **Copy-on-write snapshot (kernel change, deferred).** Don't copy RAM at snapshot; mark the
    guest pages COW and copy a page only when the guest next writes it, so a branch's extra
    memory is proportional to *pages changed after the branch point*, not to total RAM. This is
    the real fix but it is a genuine run-loop/EPT change (write-protect the EPT leaves, handle
    the EPT-violation-on-write to copy) — **out of scope for Rung 3**, noted as the eventual
    answer.
  - **ro-root reduces the disk half.** If the root is ro (§3.4.1), only the small rw data disk
    needs snapshotting — the multi-GB root is a static image reloaded identically, so it need not
    be double-buffered at all (the harness keeps one copy and re-attaches it). This alone may keep
    branch cost tractable without raising L1 much.

**Recommendation:** land Rung 3 with **file-backed snapshot** (or ro-root so only the small data
disk is snapshotted), and raise L1 `-m` modestly after confirming the shared build machine's budget with the
user. Treat COW snapshot as a later optimization.

---

## 4. Phased sub-plan (smallest testable first)

Each phase is independently landable and leaves the tree green (all M3–M27 regressions pass,
dmesg-clean, clean unload) before the next. Fast free-run throughout.

- **R3.0 — disk page-array refactor, same 64 MiB cap (no behavior change).** Convert
  `disk` → `disk_pages[]` + the page-split `disk_rw` helper; route `vblk_disk_rw` and
  `vblk_unsync_commit` through it; page-wise `HVX_SET/GET_DISK`; free path. **Assert every disk
  test is byte-identical** (`hvx_disk_boot_verify`, `hvx_fault_test`, `hvx_sqlite_verify`,
  `hvx_redis_verify` fingerprints unchanged) — this is the pure-refactor safety net, the M11
  move applied to the disk. *Smallest possible signal; permanent regression.*
- **R3.1 — disk `mmap` + raise `HVX_DISK_MAX`.** Add the disk mmap window + `hvx_disk_ops`
  fault handler; switch one harness (`hvx_disk_boot_verify`) to load + fingerprint the disk via
  mmap; confirm the fingerprint matches R3.0's `HVX_GET_DISK` path (proves the mmap sees the
  same bytes). Bump `HVX_DISK_MAX` to multi-GB; a multi-hundred-MiB disk test round-trips.
- **R3.2 — RAM to 1 GiB (`MAX_GPAGES` bump).** Pure cap change; a `hvx_bigmem_test`-style guest
  writes/reads a GPA near 1 GiB; assert all prior fingerprints unchanged (256 MiB VMs identical).
- **R3.3 — EPT PD fan-out to ~3 GiB.** The dynamic PD level; a guest touches a GPA above 1 GiB
  and above 2 GiB; determinism check (two runs identical). *Isolated EPT test, no Linux needed.*
- **R3.4 — real ext4 root via `switch_root`.** Stage `build/rootfs.ext4.img` deterministically
  (§3.4.1, ro root + small rw data disk); tiny initramfs mounts + `switch_root`; boot to a shell
  off the real root. First real-Linux wall-decode iterations expected (module order, mount
  options, root device timing) — the established discipline.
- **R3.5 — determinism harness at scale.** Extend `hvx_disk_boot_verify.c` (or a new
  `hvx_rootfs_verify.c`) to fingerprint the GB-scale RAM + (ro root static + rw data disk) via
  mmap, and run the four checks. Use **file-backed snapshot** (or ro-root single-copy) to stay
  under the L1 RAM ceiling; **confirm the L1 `-m` budget with the user first** (§3.4.3). Record
  the new baselines in `CLAUDE.md` (whole-run + branch), asserting all pre-Rung-3 baselines are
  unchanged.
- **R3.6 (optional) — verify-cost optimization.** Add dirty-page/zero-baseline hashing only if
  R3.5's verify time is the bottleneck; prove it equal to the full hash on a small case first.
- **Deferred to a follow-on rung:** the >3.9 GiB PCI-hole split (§3.2.3); COW snapshot (§3.4.3);
  direct-mount (no initramfs) rootfs (§3.3).

Land **R3.0–R3.5** as the milestone. R3.0 and R3.3 are the two refactors that carry all the
risk; both are testable without Linux, so they land and prove first.

---

## 5. Risks (honest)

- **GB-scale contiguous `kvmalloc` fails (the flagged risk).** Retired by R3.0's page-array
  refactor — the same fix M11 already validated for RAM. The refactor itself is the cost, not
  the run-time behavior. *Mitigation: land R3.0 as a pure byte-identical refactor with the full
  disk-test suite as the safety net before raising any cap.*
- **EPT single-PD 1 GiB ceiling (not previously called out).** Raising `MAX_GPAGES` past 1 GiB
  **without** the PD fan-out (R3.3) would build a truncated EPT and fault the guest at the first
  GPA above 1 GiB. *Mitigation: the cap bump (R3.2) stops at 1 GiB; anything past it lands only
  with R3.3. Add a `BUILD_BUG`/runtime guard tying `MAX_GPAGES` to the PD count.*
- **MMIO-hole collision past ~3.9 GiB (silent, dangerous).** A flat RAM space reaching
  `0xfe000000` backs the LAPIC/IOAPIC/virtio GPAs with RAM and **silently breaks every MMIO
  trap** — no fault, just wrong bytes and a diverged/hung boot. *Mitigation: cap Rung 3 at
  ≤3 GiB; the hole split (§3.2.3) is a designed, deferred follow-on. Guard `MAX_GPAGES` so a VM
  cannot be created with `mem_size` reaching the hole until the split lands.*
- **L1 RAM ceiling blocks GB-scale snapshot/branch (the L0-side item).** 4 GiB L1 cannot
  double-buffer a multi-GB guest+disk. *Mitigation: ro-root (snapshot only the small data disk)
  and/or file-backed snapshot for the landing; raise `run-l1.sh` `-m` after sizing against the
  shared build machine with the user; COW snapshot as the eventual real fix.* **This is the one risk that
  needs an L0-side change to confirm — surface it to the user before R3.5.**
- **mkfs-at-scale / journal reproducibility.** A rw ext4 journal is a new byte-reproducibility
  surface; one un-pinned byte breaks the whole-disk fingerprint. *Mitigation: ro root + small rw
  data disk sidesteps the journal for Rung 3; pin UUID/hash-seed/timestamps and force eager
  (non-lazy) init; extend the harness agent's in-flight small-image fix. If a rw root is later
  required, hunt the nondeterministic byte with the boot-to-a-wall / diff-two-runs discipline.*
- **Verify cost grows linearly.** Minutes per verify at GB scale, ×4 checks. *Mitigation: mmap
  zero-copy hashing (R3.1) first; optional dirty-page hashing (R3.6) if it bottlenecks. Accept
  minutes — it is the standing free-run operating mode (`ROADMAP.md §5`), not the astronomical
  single-step path.*
- **Longer real-distro boots.** A real rootfs boot is more instructions and each wall-decode
  iteration costs more wall-clock. *Mitigation: none needed structurally — still minutes under
  free-run; keep the ro-root/data-disk split so most of the boot is static.*
- **Single-vCPU free-run speed for heavy services (standing constraint).** All services share
  one time-sliced core (`ROADMAP.md §5`, M17 hardware-blocked). A busy-waiting service taxes the
  whole VM. *Not retired by Rung 3 — it is the fixed shape of the platform; Rung 3 only removes
  the RAM/disk ceiling so heavy services fit at all.*

---

## 6. Call-outs for the user

- **Needs an L0-side change (decide before R3.5):** the **L1 RAM budget**. `run-l1.sh` launches
  L1 with `-m 4096`; GB-scale snapshot/branch needs more, sized against the **shared** L0 KVM
  host, the build machine (never a broad `pkill`; scope to `l1-hvx`). Options if L1 cannot grow: ro-root
  single-copy snapshot, or file-backed snapshot. **This is the single hard external dependency.**
- **Could block GB-scale determinism (watch items):** (a) the ext4 **journal** on a rw root —
  prefer ro-root + a small rw data disk to sidestep it entirely; (b) any un-pinned **mkfs**
  metadata at scale — extend the harness agent's fix; (c) **verify tractability** — land mmap
  zero-copy hashing before worrying about dirty-page tracking.
- **Two refactors carry the risk, both Linux-free to test:** the **disk page array** (R3.0,
  the M11 move for the disk) and the **EPT PD fan-out** (R3.3). Land and prove each as a
  byte-identical / isolated change before scaling anything.
- **Fingerprint-neutrality holds:** every RAM/disk cap and structure change is inert unless a VM
  asks for more, so M16/M25/M26 and all device baselines are preserved; GB-scale VMs are new
  configurations with new baselines that collide with nothing.
</content>
</invoke>

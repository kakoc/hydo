# Real services on the Mac VMM (`hvm`)

**Result:** unmodified Redis 7.2.4, and unmodified miniflux 2.1.3 with PostgreSQL 15.7, run under `hvm` on an M2 Pro with their data on the virtio-mmio disk (`/dev/vda`, ext4). They are the aarch64-linux builds from the nixpkgs binary cache. Every run is bit-identical: replay, branch-from-snapshot, and runs with all 12 host cores saturated. The power-cut checks give hvx's Milestone 27 contrast, on the Mac:
- with the durable setting, every acknowledged write survives;
- with the relaxed setting, an acknowledged write is lost, as each project documents.

**The spin limit was not hit.** No run tripped the spin watchdog (`BOOT.md`, Limits). Redis, PostgreSQL (its spinlocks back off into sleeping syscalls), miniflux (Go, `GOMAXPROCS=1`), curl and busybox all make exits often enough: every sleep arms the trapped timer, and every clock read is a trapped counter read.

## Results

The workloads and invariants are those of `hvx/onboard/examples/redis` and `hvx/onboard/examples/miniflux`:
- **Redis:** 20 SETs over 127.0.0.1. Every acknowledged SET must read back.
- **miniflux:** 30 category creates over 127.0.0.1:8080 through its API. Every acknowledged category is stored exactly once, no rejected create is applied, and, in a single boot, the API lists every acknowledged category.

**Data on the disk** (kernel `Image-svc`):

| Check | Redis | miniflux + PostgreSQL |
|---|---|---|
| One boot, durable setting | `OK acked=20 all present`, `2e8b1c288435835f`, 0.16 s | `OK acked=30 exactly once`, `3a4e3bdd3da1fec8`, 6.4 s |
| Power cut, durable setting (`appendfsync always` / `synchronous_commit=on`), twice | `OK acked=20` both times; boot 1 `dfd6714b5a720819`, boot 2 `1a7d57a92770f757` | `OK acked=30` both times; boot 1 `ecebaee6ed85eed9`, boot 2 `7d2777fc8cede510` |
| Power cut, relaxed setting (`everysec` / `off`), twice | **`VIOLATION key k:0 lost`** both times; boot 1 `c6d8ce1432e1c276`, boot 2 `23a3d10e30d14855` | **`VIOLATION category c15 stored 0 times`** both times; boot 1 `a49dee23ceb92f4f`, boot 2 `6dd1c99f4746aef4` |
| Branch of the relaxed power-cut boot (snapshot mid-run, restore into a new vCPU) | at `redis ready after`: IDENTICAL, = `c6d8ce1432e1c276` | at `setup durable`: IDENTICAL, = `a49dee23ceb92f4f` |

Durable images after the cut: Redis `always` `7060b09aa728331c`, `everysec` `748c9fe690048d45`; PostgreSQL `on` `04797d2b483912b1`, `off` `ef1205c17e7e82a4`, each the same in both runs.

The power cut is the two-boot model of `cut-test.sh` and hvx M27. In boot 1 every write is held in the disk's volatile cache until a FLUSH (`HVM_FAULT_CLASSES=unflushed`, prob 100), and the boot stops right after the last acknowledgement. `HVM_DISK_OUT` keeps only the flushed bytes. Boot 2 is a fresh VM on that image, with no faults: Redis replays its AOF, or PostgreSQL runs WAL recovery, and the acknowledged list from boot 1 is checked. For PostgreSQL, the setup (initdb, schema, miniflux's admin user) is made durable by a `CHECKPOINT` before the workload, so only the workload is in the cut window. Redis's loss is the same as hvx M27's (`k:0`).

**First stage, data on tmpfs** (M48 kernel + `svc.config`, no devices, default log level):

| Check | Redis | miniflux + PostgreSQL |
|---|---|---|
| Fresh boots | 10 of 10 `3c0976cd15860f99`, 19,073 exits, 0.21 s | 10 of 10 `b680f3e3ed024afc`, 56,158 exits, 2.9 s |
| Branch | 3 points (initramfs unpack, Redis ready, oracle): all IDENTICAL | 4 points (initdb done, PostgreSQL ready, miniflux ready, workload done): all IDENTICAL |
| 12 host cores saturated | 5 of 5 identical | 5 of 5 identical (7.5–18.6 s wall) |

## What was needed

- **Kernel options** (`svc.config`, on top of `../linux/hvm.config` + `../linux/hvm-virtio.config`). `allnoconfig` turns these syscalls off, and the services need them: `SYSVIPC` (PostgreSQL's interlock segment), `ADVISE_SYSCALLS`, `FILE_LOCKING`, `MEMBARRIER`, `RSEQ`, `FHANDLE`, `INOTIFY_USER`, `KCMP`.
- **Only the files the loader opens.** The nixpkgs closures are large (Redis 207 MB, mostly systemd), so `elfdeps.py` reads `PT_INTERP`/`DT_NEEDED`/`DT_RUNPATH` straight from the ELF files and copies only what the dynamic loader will open. The guests are 22 MB (Redis) and 124 MB (miniflux + PostgreSQL + curl) unpacked.
- **PostgreSQL's `initdb` is a bash wrapper in nixpkgs.** It adds glibc's `locale` to `PATH`. With `--locale=C` the wrapped binary (`.initdb-wrapped`) is enough, so bash is not needed.
- **No CRNG seeder.** The device tree's `rng-seed` with `random.trust_bootloader=on` (`hvm.c`) initialises the CRNG at boot, so the hvx M27 seeder is not needed here.
- **No idle heartbeat.** Neither service stalled tickless.
- **No `hvm.c` change.**

## Files

| File | What |
|---|---|
| `build.sh` | builds everything on the build machine with nix: `Image-svc`, `redis.cpio.gz`, `miniflux.cpio.gz`, `redis-disk.img` (64 MiB ext4), `pg-disk.img` (256 MiB ext4). Pinned store paths at the top |
| `elfdeps.py` | the shared-library closure of ELF files, for any architecture |
| `svc.config` | the kernel options above |
| `init-redis`, `init-miniflux` | the guests' `/init`, with phases `solo`, `cut` and `recover` from the kernel command line |
| `run.sh` | `run.sh solo <svc> <setting>` and `run.sh cut <svc> <setting> <dir>`, run from `hvx-mac/` |

Build outputs (`SHA256SUMS`): `Image-svc` `24400b35dbb7be03…`, `redis.cpio.gz` `9d0d7f0a4c922e80…`, `miniflux.cpio.gz` `14c7a1b47f114b86…`, `redis-disk.img` `3bd2539ab11eeae2…`, `pg-disk.img` `81fb3fee1224710…`. A clean rebuild for byte-identity was not checked separately. The inputs are pinned and the packing is fixed: sorted cpio with fixed mtimes, `gzip -n`, and `E2FSPROGS_FAKE_TIME`.

```sh
# Linux box, scratch dir:  WORK=$PWD sh <repo>/hvx-mac/services/build.sh;  copy out/* to hvx-mac/services/guest/
cd hvx-mac && sh build.sh
sh services/run.sh solo redis always
sh services/run.sh cut redis everysec /tmp/r      # -> VIOLATION key k:0 lost
sh services/run.sh cut miniflux off /tmp/m        # -> VIOLATION category c15 stored 0 times
```

## Not done

- **Faults other than the power cut** (kill, pause, network) are not wired into these `/init`s. The disk fault classes work through `HVM_FAULT`.
- **The explorer and `hydo.yaml` do not drive `hvm` yet.**
- **miniflux's** periodic API invariant is checked once at the end, not every 2 s.

# Copy-on-write guest RAM on the Mac engine (design)

Status: built in Milestone 335 (tentative number), on by default. `HVM_RAM_SHARE=0` turns it off.
The code is `hydo/crates/hydo-hvm/src/mem.rs` (the RAM base), `serve.rs` (`xsnap_take` and
`xsnap_restore`), `vmm.rs` (`fresh_vm_from` and `rebase`), `hydo_engine::hvm::worker_memory` (what
the explorer counts) and the boot gate of `hydo-explore`'s pool. This page explains the problem,
the choices, and what is left.

## The problem

An explorer worker on the Mac is one `hydo-hvm` process with one VM. Before Milestone 335 a
worker held three copies of its guest's RAM:

- the guest RAM, an anonymous mapping that the engine locked with `mlock`, so all of it was the
  worker's own from the start;
- the snapshot S0, the RAM's bytes in an unlinked file of its own;
- the snapshot S1, in a second such file.

A restore compared every 16 KiB page of the snapshot with the RAM and copied the pages that
differed. The explorer counted twice the guest RAM and 256 MiB per worker. On this M2 Pro with
32 GB, a Meilisearch worker of 2,048 MiB had a footprint of 2,173 MB and mapped 2,220 MB of
snapshot files. A user with a 5 GB guest on a 32 GB Mac got one worker, and 60 seeds took about
an hour.

The workers of one exploration boot the same image to the same bytes, and a run writes a part of
the RAM. Most of each worker's memory held bytes that every other worker held too.

## The base of a snapshot

A RAM base is a snapshot's guest RAM in a file that nothing writes after it is made. The file
holds only the RAM's pages that are not zero, in extents (runs of pages); the others are holes.
The engine maps guest RAM from the whole file with `mmap(MAP_PRIVATE | MAP_FIXED)`. A page then
reads the base until something writes it: the guest through Hypervisor.framework, or the engine's
devices. The macOS kernel copies a page at its first write, so only the written pages are the
process's own memory.

The engine reads every page of the RAM for a fingerprint. A read of a hole makes a page of zeros in
the file's page cache, once for every process that maps the file. These pages are clean, and the
kernel can drop them and make them again without a disk read. A version that mapped only the
extents from the file and left the holes anonymous was measured and dropped. macOS gives each
process its own page for an anonymous page that is read, so a worker of a 4,096 MiB Redis guest
held 3,957 MB in the median instead of 53 MB.

- **The name.** A base is the file `ram-v1-<length>-<hash>` in `HVM_SNAP_DIR` (default
  `/tmp/hvm-snap`). The hash is the RAM tree hash of Milestone 294 over the base's bytes.
- **Finding another worker's base.** Before it writes a base, the engine opens the file of that
  name and reads its extents with `lseek(SEEK_DATA)` and `lseek(SEEK_HOLE)`. It uses the file only
  when each extent's bytes equal its RAM's and its RAM is zero between them. Otherwise it writes a
  file of its own. A base is written to a temporary file and then linked to
  its name, so a file found by its name is complete.
- **Who removes a base.** Each process holds a shared `flock` on every base it uses. When a base
  is dropped, the engine removes the name if it can take an exclusive lock without waiting. An
  engine that starts removes the bases that nobody holds, such as the bases of a worker that was
  killed.

The bytes of a base never change, and a guest RAM mapped from it is a private copy. So the bytes
the guest sees are the bytes of the snapshot, as with a copy, whatever other processes do.

## A snapshot and a restore

- **A snapshot** (`SNAP`) makes a base of the RAM. Then the engine maps guest RAM from that base at
  once. It removes the RAM from the VM (`hv_vm_unmap`), maps it from the base, and gives it to the
  VM again (`hv_vm_map`). It makes the instruction cache coherent over the base's extents, the only
  bytes the host placed. The vCPU and every device keep their state. The guest sees the same bytes
  at the same addresses, and the pages the boot wrote stop being the worker's own. The rewind mode of the spin observer keeps per-page copies and
  stage-2 protections, so a snapshot in that mode skips this step.
- **A restore** destroys the VM, maps guest RAM from the snapshot's base again, and creates the
  VM and the vCPU. Mapping again drops every page the run wrote at once. A restore no longer reads
  every page of the RAM and of the snapshot to compare them.

Every restore goes through this. Examples are the explorer's restore of S1 before a schedule, the
power cut's recovery boot from S0, and a `RESUME` of an inspection or of the state search. A snapshot
that replaces another of the same name makes its new base before it drops the old one, so equal
bytes keep one file.

## The fingerprint

The RAM's part of a fingerprint still hashes every page from its bytes. Reading a page that the run
did not write maps the base's page from the page cache, or the kernel's zero page, and copies
nothing.

The KVM engine takes the hashes of the pages that the guest did not write since its base from the
base (Milestone 294). Linux's page map says exactly which pages are private. macOS gives
`mach_vm_page_range_query`, which marks a private copy of a file's page as copied. A probe showed
the marks for resident pages. It did not show what the query says for a private page that the
kernel compressed or swapped out. A wrong answer there would change a fingerprint, so the engine
does not use the query.

## What a worker costs

The numbers come from `footprint <pid>`, sampled every 3 s for every engine of an exploration.
Its `phys_footprint` is the process's own resident and compressed memory. It leaves out the clean
file pages that the process maps, such as a shared base. The bases' size is their allocated size
on disk (`du -k`), which is the page cache they take once. "Before" is the engine without
Milestone 335, and "after" a worker once it has booted. Milestone 335 in `docs/HISTORY.md` has
the runs.

| Example | Guest RAM | Before: a worker | After: a worker, median and largest | After: data of each of S0 and S1, shared |
|---|---|---|---|---|
| Redis (the quickstart) | 1,024 MiB | 1,133 MB, and 1.1 to 2.0 GB of snapshot files | 36 to 113 MB, 137 MB | 60 MB |
| Redis | 2,048 MiB | 2,119 MB | 119 MB, 129 MB | 78 MB |
| Redis | 4,096 MiB | 4,118 MB | 68 MB, 87 MB | not measured |
| Meilisearch (an image service) | 2,048 MiB | 2,155 MB, and 2,220 MB of snapshot files | 171 MB, 1,153 MB | 535 MB |

The page cache of a base also holds the zero pages of its holes once the fingerprint has read
them: 4,096 of 4,096 MiB for a base of the 4,096 MiB guest. They are shared by all workers, and the
kernel can drop them without a disk read.

The Meilisearch row's median comes from 60 seeds without shrinks. An exploration of 16 seeds with
its shrinks gave a median of 1,058 MB and at most 1,172 MB. Those runs wrote about half of the RAM,
and the cause was not traced. A worker still writes all of its guest's RAM while it boots,
before S0, because the guest kernel touches every page. Without `mlock` that took about 2 s for
1 GiB, and the snapshot S0 then gives the pages back.

The explorer counts, for the Mac engine with RAM bases (`hydo_engine::hvm::worker_memory`):

- for the bases of S0 and S1, half of the guest RAM once;
- for each worker, half of the guest RAM and 256 MiB: the pages a run writes, and the engine's own
  memory;
- for each worker that boots, until its S0: the guest RAM and 256 MiB.

So only as many workers boot at once as fit beside the others' need. The pool line says so when
that is fewer than the workers, for example `pool: 4 workers, snapshots in files, 1 booting at
once`. With 9.4 GB available on this Mac the count gives these pools:

| Guest RAM | Workers before (twice the RAM and 256 MiB each) | Workers after |
|---|---|---|
| 1,024 MiB | 4 | 11, so the hardware threads decide |
| 2,048 MiB | 2 | 6 |
| 4,096 MiB | 1 | 3 |

A project whose runs write more than half of the RAM gets more workers than fit. The guest RAM is
not locked, so macOS then compresses and swaps the workers' pages, and the runs slow down.

## Why guest RAM is no longer locked

`mlock` of a writable private mapping makes the kernel copy every page. A probe locked a private
mapping of a 256 MiB file, and the footprint grew by 256 MiB. So a locked RAM would be the
worker's own again. The lock never decided a run. Hypervisor.framework resolves the guest's
stage-2 faults in the kernel, and the engine counts only the exits that return to it. With RAM
bases the engine does not lock guest RAM. With `HVM_RAM_SHARE=0` it locks it as before, unless
`HVMX_NO_MLOCK` is set.

## The switch

`HVM_RAM_SHARE=0` gives the engine of before Milestone 335. Each snapshot is then an unlinked file
of its own, and a restore copies the pages that differ. The explorer counts twice the RAM and 256
MiB per worker.

## What is left

- The hvx engine keeps S0 and S1 in its kernel module or in files of its own, and its guest RAM
  is the module's memory. A shared read-only root and copy-on-write VMs there are a module change
  (open item 36).
- The KVM engine shares bases and pages within one worker (Milestone 161), not between workers.
  The same named files would let its workers share S0 and S1 too.
- A worker's boot still writes the whole RAM. A worker that started from another worker's S1
  would not boot at all, but the host controller's state at S1 would have to move with it.
- The estimate of half the RAM per worker comes from two examples. A run that writes more makes
  the pool too large for the memory, and macOS then compresses and swaps.

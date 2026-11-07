# Storage faults of one service (design)

Status: built (user decision 2026-11-07). The authority on the token text is `hydo-schedule`
(`src/storage.rs`, the tests `storage_faults` and `corruption_plans` in `tests/language.rs`); on
the manifest key, the project format's schema (`services[].storage`); on what the controller does,
`hydo-control/src/schedule/storage.rs`. This note explains the options and the choices.

## The problem

hydo's disk faults act on one data disk shared by every service, and its power cut (`C=`, `cf=`,
`disk=cut`) takes the power of the whole guest: every service loses what was not flushed, at
once. Onboardings needed faults that hit one service's storage:

- NATS JetStream (`hydo/onboard/examples/nats-jetstream/RESULTS.md`, open item 38 (b)): Jepsen's
  findings 3.2 and 3.3 on 2.12.1 need bit flips or truncation of one node's `.blk` and snapshot
  files; 3.5 needs a power loss of one node while the others run on.
- TigerBeetle (`hydo/onboard/examples/tigerbeetle/RESULTS.md`, M176): its published findings need
  corruption or loss of one replica's data file.

Two faults are wanted: **corruption of one service's files**, and **a power loss of one service**
(what it had not made durable is lost, the others keep running). Both must be pure functions of
the schedule, like every other fault, and must not change any existing image, schedule or
recorded value.

## Corruption of one service's files: `G=`

### Options

1. **From the host controller through the agent.** At a defined stop of the run the controller
   lists the service's storage directory (`run` with the image's busybox), decides from the token
   and the listing which file and which bytes, reads the bytes it flips (`read`), and writes the
   changes with busybox `dd`. Every command is in the control transcript.
2. **A block-level fault in the engines.** The engines already corrupt disk requests for the
   whole disk (`disk=` faults). A per-service variant would need the engine to know which disk
   blocks belong to which service's files: the file system's block map, read on the host, or a
   partition per service (below). It changes the engines (and, for hvx, the kernel module), and it
   corrupts blocks, not files, so a target like "the `.blk` files" cannot be said.
3. **A new agent command** (`corrupt path offset ...`). Shorter transcripts, but a new agent pin
   moves every image's bytes and every recorded fingerprint (AGENTS.md: a new pin is a
   re-baseline), and it buys nothing that option 1 cannot do with existing commands.

### Choice: option 1

It needs no engine change, no module change and no new agent; it works the same on hvx, hvm and
KVM; it names files, so a schedule can aim at a kind of file (`file=.blk`); and it is
deterministic for the same reason every controller action is: the guest is deterministic up to
the stop where the controller acts, so the listing it reads is, and the decision is a pure
function of the token and the listing (`hydo_schedule::Corruption::plan`, splitmix64 of the seed).
The listing is sorted on the host, so the order `find` prints does not matter.

### The token

`G=<svc>@<at>[+<down>]:<kind>[,n=<count>][,len=<bytes>][,file=<text>][,seed=<hex>]`

- before step `at`; with `+<down>` the service is killed first and started again before step
  `at+down` (its files change while it is down, as a disk's would between two boots); without it
  the files change under the running service (a corruption the service meets while it runs);
- `flip`: `n` single-bit flips (default 1, at most 64) at seeded offsets of one file;
- `zero`: `len` bytes (default 4096, at most 1 MiB) set to zero at a seeded offset;
- `trunc`: the file shortened by `len` bytes, or without `len` by a seeded number of bytes (at
  least one);
- `file=`: only files whose path relative to the storage directory contains the text;
- `seed=`: chooses the file and the places (default 0).

The keys come in this order, each at most once, so that a token prints back byte for byte (the
guest logs the guest part as written). The letter is `G`: of the letters no manifest knob and no
recorded schedule uses, the identity corpus has an unknown knob `Z=1`, which a reserved `Z` would
turn into a refusal and so change a recorded transcript.

The changes go through the page cache (busybox `dd`, `conv=notrunc`), so they are subject to the
same durability as any write: a later whole-guest power cut may take them away, a reader with
`O_DIRECT` sees them (the kernel writes dirty pages back before a direct read of their range).

## A power loss of one service: `L=`

### Options

What a power loss takes away lives in two places: the guest kernel's page cache (data written but
not yet written back) and the disk's volatile write cache (written back, not flushed; the engines
keep it, `HVX_GET/SET_UNSYNC`, and drop it at `DROP_UNFLUSHED`). A per-service power loss must
take both away for that service only.

1. **A second virtio disk per service, and `DROP_UNFLUSHED` per disk.** The engines would map
   several disks (hvx: in the kernel module; hvm and KVM: in the engine), and drop one disk's
   unflushed writes. That covers the disk's cache but not the page cache: the guest kernel still
   holds the service's dirty pages and writes them back later. The page cache must be dropped by
   the guest anyway (below), so this only adds the second half.
2. **The unflushed writes of one partition's block range dropped by the engine.** The same as 1
   without a second device: the engines' unsync cache already records each write's block, so a
   "drop the unflushed writes in blocks [a, b)" request would do it. It changes all three engines
   and the hvx kernel module (a side module and the user's approval to reload).
3. **Shutting down the service's own file system in the guest.** Each service that wants it gets
   its own ext4 file system (a partition of the data disk); at the fault the controller stops the
   service's process tree, tells the kernel the file system failed (ext4's sysfs file
   `/sys/fs/ext4/<dev>/trigger_fs_error`, with the mount option `errors=remount-ro`: the journal
   is aborted, the file system is marked shut down and read-only, so nothing more of its data or
   metadata reaches the disk), kills the tree, unmounts the file system (the kernel drops the
   pages it had not written back) and mounts it again (the journal is replayed, as at a boot after
   a power loss). This is the guest-side half, and it needs no engine and no new program: the
   agent's `signal`, `run` and `mount` commands and the image's busybox do it.
4. **A file system in user space that keeps unsynced writes and drops them on command** (the
   approach of LazyFS, which Jepsen's analyses use). A FUSE program in the guest: a new pinned
   guest program, a kernel with FUSE, and a second copy of every write.
5. **A loop device per service.** Like 3, but over a file on the shared data file system: the
   loop file's pages in the shared page cache are a second cache level that the fault would not
   take away, and the hvx guest kernel may not have the loop driver built in.

### Choice: option 3 now, option 2 as the later refinement

Option 3 is the simplest sound one. Sound: every state it leaves is one a real power loss can
leave. What the kernel had written back before the fault stays, which is what a disk that kept
every write it was given would leave; what it had not written back is gone. Journal transactions
committed before the fault are replayed, uncommitted ones are not. Deterministic: the guest's
writeback is deterministic under every engine, so what had been written back at the stop where
the controller acts is too. It works on hvx, hvm and KVM unchanged, and it changes no existing
image (only a manifest that asks for an own file system gets the new disk layout).

It is not the harshest power loss: writes already handed to the disk but not flushed survive,
where a real disk's volatile cache could lose them. Most of what a service has not made durable
is still in the page cache at the fault (Linux writes dirty data back after about 30 s, and a
service that does not `fsync` keeps recent writes there), so the common bug, an acknowledgement
before `fsync`, is reached. Option 2 adds the disk-cache half: the engines' unsync cache dropped
for the partition's block range at the same stop. It is left for later because it changes the
engines and the hvx module (open items).

### The disk layout

`services[].storage: {dir: <dir under /data>, size_mib: <16..3072>}` makes the directory the
service's own file system. hydo-build (`image::assemble_disk_volumes`) keeps the root (p1) and
data (p2) partitions as they are and adds an extended partition p3 that holds one logical
partition per own file system, in the manifest's order of the services: `/dev/vda5`, `/dev/vda6`,
... Each is an empty journaled ext4 made with the image builder's pinned `mke2fs` under the frozen
build clock (the data disk's recipe, its own UUIDs), 1 MiB after its extended boot record. The
whole disk stays within the 4 GiB an engine maps (hydo-build refuses more). The guest kernels of
all three engines read MBR logical partitions (`CONFIG_MSDOS_PARTITION`), have ext4 and sysfs,
and the agent mounts devtmpfs, so the device nodes exist.

The controller mounts each own file system on its directory right after the data disk, in the
boot to S1 and in the recovery boot after a whole-guest power cut, with the data disk's options
and `errors=remount-ro`. A whole-guest power cut acts on these partitions as on the rest of the
disk.

### The token

`L=<svc>@<at>+<down>`: before step `at`, the power loss of the service (above); it starts again
before step `at+down`. A service without an own file system: the token is logged and ignored.
A file system that does not mount again is the verdict `VIOLATION <svc> storage unmountable after
its power loss`; a service that does not start again is `ERROR <svc> restart failed (after its
power loss)` (as `K=`; with `restart_may_fail` it is left to the heal).

## The explorer

`--storage-faults` (opt-in, like `--more-faults`) draws, from a stream of its own (so every other
field of a seed's schedule stays as it was), a `G=` in half of the schedules on a service with
`storage:` (in 70% of them while the service is down for 1 to 3 steps; flips of 1, 4 or 32 bits,
zeroed ranges of 512 B to 64 KiB, truncations; a drawn seed) and an `L=` in 40% on a service with
its own file system. A manifest's `faults:` decides (`corrupt`, `powerloss`; without a `faults:`
list, `storage:` is the opt-in). The shrinker drops each token, shortens the time down to one
step and a flip to one bit. A failure with `G=` has the class `corrupted-files`; a relaxed knob
with `L=` is `unsafe-knob`, as with a whole-guest cut.

## What stays the same

No engine, kernel module, guest kernel or agent changed; images of manifests without `storage:`
are built byte for byte as before (the old disk assembly runs for them); schedules without `G=` or
`L=` send the same commands (the new mounts and declarations happen only with `storage:` or the
tokens). The project format gained an optional key and two `faults:` values (additions only).

## Left for later

- The disk-cache half of a service's power loss (option 2): the engines drop the unflushed writes
  of the partition's block range at the same stop; hvm and KVM first, hvx on a side module.
- Corruption at the block level below the file system (misdirected or lost writes of one
  service), which option 2's request would also make possible.
- A corruption that changes bytes the kernel has cached but the disk does not (or the reverse):
  only what the page cache shows is changed today.

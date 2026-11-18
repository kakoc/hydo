# Faults of the disk's blocks (design)

Status: built in Milestone 313 (tentative number) on the Mac and KVM engines. hvx refuses the
faults before any VM. The authority on the token text is `hydo-schedule` (`src/host.rs`,
`src/storage.rs`, and the test `block_faults` in `tests/language.rs`). The engine side is
`hydo/crates/hydo-hvm/src/virtio/block.rs`, which the KVM engine uses unchanged. The controller's
side of `G=...:bad|rot` is `hydo/crates/hydo-control/src/schedule/sectors.rs`. This page explains
the problem, the choices, and what is left.

## The problem

A team that tests its own storage engine with its own simulator said that hydo's disk faults act
on whole requests and whole files. A storage engine's own rules are about sectors. A write of 8
sectors can reach the disk only in part, a block can go to the wrong place, and a sector can
become unreadable. Before Milestone 313 hydo had four disk faults:

- a power cut that loses every write the disk did not flush (`disk=cut` with `C=` or `cf=`);
- seeded I/O errors on whole requests (`disk=ioerr:<p>`);
- a disk that drops writes it acknowledged (`disk=lying:<p>`);
- bit flips, zeroed ranges and truncation of one file, through the guest (`G=`).

None of them can tear one write, move one write, damage one named sector, or leave one request
without an answer. A real disk does each of these.

## The four faults

Each fault is a host token. The engine decides it from the schedule's `dseed=` and from request
numbers counted from the workload's start, so a schedule repeats bit for bit. Each is inert when
absent. Its state lives outside the device state that every recorded fingerprint hashes, and it
adds nothing to a fingerprint until a token arms it.

| token | what the disk does |
|---|---|
| `torn=<p>[:any]` | Needs `disk=cut`. At a power cut, each write of two or more sectors in the volatile cache is torn with probability `<p>` percent. A torn write keeps a seeded prefix of its sectors. With `:any` it keeps a seeded subset. It never keeps none of them or all of them. |
| `mis=<k>[:<sectors>]` | The k-th write request of the data disk goes `<sectors>` away, with a sign. Without the distance it goes 1 to 16 blocks of 4 KiB away in a seeded direction. The sectors it was meant for keep their old bytes. |
| `lse=<sector>+<count>[,...][:rot]` | Latent sector errors. A read that touches one of the sectors fails with an I/O error. With `:rot` it returns 16 seeded damaged bytes per sector, the same at every read. A write of a whole sector repairs it, as a disk remaps a sector it can write. The sectors stay bad for the whole run, also after a power cut. |
| `hang=<k>[+<ms>]` | The k-th request of the data disk (a read, a write or a flush) gets no answer. With `+<ms>` it completes with an I/O error after that many milliseconds of guest time. |

`G=<svc>@<at>[+<down>]:bad|rot[,len=][,range=][,file=][,seed=]` aims a latent sector error at a
file of a service's storage. The next section says how.

## Aiming a latent sector error at a file

A storage engine's rules name files and offsets, and the disk knows only sectors. So `G=` gains
two kinds, `bad` and `rot`. They choose a file and a range of its bytes as `zero` does (`len=`,
default 4,096 bytes, at a place that `range=` and `seed=` decide). At the stop before step
`<at>` the controller finds the sectors under that range. It sends the engine the new command
`DISK lse=...`, which adds them as bad sectors from that stop on.

### Options for the mapping

1. **The `FIEMAP` ioctl in the guest.** The kernel answers it for every file system. The image's
   busybox has no applet that makes the call (no `filefrag`, and its `hdparm` has no `--fibmap`).
   A new agent command would make it. But a new agent moves the agent pin and every image's
   bytes, which AGENTS.md forbids for an opt-in feature.
2. **A pinned helper program in every image.** Every image's bytes would move again.
3. **The controller reads ext4 itself, through the agent's existing `read`.** The controller
   runs `sync` and asks busybox `stat` for the file's inode and device. It reads the partition's
   first sector from `/sys/dev/block/<major>:<minor>/start`. Then it reads the superblock, the
   group descriptor, the inode and the extent tree from the partition's device file.

### Choice: option 3

It changes no agent and no image. It is deterministic for the same reason as `G=`. The guest is
deterministic up to the stop, so the bytes the controller reads are too. The mapping is a pure
function of them (`sectors::map`, with a unit test over a hand-made ext4 image). The reads go
through the guest's page cache of the device, where ext4 keeps its metadata. So they see the
current tree, also before the journal is written back. Then the controller drops the guest's
clean page cache (`/proc/sys/vm/drop_caches`), so that the next read of the range goes to the
disk, as after a reboot.

It maps ext4 with extents only, which is what hydo-build makes. The controller logs the reason and
changes nothing for any of these:

- a file of another file system, or without extents, or with its data inline in the inode;
- a range in a hole or in an unwritten extent, where Linux reads no sector;
- a file on another disk than the data disk.

## A request that never completes

The guest kernel's virtio block driver has no timeout of its own: blk-mq arms its 30 s timer and
renews it without end. A program that waits on the request (a read, an `fsync`) waits in an
uninterruptible sleep. In the example `disk-faults`, the careless store's step waited on its
write until the workload's timeout of 60 s. Then the agent killed the workload's shell
(`rc=143`). Later flushes queue behind a held flush, so a check that syncs the same data waits
too. Two rules end such a run with a clear verdict instead of the engine's exit budget:

- **The workload's timeout.** A workload that times out in a run with `hang=<k>` ends the run at
  once with `VIOLATION [disk.hang]`. The verdict says that the workload's steps did not end
  within their timeout while the data disk held request `<k>`. A system that sets its own
  deadline for disk calls finishes its steps and passes. The example's careful store does its
  disk work in a child process, waits for it at most 5 s, and does not acknowledge the step.
- **The guest's 120 s.** The engine counts how long the guest has waited on a request that never
  completes. Linux's own hung-task report also waits 120 s. At 120 s of the guest's time the
  engine ends the run with `ERROR <phase>: the data disk never answered a request (hang=)`. On
  Redis, `hang=30` held a flush, and the check's read waited on it: `ERROR check: ...`. The clock
  of an idle guest jumps to its next timer, so the 120 s cost little wall time.

`hang=<k>+<ms>` is the form that most systems can pass. The request fails with an I/O error after
the delay, as a disk does after its own retries. The program must then handle the error from
`pwrite` or `fsync`.

## The explorer

`hydo explore --block-faults` draws one block fault in 60% of the schedules, from its own random
stream. So an exploration without the flag draws exactly the schedules it drew before. It draws a
torn write only for a schedule with `disk=cut` and a power cut. It draws a latent sector error
under a file only for a service with `storage:`. It follows the manifest's `faults:`: `cut` for a
torn write, `disk` for the others, and `corrupt` for `G=`. The shrinker leaves the token out and
turns `torn=<p>:any` into a prefix. A failure with a block fault has the class `damaged-disk`.

## What hvx would need

hvx's virtio block device is in its kernel module (`hv/hvx/hvx.c`). So each fault there is a
module change on a side module first, as AGENTS.md says:

- A torn write. `HVX_SET_UNSYNC` already hands the volatile cache to the engine at a power cut.
  The engine could apply a seeded part of each entry before it drops the rest. The cache holds
  writes of up to 8 KiB only, so a longer write is kept whole today.
- A misdirected write and a held request. Each is a new fault class in the module's request
  loop, with the request number. A held request also needs a queue of held descriptors in the
  snapshot.
- Bad sectors. The module needs a set of sectors that it checks on every read and keeps in the
  snapshot. It carries the set into the recovery boot with the data partition, and an ioctl adds
  to it at a stop.

Each needs its part of the module's snapshot and fingerprint, inert while unarmed, so that every
recorded hvx value stays.

## What is left

Open item 160 in AGENTS.md lists what is left: hvx, a test of CI on a real KVM host, the added disks
(`guest.disks`), the mapping for other file systems, and the explorer's draw of raw `lse=`.

# Devices: a reboot of the guest and disks with caches of their own

This page explains two additions for testing embedded devices (Milestone 263). The first is a
reboot that a guest program asks for. The second is a disk beside the main one, with a write
cache of its own. Both come from the software-update examples of Milestone 259
(`hydo/onboard/examples/rauc` and `swupdate`). There, the device rebooted with a script inside the
running guest, and one disk stood for every storage chip of the device.

Both additions are opt-in. A manifest without them gives the same commands, the same transcripts,
and the same fingerprints as before.

## A reboot that the guest asks for

### What a manifest declares, and how a program asks

`guest.reboot: true` (a format-1 addition) lets the guest reboot itself. A program asks for a
reboot with one line on the console:

```sh
printf 'HYDO-REBOOT\n' > /dev/console
```

The program then waits to be stopped, as a program that calls `reboot` on a device does. The
line may carry a reason after a space, for example `HYDO-REBOOT health check failed`. hydo
writes the reason into the run's log.

hydo does not watch `reboot(2)` itself. The guest agent is the guest's first process, and a change
to it changes the bytes of every image and every recorded fingerprint. A device program that
calls `reboot` needs a one-line wrapper that prints the line instead.

### When hydo takes a reboot

The host controller acts on the line at the engine stop where it sees it. It takes a reboot at
these points:

- during a workload step;
- during the workload's retry, after the last step;
- while the services start after a reboot or after a power cut, when a readiness check fails;
- in the recovery boot after a power cut, once the services started, before the checks.

A line at any other point (the provisioning before the snapshot S1, the checks) is written to the
log and does nothing. A run takes at most 64 reboots. One request more ends the run with
`ERROR reboot loop`.

With `guest.reboot: true`, each workload step runs as its own call of `workload.run`, as with
`perf:`. So a reboot ends one step, and the run goes on with the next step in the new boot. A
step that a reboot ended is not run again. The client of the example asks for the reboot at the
end of a step and checks the booted version in the next step.

### The clean shutdown

A reboot is a clean shutdown of the guest, followed by a new boot of the same machine:

1. The controller stops its periodic checks and the restart supervisor.
2. It sends SIGTERM to the workload's process tree and to every service's tree. After 2 s of
   guest time it sends SIGKILL to them.
3. It runs `sync` and then flushes every disk of the guest (`busybox sync -d /dev/vdX` for each
   disk, which sends the disk a cache flush). A device's kernel does the same at shutdown: the
   eMMC and SCSI drivers flush the device's cache.
4. It reads the run's files (`$HYDO_ACKED`, `$HYDO_OUTCOMES`), the guest's clock and which
   reactions (`A=`) have acted, and ends the conversation with the engine's `REBOOT`.

The disk keeps what the guest wrote back in step 3. A write that a program never wrote back to
the disk is lost, as on a device: the page cache and the kernel are gone.

### The new boot

The engine starts a new VM from the snapshot S0, the state after the agent's start and before the
data disk is mounted. The disks keep their bytes. This is the start of the recovery boot after a
power cut, with two differences: nothing is lost from the disks' caches, and the workload goes on.

The new conversation (`reboot <n>` in the run's log) then:

1. sets the clock to the clock at the shutdown;
2. writes back the run's files;
3. mounts the data disk and starts every service (each service's `setup:` runs again, as at a cold
   boot);
4. goes on where the reboot came. That is the next workload step, or the end of the run after a
   reboot in the retry, or with the checks after a reboot in a recovery boot.

### Faults across a reboot

A reboot ends every fault that is active at the shutdown. The new boot starts every service and a
new kernel, so a killed or paused service runs again, and a network fault is gone. The rules:

- A fault whose step comes before the reboot's next step is not applied again. Its end (a
  restart, a continuation, a heal) is not applied either. The log names each such fault once.
- A fault whose step comes at or after the next step applies as in a run without a reboot. This
  includes a power cut `C=` and the storage faults `G=` and `L=`.
- A reaction (`A=`) that had not acted watches the new boot from its first step.
- The disk faults of the schedule (`disk=`) stay in force. The engine sets the disk's fault policy
  again in the new VM, from the same seed.
- A power cut in a later step works as before: its recovery boot starts from S0 with the disks'
  flushed bytes.

These schedules are refused before any VM, with a message. The explorer counts them as refused,
never as failures. They are `cf=` (a cut at the k-th flush counts
the flushes of one VM), `U=` upgrades, test compositions, `perf:`, `fakes:`, `proxies:` and
`emulators:`. Each of them carries state across a boot that the first version does not carry.

### What the engines do

The lock-step protocol gets one command, `REBOOT`. The engine answers `OK reboot at=<n>`, and its
next stop is the new VM's first (`why=start`, `phase=workload`). The engine:

- takes the disks' flushed bytes, as at a power cut;
- restores S0 into a new VM with those bytes;
- arms the schedule's host part again (the disk fault policy, the tick jitter on the Mac and
  KVM);
- adds the ended boot's fingerprint into the run's fingerprint.

The fingerprint of a run with reboots is the FNV-1a hash of each boot's fingerprint in order,
with the last boot's as before. A run without a reboot gives the same fingerprint as before.

The control transcript has one conversation per boot. The run's control hash (`ctl=`) covers
every conversation in order. Without a reboot it is the same as before.

The engines need no change of the hvx kernel module: the hvx engine's recovery boot already
restores S0 into a new VM with a disk image.

## Disks with caches of their own

### What a manifest declares

`guest.disks` (a format-1 addition) adds disks beside the main one:

```yaml
guest:
  disks:
    - name: emmc
      size_mib: 64
      cache: volatile     # the default
    - name: nor
      size_mib: 1
      cache: none
```

- `name`: a letter followed by letters, digits and `-`, at most 16 bytes.
- `size_mib`: 1 to 4096.
- `cache`: `volatile` keeps every write in the disk's cache until a cache flush of that disk;
  `none` makes every write durable when the disk completes it, as on a NOR flash chip.

The guest sees the disks after the main disk, in the manifest's order: `/dev/vdb`, `/dev/vdc`,
and so on. Each starts with zero bytes. A program in the guest writes their first contents, for
example a device's factory step in the first boot.

### What a flush and a power cut do

Each disk has its own cache. A flush that the guest sends to one disk makes only that disk's
cached writes durable. So an `fsync` of a file on `/dev/vdc` leaves `/dev/vdb`'s cache as it was.

A power cut is one event for the whole machine. At a `C=` cut, a reaction's cut, or a `cf=` cut,
every disk with `cache: volatile` loses its cached writes, whatever the schedule's `disk=` says.
The cache is a property of the device, not a fault. The schedule's `disk=` faults (`ioerr`,
`lying`, `torn`, and the main disk's cache under `disk=cut`) apply to the main disk only.

### The faults of one service's storage

`G=` (corrupted bytes) and `L=` (a power loss of one service's storage) name a service. They act
on that service's `storage:` file system, which is on the main disk. They do not reach the added
disks in this version. A later form can name a disk: `G=disk:emmc@<at>` would flip bits in one
block of the disk's durable bytes, and a power loss of one disk would drop its cache alone.

### What the engines do

The Mac and KVM engines share the virtio-mmio devices of `hydo_hvm::virtio`. An added disk is one
more virtio-mmio block device after the main disk and the network device. On the Mac it gets a
node in the device tree, and on KVM a `virtio_mmio.device=` entry on the kernel's command line and
its own interrupt line. Its state, its bytes and its cache are in every snapshot and in the
fingerprint. A machine without added disks has the same device state and the same fingerprint as
before.

The hvx engine's block device lives in the kernel module (`hv/hvx/hvx.c`, `struct hvx_vblk`), one
per VM. An added disk there needs a second device in the module. It needs its PCI function, its queue,
and its cache. It also needs its part of the snapshot and of the fingerprint, and a second disk
window for the engine.
That is a module change on a side module first. Until it is built, the hvx engine refuses an image
with `guest.disks` before any VM.

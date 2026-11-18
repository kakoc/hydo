# The block faults on a careless and a careful record store

This example shows the block faults of Milestone 313 (`hydo guide faults`, section "Faults of the
disk's blocks"; `docs/design/block-faults.md`) on recs, a small record store in
`src/main.rs`. recs keeps 16 keys in a file of 4 KiB slots and writes them in place with
`O_DIRECT`. Step `i` of the workload syncs and acknowledges the record of step `i - 1`, then
writes the record of step `i`, so one write is in flight between two steps. Its check reads every
key back and requires the acknowledged record, or a record written after it, byte for byte.

- `hydo.yaml` runs the careless variant: one copy of each record, no checksum, and the errors of
  `pwrite` and `fdatasync` ignored.
- `hydo-careful.yaml` runs the careful variant: two copies of each record (regions A and B), each
  with a CRC-32 over the slot, its key, and its region. A record is acknowledged only after both
  copies were synced without an error, and a read takes the newest valid copy. A step does its
  disk work in a child process and waits for it at most 5 s.

## The runs

All runs used the Mac engine on 2026-11-18, with `hydo` built from this tree, the images built
with `./build.sh` and `hydo build` (careless `984d8ec5…`, careful `b90d8941…`). Each schedule ran
twice. Both runs of each schedule gave the same fingerprint and the same control transcript.

| schedule | careless | careful |
|---|---|---|
| `disk=none dseed=1 \| run` | OK (`b7fb6c3f…`) | OK (`8868e032…`) |
| `disk=cut dseed=1 \| run C=8` | OK (`5dd6c4c1…`) | OK (`d2da3cfd…`) |
| `disk=cut dseed=1 torn=100 \| run C=8` | VIOLATION (`927ce795…`) | OK (`ad89fc28…`) |
| `disk=cut dseed=1 torn=100:any \| run C=8` | VIOLATION (`9a45d113…`) | OK (`1e26210e…`) |
| `disk=none dseed=1 mis=5 \| run` | VIOLATION (`182580ae…`) | OK (`39b63e3c…`) |
| `disk=none dseed=1 \| run G=recs@6:bad,range=114688-118784,file=records` | VIOLATION (`89148f32…`) | OK (`2ed7a14a…`) |
| `disk=none dseed=1 \| run G=recs@6:rot,range=114688-118784,file=records` | VIOLATION (`1efa063e…`) | OK (`f0343e7d…`) |
| `disk=none dseed=1 hang=10+1000 \| run` | VIOLATION (`59d4f58f…`) | OK (`261710f6…`) |
| `disk=none dseed=1 hang=10 \| run` | VIOLATION `[disk.hang]` (`b3f0cd15…`) | OK (`463bd9ae…`) |

What each fault did, from the runs' `one.block-faults.txt` and their checks:

- **The plain power cut** loses the write in flight whole, so the key keeps its acknowledged
  record and both variants pass.
- **A torn write.** At the same cut, `torn=100` kept 4 of the 8 sectors of the write in flight
  (`torn write at sector 45816: 4 of 8 sectors kept`). The careless store read a record whose
  first half is new and whose second half is old. The careful store found the CRC of copy A
  wrong and read copy B.
- **A misdirected write.** `mis=5` sent the fifth write, the record of step 4, 104 sectors too
  low (`write 5 meant for sector 45792 went to sector 45688`). The careless store acknowledged it
  and then read the old record: `key 4: acknowledged value 5, reads no value`. The careful store
  read copy B, and refused the stray copy, whose key is not its slot's.
- **Bad sectors under a file.** `G=recs@6:bad` made the 8 sectors under key 4's slot fail
  (`BAD SECTORS under recs's /data/recs/records.dat bytes 114688..118784: lse=45792+8`). The
  careless read failed: `key 4: read of region 0: I/O error (os error 5)`. With `:rot` the read
  returned damaged bytes, which the careless store returned as a record. The careful store read
  copy B in both runs.
- **A request that completes late with an error.** `hang=10+1000` failed the record write of
  step 4 after 1 s. The careless store ignored the error and acknowledged the record, which never
  reached the disk. The careful store saw its write fail and did not acknowledge it.
- **A request that never completes.** With `hang=10` the careless step 4 waited on its write without
  end. At the workload's timeout of 60 s the agent killed it (`rc=143`), and the run ended with
  `VIOLATION [disk.hang] workload steps 0..11 did not end within their timeout of 60s, and the
  data disk never answers its request 10`. The careful step gave up after 5 s, acknowledged
  nothing more, and the workload ended in time.

A raw latent sector error from the run's start, `lse=45792+1`, did nothing: step 4 rewrote the
sector, and a write of a whole sector repairs it, as on a disk.

## The explorer

`hydo explore --seeds 1-20 --block-faults` drew a block fault in 10 of the 20 schedules (a torn
write, `mis=`, `hang=`, and `G=...:rot`). On the careless image 3 runs failed: a
`hang=4` (class `damaged-disk`), a lying disk, and an I/O error rate with `mis=3`, which shrank to
`disk=none dseed=aeaf52febe706064 mis=3 | run`. On the careful image no run failed.

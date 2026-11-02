# etcd example — results (2026-10-21/22, bare-metal test machine)

Images built by `examples/etcd/build.sh <version>` on the build box with the pinned toolchain
(`hvx_seedrng` `c2a440c5…`, docs/BUILD-PINS.md), run with `hvx_explore` / `explore.py` from this
branch, one VM at a time. Results under `/var/tmp/m59-etcd` on the box (`standin/` holds an
earlier pass whose images carried the stand-in `hvx_seedrng` `01adf1bd…`; superseded).

| image | etcd tarball sha256 | disk image sha256 |
|---|---|---|
| `explore-ob-etcd-3.6.15-disk.img` | `b51d86e6…` | `2f655314b9ff8bc8…` (two builds identical) |
| `explore-ob-etcd-3.5.5-disk.img` | `7910a2fd…` | `bb5f8b79db42e3d8…` |
| `explore-ob-etcd-3.5.6-disk.img` | `4db32e3b…` | `7f63c2848aab3e6d…` |
| `explore-ob-etcd-3.6.8-disk.img` | `cf9cfe91…` | `5b32d9e3aa48eda2…` |
| `explore-ob-etcd-3.6.9-disk.img` | `633136f1…` | `a4a5c641007eb28c…` |

## Clean runs (`disk=none dseed=1 | run`), twice each

| version | verdict | fingerprint (both runs) |
|---|---|---|
| 3.6.15 | OK, acked=90, 180 observations monotonic, members agree | `c42a0dfcf10a9655` |
| 3.5.5 | OK | `56f01a9f4404c4b5` |
| 3.5.6 | OK | `86970013d37351c1` |
| 3.6.8 | OK | `e6409067a40cc2d5` |
| 3.6.9 | OK | `084054bfa1889a31` |

## Link faults work (3.6.15, power cut at the end so the acked list is the workload's own)

| schedule | acked of 90 | fingerprint |
|---|---|---|
| `disk=cut dseed=1 \| run C=30` | 90 | `d676b5aa8a1c10a2` |
| `… N=etcd1-*@0-30:loss,100% C=30` (etcd1 cut off from both peers) | 58 | `3ffac52830636849` |
| `… N=etcd1-etcd2@0-30:loss,100% C=30` (only the 1–2 link) | 60 | `6b0c3addee3d1507` |

Clients connect from 127.0.0.1 and are not filtered, so the lost acks come from the members'
own traffic: the per-uid source routing puts each member's address on its packets.

## Explorer: `--strategy process --new-faults --timing`, seeds 1-100, 1 worker

| version | failures | reproduced | finding |
|---|---|---|---|
| 3.6.15 | 0 | – | – |
| 3.5.5 | 19 | 19/19 | 18 × `etcd.members_agree` (members end on different revisions), 1 × `client.revisions_monotonic` |
| 3.5.6 | 0 | – | – |
| 3.6.8 | 0 | – | – |
| 3.6.9 | 0 | – | – |

Minimal reproducers on 3.5.5 (each run twice, identical):
- `disk=cut dseed=4127cc1347fae6ba | run C=30` → `members disagree: revisions 99 99 98` (`b802065d149ebda2`).
  The cut lands right after the defrag that ends step 29.
- `disk=none dseed=890abaea1c4d92d3 | run K=etcd1@10+1 P=etcd3@26+1 T=2:-30` → client 2 read revision
  83 through etcd1, then 82 through etcd2 (`20ec47337eaf03f8`). etcd1 was killed right after the
  defrag of step 9 and replayed entries on restart, so its revision runs one ahead; a client
  that switches members sees the revision go back. Same bug, second symptom.

This is etcd issue 14685 (fixed in 3.5.6): defrag commits applied entries without updating the
consistent index, so after a crash the member re-applies them and its revision diverges.
On the same 100 schedules 3.5.6 has no failure.

Hand-written check (kill each member right after each defrag, `K=etcd<n>@<5,10,15,20,25>+2`):
3.5.5 5 of 15 violate `etcd.members_agree`, 3.5.6 0 of 15.

3.6.8 vs 3.6.9 (stale reads caused by process pausing, etcd issue 20418): no difference, 0
failures on both. The process strategy pauses a member only between workload steps
(`P=svc@at+n`), so `fault.pause_mid_request` was never exercised; the reported bug needs a
member (the leader) to stall while requests are in flight. Timed pauses (`P=svc@at~off:dur`,
drawn by `--new-faults` under the random/swarm strategies) are the next thing to try.

Cost: 20–37 s per schedule (the box was shared with other sessions), 0.7–2.2 h per 100 seeds.

## Rebuilt with the netem drain (2026-10-23, M77)

The images above were built before Milestone 71, so lifting a link or network fault dropped the
packets waiting in netem's delay queue. They were rebuilt from `2e0e3fe` with the pinned tools as
`explore-ob-etcd-<v>-nq-disk.img` (3.5.5 `2d4d9301…`, 3.5.6 `1df90f16…`, 3.6.8 `bf213ea5…`,
3.6.9 `fd2e9c58…`; 3.6.15-nq `021d350a…` is from M71); only `/sbin/init` and
`/etc/hydo/init-links.sh` differ. Use the `-nq` images for new runs. Full tables (every schedule
above plus link-heal schedules, old image vs new image): `docs/REBASELINE.md` section 7.

| version | clean run (`disk=none dseed=1 \| run`), new image | M60 explorer campaign, new image |
|---|---|---|
| 3.6.15 | `b252dd1dac5b1449` (M71) | not re-run |
| 3.5.5 | `4baa570bd70d0ae7`, OK | 17 failures, 17 reproduced, all `etcd.members_agree` (was 19) |
| 3.5.6 | `abface2f2b610673`, OK | 0 failures |
| 3.6.8 | `875de1a790776cff`, OK | not re-run |
| 3.6.9 | `ec25ed73287912f2`, OK | not re-run |

On 3.5.5 the bug is still found, but which schedules hit its window moved (guest timing shifted
with the longer init script): the first minimal reproducer `disk=cut dseed=4127cc1347fae6ba | run C=30`
still fails (`2bd06b8446559fe9`, members disagree 99 98 98), the second one
(`K=etcd1@10+1 P=etcd3@26+1 T=2:-30`, client revision going back) now passes (`73546ff456569eba`),
and the hand-written check fails in 3 of 15 schedules (`K=etcd<n>@25+2` for each member) instead of
5 of 15. 3.5.6 passes everything on both images. Every heal on the new images delivered the queued
packets (4–41 on the link-delay schedules, `left=0`).

## The second symptom of 14685 on the `-nq` image (2026-10-24, M78)

On `explore-ob-etcd-3.5.5-nq` M60's second reproducer (`K=etcd1@10+1 P=etcd3@26+1 T=2:-30`) no
longer fails. A hand sweep of its variants found it again; the clock step is not needed:

    disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1

| image | run | verdict | fingerprint |
|---|---|---|---|
| 3.5.5-nq | fresh boot (`hvx_explore one`), twice | VIOLATION `client.revisions_monotonic`: client 2 read revision 83 through etcd1 (get k77, step 25), then 82 through etcd2 (get k80, step 26); the final check also reports `members disagree: revisions 101 100 100` | `0470512bb297c87f` both times |
| 3.5.5-nq | from the S1 snapshot (`run_list.py`, removed in M108; `hydo-bench list` runs a list the same way) | the same | `0470512bb297c87f` |
| 3.5.6-nq | fresh boot, twice | OK, acked=90, every invariant holds | `63e112769072c0c1` both times |
| 3.5.6-nq | from the S1 snapshot | OK | `63e112769072c0c1` |

The details are the same as M60's (83 through etcd1, then 82 through etcd2). With `dseed=890abaea1c4d92d3`
the same two faults give the same violation with fingerprint `a9be9129d8cc0957` (two runs).

The sweep (68 schedules on 3.5.5-nq, from the S1 snapshot, 2 workers, about 15 minutes):
- kill step 7–13 × pause step 23–29, with `T=2:-30`: 1 of 49 fails, `K=etcd1@10+1 P=etcd3@29+1 T=2:-30`
  (client 2, 92 then 91);
- the clock step moved to step 0, 1, 3, 4, 5, 6, 10, 15, 20 or 25 (kill at 10, pause at 26): 3 of 10
  fail (T at step 3, 4 and 6, all with M60's details);
- the clock step dropped: fails (above).

Each part alone passes: `K=etcd1@10+1` alone (members agree, 98 98 98) and `P=etcd3@26+1` alone. With
the kill at step 10 and a pause of another member late in the run, etcd1 ends one revision ahead:
pausing etcd2 at step 26 or etcd3 at step 25 gives `members disagree` only, pausing etcd3 at step 26
(for one or two steps) or 27 also makes a client's revision go back (at step 27 it is client 1,
86 then 85), and pausing etcd1 itself gives no failure. Why the one-revision lead appears only
together with the later pause was not investigated. On 3.5.6-nq all six failing schedules of the
sweep pass. Results: `/var/tmp/m78-etcd` on the box (`A`, `B`, `D`, `E` = `run_list.py` outputs,
`runs/one5*` = the fresh-boot runs, `results.txt`).

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**x86_64 (hvx), etcd-3.5.5**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `ed50aa191ffd0a39` | yes |
| `disk=cut dseed=1 \| run C=30` | VIOLATION [etcd.members_agree] members disagree: revisions 98 99 98; k89 mod_revisions 97 97 97 | `200822da59c2ec72` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `3b47960fb89dd908` | yes |

**x86_64 (hvx), etcd-3.5.6**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `fe7fa0444ca89f70` | yes |
| `disk=cut dseed=1 \| run C=30` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `b95213ade20090b2` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `220a997535814ade` | yes |

**x86_64 (hvx), etcd-hc-3.5.5**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `edec19943cb68c42` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | VIOLATION [etcd.kv_linearizable] key k31: these 4 operations cannot be ordered consistently with real time ... | `2367445439ec5bc8` | yes |

**x86_64 (hvx), etcd-hc-3.5.6**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `f9142657ac4708ac` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable) | `55e6b1dda9a9b14e` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port (the current values)

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**x86_64 (hvx), etcd-3.5.5**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `c2f71429accf734c` | yes |
| `disk=cut dseed=1 \| run C=30` | VIOLATION [etcd.members_agree] members disagree: revisions 99 99 98; k89 mod_revisions 96 96 96 | `97fd98642e8f2614` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `5d0dde7808bdb377` | yes |

**x86_64 (hvx), etcd-3.5.6**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `0428c99be308463e` | yes |
| `disk=cut dseed=1 \| run C=30` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `347369b1bc71618b` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `0c3cae8d4892f38f` | yes |

**x86_64 (hvx), etcd-hc-3.5.5**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `145703f206f2a797` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable) | `e91c242f589bb74c` | yes |

**x86_64 (hvx), etcd-hc-3.5.6**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `fe88a223de851b5a` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable) | `3c036a3f62a06536` | yes |

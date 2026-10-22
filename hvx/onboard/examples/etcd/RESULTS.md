# etcd example — results (2026-10-21/22, bare-metal test machine)

Images built by `examples/etcd/build.sh <version>` on the build box with the pinned toolchain
(`hvx_seedrng` `c2a440c5…`, hvx/BUILD-PINS.md), run with `hvx_explore` / `explore.py` from this
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

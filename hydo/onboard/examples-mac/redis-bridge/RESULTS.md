# redis-bridge on the Mac backend (Milestone 99)

`hydo.yaml` here is `hydo/onboard/examples/redis-bridge/hydo.yaml` with the aarch64 Redis 7.2.4 of
`../redis/hydo.yaml`. `network: bridge` makes `hydo-build --arch arm64` build it for the second
guest kernel, `Image-explore-netns` (`806807e7…`), and record that in the image
(`/etc/hydo/kernel`); `hydo run` and `explore.py --backend hvm` boot that kernel by themselves.

Built on the M2 Pro through a release-like prefix (the M96 Mac release prefix with this branch's
`hv/hvx/`, `bin/hydo`, `hvm`, `hvmx`, both kernels, `guest/hydo_fc` and the aarch64 `ip` and
`iptables-legacy` files in `guest/tc`), from a copy whose `src:` paths point at the release's
`examples/redis/files`. Two builds, identical: image `e066eeee…`, disk `26f4a507…`.

Every schedule twice from a fresh boot (`hydo run`), both fingerprints equal; all OK:

| schedule | what the console shows | fingerprint |
|---|---|---|
| `disk=none \| run` | the replica's lag is 0 or 30-32 bytes after every step | `307efc476cddb5c4` |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | netem on `vh2` and `vh1`; the lag grows 60, 90, ... 244 bytes over steps 5..11 and is 32 at step 12; the heal reports `vh2:sent=0,dropped=5 vh1:sent=0,dropped=5` | `a08d989df70a3e3b` |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | netem on `vh2` only (primary -> replica); `dropped=10`; the lag grows to 182 and is 0 at step 11 | `edaf43b9b66397cc` |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | netem on `eth0` inside the primary's namespace; `eth0@primary:sent=34`, 1 packet queued at the heal and drained in 290 ms | `8fb45a90311f776f` |
| `disk=none \| run K=replica@5+3` | the replica killed before step 5 (lag `none`), restarted in its namespace before step 8, lag 30 at step 8 | `310b32418083a8e7` |
| `disk=cut \| run C=8` | power cut before step 8; the recovery boot brings both servers up in their namespaces and the replica catches up 2.01 s after the heal; 8 acknowledged on the primary | `d761c4b1ac47b64d` |
| `disk=none \| run F=replica<primary@5#3:2000` | `hydo-fc: FROZE ... at resp #3 ... SET k:7 v7` inside the replica's namespace (the same packet as on x86_64, M95); the lag grows to 154, then 0 after the resume | `953bf35244e9367e` |
| `disk=none \| run F=primary<workload@5#2:1000` | the primary frozen at the workload's second packet of step 5 (from 10.89.0.1) for 1000 ms | `d233a89ad7decda7` |
| `disk=none \| run H=redisp@3-6:drop` | the proxy drops the answers of requests 4..6; 13 of 16 acknowledged | `b6fe2e795a0a5fdb` |

From the S1 snapshot in `hvmx serve` (`hydo explore --schedule`), the clean, link, cut and freeze
schedules give the same four fingerprints as the fresh boots.

`hydo explore --strategy process --new-faults --freeze-call --protocol-faults --seeds 1-30
--workers 3`: 30 of 30 OK in 27 s; 8 schedules drew the link `primary-replica` (5 `N=`, 3 `O=`),
12 a freeze, 19 a proxy fault; the heals show the faults acting (for example
`vh1:sent=37,dropped=16`, `vh2:sent=0,dropped=4 vh1:sent=0,dropped=3`), and freezes landed on
replication packets (`REPLCONF ACK`). The report's summary names `guest_kernel
Image-explore-netns (sha256 806807e7be5f55da)`.

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**Mac (hvm), redis-bridge**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `8fd9e1576dee25c4` | `14af985fafa6a0ca` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c42fb828cb40ea69` | `b9b191afe8ff347f` | yes |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `022f2184ab03958f` | `4f0e58064c0510be` | yes |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c2bc92c9a0b26ea4` | `72f5aa1bb200a63b` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `2f0c2e49fb70d37a` | `6a8c64c25c180c74` | yes |
| `disk=cut \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `bd3d67ef875fb289` | `0f774df0f3a9ecb2` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c5468c557f44330f` | `1993b9669f47ad4e` | yes |
| `disk=none \| run F=primary<workload@5#2:1000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `e2d9ddbf12753896` | `db19e485a83060b1` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `9da67ba88882b5d5` | `01634ac3903ea950` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port (the current values)

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**Mac (hvm), redis-bridge**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `dddc2303d3d42ccf` | `6a8e130bfbf9fef2` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `79d0a7f4e0e3f363` | `bb902d00160d7286` | yes |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `8efe5abed105a3a9` | `f2d609514474f032` | yes |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `763b91f2b44aae1f` | `ab4e21b1c8e8ce83` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `501b262abcf67878` | `38e15c68d9963cda` | yes |
| `disk=cut \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `aa1c35b41a917b4e` | `1ac708d19e5718f8` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c5d5ec021d56f506` | `db293b879f216430` | yes |
| `disk=none \| run F=primary<workload@5#2:1000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `80ce5234857338c7` | `4ff9110839ed6dfd` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `94e11ee5fff0c8ce` | `a31807d7688f461c` | yes |

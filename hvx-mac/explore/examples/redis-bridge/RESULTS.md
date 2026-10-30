# redis-bridge on the Mac backend (Milestone 99)

`hydo.yaml` here is `hvx/onboard/examples/redis-bridge/hydo.yaml` with the aarch64 Redis 7.2.4 of
`../redis/hydo.yaml`. `network: bridge` makes `hydo-build --arch arm64` build it for the second
guest kernel, `Image-explore-netns` (`806807e7…`), and record that in the image
(`/etc/hydo/kernel`); `hydo run` and `explore.py --backend hvm` boot that kernel by themselves.

Built on the M2 Pro through a release-like prefix (the M96 Mac release prefix with this branch's
`hvx/`, `bin/hydo`, `hvm`, `hvmx`, both kernels, `guest/hydo_fc` and the aarch64 `ip` and
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

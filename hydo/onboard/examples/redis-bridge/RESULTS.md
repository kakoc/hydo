# redis-bridge: results (Milestone 95)

The control project for `network: bridge` (MANIFEST.md, "Service networks"). Two unmodified
Redis 7.2.4 servers (the pinned static build), each in its own network namespace and both on
port 6379: `primary` (10.89.0.11, append-only file, `appendfsync always`) and `replica`
(10.89.0.12, replicating from `primary` over the bridge, no persistence). Their `ready:` checks
ask `redis-cli` on localhost, which inside a namespace is the service itself. The workload writes
`SET k:i v<i>` through a `tcp` fault proxy of `hydo-world` on the bridge's own address
(`redisp` = 10.89.0.1:6380) and prints the replica's lag (the primary's replication offset minus
the offset the replica acknowledged) after every step.

## Build

On the bare-metal test machine, through the Linux release's `hydo build` (no Nix; installed in
`~/hydo-netns/rel` from `~/hydo-lrel/final`, overlaid with this branch's `hydo/onboard`, and with
the guest kernel's `llc`, `stp`, `bridge` and `veth` modules added; see Milestone 95 in
`docs/HISTORY.md` for where those came from). `hydo-world` was built on the Mac with Go 1.26.5
(`CGO_ENABLED=0 -trimpath -buildvcs=false -ldflags="-s -w -buildid="`), sha256 `91eb37c1…`.

- `explore-ob-redis-bridge-disk.img`, sha256 `ab6847dfd275cbc8…` (root `9860e738…`, data `89c804a4…`).

## Runs

Every run is a fresh boot, `hvx_explore one` (built from this branch), main `hvx.ko`
(srcversion `CA5BF8BF2A0D66B312A68EF`), 1024 MiB. Each schedule ran twice and both fingerprints are
equal. All verdicts are OK; the replica caught up within the 30 s deadline in every run.

| schedule (`disk=none dseed=1 \| run ...`) | fingerprint | what the console shows |
|---|---|---|
| (none) | `ff28a3ff4f50a9e3` | lag 0 or one write behind after every step; 16 of 16 writes acknowledged |
| `N=primary-replica@5-10:loss,100%` | `cfc7d03f4c2e29bd` | `LINK FAULT ... on -:vh2 -:vh1`; the lag grows 60, 90, 120, 150, 194, 226, 228, 260 bytes over steps 5..12; the heal reports `vh2:sent=0,dropped=5 vh1:sent=0,dropped=5`; lag 0 at step 13 |
| `O=primary-replica@5-10:req:loss,100%` | `eb6be40436485932` | only primary -> replica: `on -:vh2`, `dropped=10`; the same lag curve |
| `O=primary@5-8:resp:delay,300ms` | `e4b55b516a4bf88f` | the delay sits on the primary's `eth0` inside its namespace: `eth0@primary:sent=36`; 1 packet queued at the heal, drained in 210 ms, 0 lost |
| `K=replica@5+3` | `98ecf8aa3094f306` | the replica is killed before step 5, restarted in its namespace before step 8, ready after 1 poll, resynced |
| `F=replica<primary@5#3:2000` | `0d3e5df9c9bcfff9` | `hydo-fc: FROZE pid 179 ... at resp #3 (6379>57900 len=30) ... SET k:7 v7`: the helper and its netfilter rules ran in the replica's namespace; resumed after 2000 ms |
| `F=primary<workload@5#2:1000` | `ce449ef5ad9ac3ca` | the freeze's workload peer: the rules match the client's packets from the bridge address 10.89.0.1 in the primary's namespace; frozen at the client's `INFO replication` request |
| `H=redisp@3-6:drop` | `8273569939085966` | `hydo-world` on 10.89.0.1 dropped the answers of steps 3..5 (13 of 16 acknowledged, no violation) |
| `disk=cut dseed=1 \| run C=8` | `a5fbc70c219f6d51` | the recovery boot (restored from S0, so the namespaces are there) starts both servers, the replica resyncs 2.12 s after the start; 8 of 8 acknowledged writes on the primary |

The link fault is the point of the milestone: on one shared loopback network, the replica's
connection would leave from 127.0.0.1 and a link fault between the two addresses would match
nothing (compare `examples/compose-voting`, where the same fault on the Milestone 84 image changed
nothing). Here every packet between the two servers carries their own addresses, so the fault
acts, and the console shows the replication stall and the catch-up.

## Explorer campaign

`explore.py --manifest hydo/onboard/examples/redis-bridge/hydo.yaml --image explore-ob-redis-bridge-disk.img
--strategy process --new-faults --freeze-call --protocol-faults --seeds 1-30 --workers 2` (`HYDO_BUILD`
= the image's directory): 30 schedules, 0 failures, 260 s. The fault space's network targets are
`primary`, `replica` and the one link `primary-replica`; 8 schedules drew a link fault (`N=` or
`O=` on `primary-replica`), and their heal lines show the faults acting on the replication traffic
(for example `acted on: vh1:sent=37,dropped=14`). Other draws: 20 kills, 27 pauses, 23 clock
jumps, 12 freezes, 24 protocol faults, 9 power cuts.

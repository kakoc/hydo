# A benchmark of unexplained failures for debugging tools (Milestone 388)

hydo replays every run exactly, so it can offer debugging tools that a machine without
determinism cannot. The lead's rule is to build only what answers real, recorded problems. This
page collects those problems from hydo's records: every failure whose cause was never found, and
the failures whose cause was found late or first described wrongly. For each one it names the tool
that would have answered the open question, and the data that tool needs. It then ranks the tools
and proposes the order to build them in. Nothing was run for this page, and nothing was built.

## The four tools

The page judges four tools. Each name below is the plain label that the tables use.

| label | the tool | the question it answers |
|---|---|---|
| `stacks` | every thread's stack and held locks at any stop, forward and back in time (`hydo inspect stacks`, being built now) | what is each thread of each node doing, or waiting on, at this moment |
| `watch` | data watchpoints across all processes of all nodes | when did this key, value or file range change, in which thread, at which line of code |
| `divergence` | the first divergence between a failing run and its nearest passing run, at the machine level (messages, system calls, scheduling), with counterfactual branches | where do two runs part, and does the failure stay if one event moves (a message 1 ms later, a fault left out) |
| `path` | a message's path from its send to its apply | where did this request, record or reply go, and which write did it cause |

Two more labels mark cases that need none of the four:

- `logs`: the tools hydo has today answer the question. A service's log was not read, or one
  command in a held run (`hydo inspect exec`) would have given the value. `docs/design/inspect.md`
  describes those tools.
- `none`: none of the four answers it. The table names what would.

A stop is a point where the engine hands control to the host. A held run is a `hydo inspect`
session that keeps a replayed run at one stop.

## How the cases were collected

The sources are `docs/HISTORY.md`, the open items of `AGENTS.md`, every
`hydo/onboard/examples/*/RESULTS.md`, and the findings ledger of Milestone 380
(`docs/design/findings-ledger.md` and its CSV). A search for these phrases found the
candidates:

- not traced, not investigated, not separated, not established, not looked for;
- cause not found, cause was not found, unexplained;
- was not read, two fingerprints, and a sentence that starts with why and ends with was not.

Each candidate was then read in its section.

A case enters the benchmark when it is a failure or a behaviour of a run that the records could
not explain. Hypotheses without a failing run stay out. Every case has a source, so a reader can
check its cell. The time spent is the record's own number where it has one. The time a tool would
save is this page's estimate, and the tables say so.

The cases are 72. 44 are failures of real systems whose cause is open, and 9 are gaps of hydo's
own engines. 19 are cases whose cause was found but took long or was first wrong. Nine more records were left
out, because no tool inside a guest can reach them (the section "Cases outside the guest").

## Open failures of real systems

A "neighbour" is the same schedule without one of its faults. A fingerprint names the end state
of a run; two runs with one fingerprint ended the same.

| id | example, milestone | schedule and fingerprint | symptom and open question | tried, time | tools | data needed | estimated saving |
|---|---|---|---|---|---|---|---|
| A1 | garnet, M371 | `disk=none dseed=0 \| run W=69274d70,p3,q1,r500,oparallel_driver_del A=garnet.bgsave+10:kill-now-g1`, KVM `88ca1c0b695a75fb`, hvx `e12c7c93adf9d46c`; saved regression | the replica `g4` answers no command 120 s after the heal. What does its recovery task wait on? | `hydo inspect diagnose --evidence` read the log: `background recovering task has not completed`; two issues read | stacks, path | `g4`'s managed (.NET) and native stacks and locks at a stop after the heal | the candidate's label; a report |
| A2 | garnet, M371 | `disk=none dseed=0 \| run P=g1@12+3 W=0a39fa3d,p1,q2,r500`, KVM `00af90d009683f21`; neighbour without the pause OK `4239689f3f3ff03e` | two acknowledged writes lost after failovers on both sides of a migration. Which node named which owner of slot 13404 at each failover? | logs only; "as far as the logs show" | divergence, watch, path | both runs' events; each node's slot table | part of the 6 h of directed runs |
| A3 | garnet, M371 | `disk=cut dseed=0 \| run K=g4@22+1 C=40 W=a0075364,p1,q1,r500`, KVM `8bb39f73a77bf0fb`; without the kill OK `23cd71d660070fd1` | after the cut the replica `g1` reports the primary's offset 2,460 and holds other data in 8 keys. Which records did it apply at which offsets? | none recorded | path, watch | each replication record from `g4` to `g1`; writes to the counter key on `g1` | the candidate's label |
| A4 | garnet, M371 | seed 103 of the second exploration, KVM | the recovery boot ended `SPIN`, a loop without VM exits. Where does it loop? | "not looked at" (open item 192) | stacks | the spinning thread's stack | under 1 h |
| A5 | rabbitmq, M370 | `disk=none dseed=0 \| run K=rabbit2@1+3 N=nodes.bridge.6@21-23:loss,100% P=rabbit1@16+11 W=5764a73c,p3,q2,r1000 G=rabbit3@20:flip,n=4,seed=4979bdc4 L=rabbit3@21+3`, `46d06ab61b82f53e`; saved regression | a confirmed stream message is lost; each neighbour passes. Why does the loss need the kill of `rabbit2` before step 1? | neighbours run by hand; logs and code read for the rest of the cause | divergence, path | the runs with and without `K=`; the first coordinator decision that differs | the last open part of upstream #18515 |
| A6 | rabbitmq, M370 | the saved regression with `K=nodes.primary@13+2`, `efcd83d18848454a` | which moments of the kill fall in the window of the parked retry? | the kill before steps 10 to 13 run by hand | divergence, watch | counterfactual kills 1 s apart; the parked retry time | a window map in one call |
| A7 | rabbitmq, M370 | seed 4 of e4, `K=rabbit1@21+9 P=rabbit1@8+1 T=10:-2592000` | `eventually_probe` failed; the record guesses finding 3. Is it? | "its probe's output was not read" | logs, watch | the probe's output; the parked retry time | minutes |
| A8 | rqlite, M369 | `O=rq2@4-6:resp:loss,74% W=ddd2fe8b,p3,q3,r1000 G=rq2@33+2:flip,n=4,seed=32b61fcc`, `e778b8adb3f888be`; without `O=` OK `173ba02d53467c1c`, without `G=` OK `5fba2024ed03e57b` | a queued write `q.k80` is lost. Why does the loss need the one-way loss of steps 4 to 6, and why do kills at steps 12, 18, and 28 pass? | neighbours run by hand | divergence, path | the runs with and without `O=`; the path of `q.k80` | the open part of a known bug (pull request rqlite#3027) |
| A9 | kvrocks, M351 | the grid `--reactive-at kvrocks.failover_done`, cuts 0.5 s to 23 s after a failover | 14 of 30 cells fail; seeds 1 to 10 with the default composition all pass. What do they lack? | "why was not traced" | watch, divergence | the replica's full-sync state and its checkpoint files | 1 h |
| A10 | kvrocks, M351 | seed 40 of e3, `G=` on the master's WAL before step 14, then a cut | the replica `kv2`, whose files were intact, did not start. Did a full sync come first? | "its log was not read" | logs, watch | `kv2`'s log; the sync state | minutes |
| A11 | redpanda, M279 | `K=nodes.primary@66+1 P=n2@42+11`; `A=rp.txn_add@20+3:kill-n1` | a committed transaction is visible on one partition. Why do the runs without faults pass? | trace of the two requests 14 ms apart; "not traced" | divergence, path | where the partitions' leaders part between the two runs | 1 h |
| A12 | redpanda, M304 | ten KVM shards' runs, seed 18 shrunk to `W=06dd08ca,p2,q1,r500,n30,x0-59` | ten runs broke `rp.no-loss` and one `rp.fan-out`; seed 18 fails without a fault | "their causes were not read" | logs, path | the failing runs' logs; the lost records' paths | 1 to 2 h |
| A13 | hatchet, M250 | the probes of finding 1 after kills of an engine | a cancelled run becomes `COMPLETED`. Which message carried this cancellation? | "not traced" | path | the cancellation's message through the dispatcher's queue in PostgreSQL to the worker | the open part of a known class (issue #2655) |
| A14 | hatchet, M250 | seed 2, `K=postgres@4+3` with a partition and a pause | a DAG acknowledged and never completed, and a rerun | "its faults were not separated" | divergence, path | counterfactual runs without each fault; the DAG's next message | 1 h |
| A15 | hatchet, M250 | seed 1, `T=13:-300` and a power cut | `worker_1` exits 1 at every start of the recovery boot | "its log was not read" | logs, stacks | the worker's log; its stack at exit | minutes |
| A16 | tikv, M287 | seed 4 of the first exploration | readers of the locked accounts timed out, where directed runs blocked writers only | "not traced" | stacks, watch | the readers' stacks; the lock's `min_commit_ts` | 1 h |
| A17 | tikv, M287 | seed 4 | `monotonic time jumped back, 52.048 -> 45.589` once on `tikv1`. The guest's clocks, or TiKV's comparison? | "not traced" | watch | the two instants that TiKV compared, and the clock reads behind them | 1 h |
| A18 | clickhouse, M311 | `disk=cut \| run C=40` on the default image (`fsync=0`) | the cut passes on the image that syncs nothing, and fails with `fsync=insert`. Why? | "not traced" | watch | the part files' writes and their write-back to the disk | 1 h |
| A19 | clickhouse, M311 | seeds 27 and 40 of E2, `TIMEOUT workload` | not run again with the exact tick; seeds 27, 33 and 40 not checked for the Keeper loop | not run (open item 157) | stacks | the busy threads' stacks at a few stops | the hand sampling of M325 |
| A20 | pulsar, M356 | `disk=cut \| run C=34`, `0aa2f73c75dcea55` | bookie2's auditor met `BadVersionException` on an under-replication znode and the bookie exited 0. Who changed the znode's version? | "not traced" | watch, path | every write to that znode on the three ZooKeeper servers, with its client | 1 to 2 h |
| A21 | pulsar, M356 | three cuts with `journalSyncData=false`, one 5 ms after a send | nothing was lost. Why did the entry survive without a sync? | "why was not traced" | watch | when the journal's pages reached the disk | 1 h |
| A22 | mender, M276 | a cut 64 ms and 65 to 68 ms after the commit's event, on hvx | 64 ms gives the later form of finding 2, and 65 to 68 ms do not | "not investigated" | divergence | the first event that differs between the cuts at +64 and +65 | 1 h |
| A23 | nats-jetstream, M235 | `disk=cut dseed=4127cc1347fae6ba \| run K=n2@52+20 N=n2-*@294-299:loss,72% C=300 P=n1@145+4 T=191:30 W=3e71cf9e,p1,q2,r1000 L=n1@76+3` | after a cut of every node, one node keeps three unacknowledged messages that the others lack, for good | "not investigated further" | path, watch | the three messages from publish to each node's store | 1 h |
| A24 | nats-jetstream, M245 | seed 2 of the `blocks/` exploration, `G=n3@167:flip,n=4,...` | 4 flips in a middle block of the stream leader lost 4 acknowledged messages, and the replicas agree | "not investigated further" (open item 38) | path, watch | how the replicas came to drop the messages | 1 to 2 h |
| A25 | nats-jetstream, M245 | the first schedule of Jepsen's 3.5 under `L=...:strict` | it passes. The writeback, or the silent links? | "not separated" | none | the strict form's two parts as separate tokens | not a tool question |
| A26 | etcd, M60, M78 | `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1`, `0470512bb297c87f`; at epoch 6 `K=etcd2@25+1 P=etcd1@27+1` | a client's revision goes back. Why does one member's lead of one revision come only with a later pause of another member? | 68 schedules in 15 min (M78); 90 and 30 more at epochs 4 and 5; "not investigated" (open item 8) | watch, divergence | each member's revision and consistent index over time; the runs with and without the pause | four re-searches over four epochs |
| A27 | qdrant-flaky-10200, M208 | the KVM and hvx runs of the flaky test | why does the KVM guest switch threads in the window and the hvx guest not? | "not investigated" | stacks | which thread runs at each stop of the window, on each engine | 1 h |
| A28 | qdrant-flaky-10200, M302 | 2 vCPUs on KVM, starts 390 to 460 ms | 21 of 71 starts ended in ERROR | "the ERROR's cause was not read" (open item 80) | logs | the run directory's diagnosis | minutes |
| A29 | sqlite (`hydo/knownbugs/sqlite`), M85 | the hunt's workload `S=32efe18d` | the WAL restarts 10 to 70 times less often under hydo than natively, which keeps blind hunts unlikely | "not explained" | stacks, watch | where the checkpointers wait; writes to the WAL header | the reason the blind hunt fails |
| A30 | vikunja, M273 | seed 9 under disk errors | the walk was on the login page from step 4 on. Why did the session end? | "not traced" (open item 137) | path, watch | the request whose answer ended the session; the session's row | 1 h |
| A31 | vikunja, M273 | seeds 1 and 8 with `disk=ioerr` | the rule could not read its copy of the database five times | "which read failed was not traced" | divergence | the system calls of the rule's `cp` and their errors | 30 min |
| A32 | memos and web-shop, M267 | a walk with `,rec` against the same walk without it | the walker parted in both recorded steps | "why was not traced" | divergence | the first page event that differs | 1 h |
| A33 | surrealdb, M277 | 11 power cuts at a disk flush inside the 4.0.1 open | which cuts hit the conversion, and which the data migrations after it? | "not separated" | stacks | the server's stack at each cut's stop | 1 h |
| A34 | restate, M303 | seed 15 of exploration A | a failure with a truncated file and a backward step | "not separated" | divergence | counterfactual runs without each fault | 30 min |
| A35 | yugabyte, M193 | seeds 13, 15, and 18 | a service refused to start. The changed file, or the clock step? | "not separated" | logs, stacks | the start's log; the stack where the start ended | 30 min |
| A36 | yugabyte, M193 | the version pair of TA-30772 | the known bug's trigger did not fire | "not established"; the record names the next step: read the bootstrap with `hydo inspect` | watch, logs | the large transaction's apply during the bootstrap | 1 h |
| A37 | blnk, M224 | finding 4, `T=` an hour back after a kill | transfers waited 11 minutes on a lock | "the lock's expiry was not read in Redis" | logs | `PTTL` of the lock key in a held run | minutes |
| A38 | redis, M313 | `hang=30+1000` | after a failed flush Redis's restart stopped at `FATAL AOF MANIFEST FILE ERROR` | the state of the guest's file system after the failed flush "was not traced" | watch | the manifest file's writes and blocks around the failed flush | 1 h |
| A39 | typesense, M366 | a `G=` that zeroed `raft_meta` | the node stayed up and unhealthy | "its log was not read" (open item 188) | logs | the node's log | minutes |
| A40 | compose-voting, M128 | 2 of 50 seeds, a database pause with a clock jump | on agent images only, the checks get no answer from PostgreSQL after the heal | "not explained" | stacks | PostgreSQL's backends' stacks after the heal | 1 h |
| A41 | stack (bug benchmark), M129 | `P=kafka@14+16`, `9f3e843a38fc41b7` | on agent images only, an acknowledged order is missing from Kafka | "not explained" | path, divergence | the order's message from the order service to Kafka | 1 h |
| A42 | nsq, M362 | e1's power-cut failures other than seed 1 | 6 failures; only seed 1's shrink was traced to the missing metadata file | "not traced" (open item 184) | watch | the metadata file's writes and renames | 1 h |
| A43 | tikv, M287 | the example's first form | TiKV's gRPC client did not reach PD by service name | "not traced" (open item 147) | path | the name lookup and the connect attempts | 1 h |
| A44 | lago, M236 | the x86_64 image on hvx | the `api` service exits at start with a `require` error in `bin/rails` | not traced (open item 110) | none | a record of the files that a process opens | 1 h |

## Gaps of hydo's own engines

These cases are runs that should have repeated exactly and did not, or a hang of hydo's control.

| id | where, milestone | the run | symptom and open question | tried, time | tools | data needed |
|---|---|---|---|---|---|---|
| B1 | hvx under lock-step control, M127, M136, M138 | `stack-grpc` planted seed 6, `disk=cut dseed=581cc68c71772e2c \| run K=front@27+2 C=30 P=front@2+4 T=17:-30 H=orders@8-11:status=UNAVAILABLE,rpc=Place H=orders.place@3-30:drop,p=50,seed=194` | over the serial line the guest stops making stops, with the agent's last frame cut. The cause inside the serial path "was not looked for" | runs of 900 s and 3,673 s; avoided by the host-call port | stacks | the guest kernel's stacks of the agent's write and the 8250 driver |
| B2 | hvx, M118 | `explore-ob-redis-nq`, ends at one icount with cadences 16 to 1 | four RAM hashes; 32 bytes of `irq_randomness` differ. The cause in `hvx.c` "was not looked for" | a RAM diff found the bytes | divergence | the first interrupt that arrives at another instruction |
| B3 | KVM, M200 | Ghost, `H=mailgun.messages@0-12:status=500,effect=yes` | two fingerprints with the same transcript, consoles, and verdict | "not investigated" (open item 69) | divergence | the first VM state that differs |
| B4 | KVM, M304 | Dragonfly's first runs | the Xeon Platinum 8573C's fingerprints differ from the Xeon 6975P-C's, 0 of 6 seeds equal | "the cause was not looked for" (open item 153) | divergence | the first VM state that differs between the hosts |
| B5 | hvx, M245, M292 | `blocks/` explorations; a `hydo fuzz --vm` recovery boot | `HVX_HC_PUT: No space left on device` 46 times per worker, none under `hydo run`; a recovery boot loops | "not investigated" (open item 120) | path | each agent record from the guest to the host port |
| B6 | KVM test, M208 | `the_rewind_mode_changes_nothing_without_a_counter_loop` | two fingerprints once, then 50 passes | "not explained" (open item 77) | divergence | both runs' events; not reproduced since |
| B7 | KVM, M173, M175 | libSQL, two simultaneous fresh runs | two fingerprints once | "stays unexplained"; not reproduced | divergence | both runs' events |
| B8 | hvx, M126 | the Redis lying-disk seeds 4, 8, and 19 | the agent image's guest makes other disk requests, so the lying disk hits others | "not investigated further" | divergence | both runs' disk requests in order |
| B9 | hvx, M34 | Kafka with `tsc=reliable` and the exit clock | the branch's durable outcome differs from the straight run | "not investigated"; the images are gone | divergence | the first event that differs |

## Causes found late, or first described wrongly

| id | example, milestone | how the cause was found | time, as recorded | tools that would have answered | estimated saving |
|---|---|---|---|---|---|
| C1 | rabbitmq finding 1, M370 | the first record said three false things. A replay with hydo 0.7.0 kept every node's log, and the coordinator's and osiris's code gave the order | the onboarding spent 1 h on timing-sensitive candidates; the check came in a later session | path, watch | the wrong first record; a report's text |
| C2 | rabbitmq finding 3, M370 | the first record said two false things and named an unrelated pull request. Held runs read each node's clocks and the coordinator's state at four points; code of RabbitMQ, Ra, and the Erlang runtime | a later session; not timed | watch | the wrong first record |
| C3 | rqlite finding 3, M369 | two nodes' logs and the code of hashicorp/raft and of `rqlited`'s bootstrap | 2 h 30 min for the whole example's judging | path, stacks | most of the code reading |
| C4 | typesense finding 1, M366 | an image built with one batch per delete passed; then the code of the deferred batches | 4 h 30 min for exploring and judging | watch | the second image and the A/B runs |
| C5 | typesense finding 3, M366 | the logs of a replay with hydo 0.6.0, then the code of braft's and brpc's timers | in the 4 h 30 min | stacks, watch | the code search for the timer |
| C6 | garnet finding 1, M371 | the replica's status, the option's code, and a native run | in about 6 h of directed runs | watch | part of the directed runs |
| C7 | garnet finding 2, M371 | separation by directed runs, then the logs and the code, then a native run | most of about 6 h of directed runs | watch, divergence | several hours |
| C8 | clickhouse Keeper loop, M311, M319, M325 | first read as hydo's own TIMEOUT. M319 found two busy threads; M325 held runs at three stops and took 100 kernel-stack samples, then read the code | three milestones; runs of 419 s | stacks | two milestones |
| C9 | typesense on KVM, M373, M376 | every node exited with SIGSEGV. `ls /proc/self` in the guest, a preloaded library on a runner, then gdb's backtrace `bvar::read_proc_io` | a milestone of runner rounds | stacks | the runner rounds |
| C10 | blnk finding 1, M224, M285 | the value history of M285 named the limit lost in the worker's copy | 4 h for the onboarding; M285 later | watch | the existing precedent of `watch` |
| C11 | stack order service, M56, M58 | 12 of 282 cut points failed in M56, and the record said "not yet re-run or analysed. M58 traced InnoDB's commit before its redo sync and the client's retry | one milestone | path | most of a milestone |
| C12 | hvx harness, M56, M58 | 2 of 205 failing points gave other fingerprints. M58 traced a failed 64 KiB allocation, with new tools `statediff.py` and the return log | one milestone | divergence | the precedent of `divergence` |
| C13 | hvx, M65 | a rare fingerprint gap. `retsplit.py` found the first differing return: +905,573 branches from SMIs | gaps since M34; one milestone | divergence | the precedent of `divergence` |
| C14 | the worker pool, M206, M207 | four schedules differed between the pool and a fresh boot, "not looked for" in M206. M207 found guests booted without the CPUID cache leaves | one milestone | divergence | most of a milestone |
| C15 | KVM on GitHub's runners, M254, M265 | 13 of 85 boots ended `SPIN`, first guessed a host delay. M265 built `HYDO_KVM_GAPS` and found the engine's own observer | one milestone | none: a host-time account per stretch, now built | |
| C16 | KVM reboots, M263, M266 | AMD models seemed to differ; three workflow rounds found two builds of the image, with the build time in the bundles | three runner rounds | divergence | two rounds |
| C17 | the agent's lines on hvx, M259, M261 | `the agent refused a line ... bad-json text after the value at byte 6`; M261 found staged bytes overwritten during the agent's read | one milestone | path | most of a milestone |
| C18 | medusa, M174 | four `ERROR service start` runs, each found by rebuilding with a `tee` wrapper | 9 minutes and 7 rebuilds each | logs (built in M178) | the reason for `hydo inspect` |
| C19 | nsq finding 1, M362 | the aimed grid shrank the in-flight loss to `+0`, where finding 1 hides it, and the report named the wrong cause. Directed delays on both sides of the 2 s sync told them apart | in the 5 h 50 min of the example | watch, divergence | the wrong first report |

## Cases outside the guest

These records stay out of the counts. No tool that looks inside a guest reaches them:

- a guest image whose build differed once (M34), and Mac values whose staged inputs were lost
  (M155): a build needs a record of its inputs;
- both engines of a pool that died at once (M92), fuzzing VMs out of memory (M97), and a worker
  that held 1,058 MB (M335). They need an account of the host's memory and processes;
- a test's child process that waited 15 minutes (M309), and two tests that failed once (open
  item 78, M155). They need a debugger on the host process;
- `hydo rules check` without a result for one rule (open item 122), which needs the same;
- Formance Ledger's Go link that hung twice on the bare-metal machine (open item 110).

## The data each tool needs

**`stacks`.** It needs the user stack and the kernel stack of each thread at a stop, named by the
program's debug information. It also needs the futexes and kernel locks each thread holds or waits
on. 12 of its 15 cases run native code: Go, Rust, C, and C++. Garnet needs .NET's frames (A1, A4), and
B1 needs the guest kernel's. A stop anywhere in the run needs a replay to it, which `hydo inspect`
already makes.

**`watch`.** An address, a key, or a file range; every write to it, with the stop, the process,
the thread, and its stack. Three cases watch a file's blocks on the disk (A18, A21, A38), which the
engine's disk model sees. A key in a store needs its address, from a function that the user names,
as the value history of Milestone 285 records values. The Mac engine has hardware breakpoints for
that history; data watchpoints need debug registers or write-protected pages on every engine.

**`divergence`.** Two runs' events in order: the control transcripts, the agent's events,
packets on the bridge, system calls, interrupts, and VM exits. The tool aligns them and names the
first event that differs, with its stop. A counterfactual branch restores the snapshot before an
event, changes it, and runs on. The records already hold the passing neighbour of A2, A3, A5, and
A8. B6 and B7 did not repeat, so the tool needs to record every run's events, not only the runs
a user asks for.

**`path`.** It needs each message's send, with its process, system call, and stop. It needs each
packet on the bridge or through a fault proxy, the receive, and the write that applies the
message. The write comes from `watch`, and the packets from the event stream of `divergence`.

## The counts

The first tool of a row is its primary tool. A row can name two or three tools.

| tool | primary | named at all | primary in A | primary in B | primary in C |
|---|---|---|---|---|---|
| `divergence` | 21 | 26 | 10 | 7 | 4 |
| `watch` | 15 | 27 | 9 | 0 | 6 |
| `stacks` | 12 | 15 | 8 | 1 | 3 |
| `path` | 12 | 20 | 7 | 1 | 4 |
| `logs`, the existing tools | 9 | 10 | 8 | 0 | 1 |
| `none` | 3 | 3 | 2 | 0 | 1 |

The primary columns count 72 cases. Three readings follow from them:

- `divergence` leads because of hydo's own engines: 7 of the 9 gaps of B need it. Among the
  system failures it ties with `watch`, and most of its system cases are separations by hand that
  a counterfactual branch would make.
- `watch` is named most often, 27 times, and it answers 6 of the 19 cases that were traced late,
  more than any other tool. Two of them were first described wrongly (C2, C19).
- 9 cases needed no new tool. A log or one command in a held run would have answered them. These
  are the cheapest to close: a rule that a cause is not called unknown before the failing run's
  logs are read.

## The first proofs of each tool

A good first proof has a recorded schedule, a short run, and a sharp question. A case with a
known answer comes first: the tool must find what a person found by hand. Then a case whose
answer is open.

| tool | known answer first | then open cases |
|---|---|---|
| `stacks` | C8, the Keeper loop: seed 20 of E2, a run of 419 s on hvx; the answer is `KeeperTCPHandler`'s `poll()`. C9, Typesense on KVM: the answer is `bvar::read_proc_io` | A1, Garnet's replica that hangs (saved regression, 50 to 60 s on hvx); A33, SurrealDB's cuts inside the upgrade; and A19, the two ClickHouse seeds that timed out |
| `watch` | C2, RabbitMQ finding 3 (saved regression): the parked retry a day away. C4, Typesense finding 1 (saved regression): the deferred batch that deletes `i4x39`. C10, Blnk's overdraft limit | A26, etcd 14685 (each member's revision); A3, Garnet's replica at offset 2,460 (saved regression) |
| `divergence` | C14, the pool against a fresh boot: the answer is the CPUID cache leaves | A5, RabbitMQ finding 1 with and without `K=rabbit2@1+3`; A8, rqlite seed 28 with and without `O=`; A2, Garnet with and without the pause; A22, Mender's cut at +64 against +65 ms |
| `path` | C3, rqlite finding 3 (saved regression): the leader's AppendEntries to the removed node. C1, RabbitMQ finding 1 (saved regression): the chunk of `s29` | A13, Hatchet's cancellation message; A3, Garnet's replication records; A11, Redpanda's two AddPartitionsToTxn requests |

Every proof but A19, A26 and A33 has a saved schedule with its fingerprint in the example's
`RESULTS.md` or `hydo/regressions/`. Garnet's runs take 200 to 350 s on KVM, so its proofs run on
hvx.

## The proposed order

1. **Finish `stacks`.** It is being built, and the other three tools need its symbols to name
   the code's place. Prove it on C8 and C9, then on A1 and A33.
2. **Build `divergence` in two steps.** The first step compares the events hydo records today:
   transcripts, agent events, packets on the bridge, and service logs. It also adds the
   counterfactual branch that leaves a fault out or moves it. That step serves the separations of
   A2, A5, A6, A8, A14, A22, and A34. The second step adds system calls, interrupts and VM exits,
   for the gaps of B. Prove it on C14, then A5 and A8.
3. **Build `watch`.** It serves the most cases, but it needs engine work on each engine: debug
   registers or write-protected pages, and the disk model for file blocks. The Mac engine's value
   history is the first form. Prove it on C2 and C4, then A26.
4. **Build `path` on top of the two.** Its packets come from `divergence`, and its apply from
   `watch`. Prove it on C3 and C1, then A13.

Before any of them, read the logs. Nine cases (A7, A10, A12, A15, A28, A35, A37, A39, C18) needed
only a log or one command in a held run. A failing run of an exploration keeps no log today, so
A12 and A15 would need a replay first.

## What the benchmark cannot say

- The time each tool would save is this page's estimate, from the record's own times where it has
  them. Most records of open cases do not say how long was spent before the case was left.
- A case's tools are this page's reading of its question. A tool that answers the question may
  still leave the label open, as for Garnet's three candidates.
- Old cases (A40, A41, B8, B9) ran on images built before the switch to agent images or before
  Milestone 140, and some of those images are gone.
- B6 and B7 did not repeat. A tool that compares two runs needs both runs recorded at the time.

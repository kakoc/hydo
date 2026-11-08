# What Jepsen has that hydo lacks (design)

Status: proposal, nothing built. It compares hydo, as AGENTS.md "Current state" describes it on
2026-11-08, with Jepsen's current sources, and proposes what to build and in which order. The
messaging checks it ranks first are designed in [messaging.md](messaging.md).

Sources read (the `main` branches on 2026-11-08):

- Jepsen, https://github.com/jepsen-io/jepsen: `jepsen/src/jepsen/nemesis.clj`,
  `nemesis/combined.clj`, `nemesis/file.clj`, `nemesis/time.clj`, `nemesis/membership.clj` and
  `membership/state.clj`, `lazyfs.clj`, `net.clj`, `db.clj` (the `Primary` protocol), `role.clj`,
  `independent.clj`, `checker.clj`, `checker/perf.clj`, `checker/timeline.clj`,
  `checker/clock.clj`, and `tests/` (`bank.clj`, `long_fork.clj`, `causal.clj`, `adya.clj`,
  `linearizable_register.clj`, `cycle/`, `kafka.clj`). The generator combinators now live in the
  separate library `io.jepsen/generator`; this note names the ones Jepsen's own sources use.
- Elle, https://github.com/jepsen-io/elle: `src/elle/list_append.clj`, `rw_register.clj`,
  `consistency_model.clj`, `txn.clj`.
- LazyFS, https://github.com/dsrhaslab/lazyfs (`README.md`), which Jepsen's `lazyfs.clj` mounts.

## How the two tools differ before any feature

Jepsen runs a test against real machines or containers, with wall-clock time, threads and a
random generator. Faults happen while client operations run, repeatedly, for minutes. A run does
not reproduce: Jepsen keeps the history and checks it, and a failure is studied from its
history.

hydo runs the whole system in one deterministic guest. A run is a pure function of the image and
a schedule string, so every failure reproduces bit for bit, shrinks to a minimal schedule, and
can be inspected at any stop. The price is that every fault must be a token of that string, and
that every new fault must leave all recorded fingerprints alone when it is not used.

So a Jepsen feature does not port as code. Each row below says how it maps onto hydo's model:

- **a schedule token** (`hydo-schedule`, applied by the host controller `hydo-control`), or an
  extension of an existing token's syntax. New letters are costly: every free upper-case letter
  can be a manifest knob today (`RESERVED` in `hydo-schedule/src/guest.rs` is `KPNOCTQXBFHWAGL`),
  so reserving one refuses a manifest that uses it as a knob. Extending the target syntax of an
  existing token breaks nothing.
- **the explorer** (`hydo-explore`): new draws come from their own random stream behind an
  option, as `--storage-faults` does (`STORAGE_SALT`), so every existing seed keeps its schedule.
- **a host-side checker** (`hydo-history`, `hydo-sql`): checkers run on the host after the run,
  so they change no fingerprint.
- **the results format** (`hydo-results`): additions only, checked by `hydo-schema-compat`.

## The comparison

Columns: what Jepsen has; what hydo has (with where); the gap; the value for users; a rough cost
(S: days, M: one to two weeks, L: more); how it maps onto hydo.

### Faults

| Jepsen | hydo today | gap | value | cost | mapping onto hydo |
|---|---|---|---|---|---|
| Kill, pause, start, resume of a **node set**: `nil` (random nonempty subset), `:one`, `:minority`, `:majority`, `:minority-third`, `:primaries`, `:all`, or named nodes (`combined.clj` `db-nodes`) | `K=`, `P=`, `X=` name one service each, and a schedule may hold at most one of each (`repeated_fault` in `hydo-control/src/schedule.rs`; MANIFEST.md "Fault tokens"). Several services at once only through reactions (`A=<event>:kill-<svc>`, one per service), which need an assertion event. The explorer draws one kill per schedule (`gen.rs` `generate_process`: `s.k` is one value) | no way to say "kill a majority" or "kill every node" in one token; no draw of node sets | high: crash-all and majority loss find durability and recovery bugs (Jepsen's NATS 3.1, hydo's M141 needed hand-written reactions) | S for named sets, M with the explorer | extend the target before `@`: `K=n1,n2,n3@5+2` (a comma list; service names are `[a-z0-9_]`, so a comma is free). The explorer draws a set shape from its own stream behind `--node-sets` |
| Faults aimed at a **role**: `:primaries` from the database's `Primary` protocol (`db.clj`), `jepsen.role` for nodes with distinct roles | none: no manifest key names a group of services or a role | the leader cannot be targeted, and a schedule cannot say "the current primary" | high for replicated systems: leader faults find most election and failover bugs | M | manifest addition `groups:` (named lists of services) and a per-group probe command that prints the current primary; selectors such as `K=nodes.primary@5+2`, `K=nodes.majority@5+2`, resolved by the host controller at the fault's stop by running the probe (deterministic under hydo) and logged with the members it chose; random picks (`majority`) take an index (`nodes.majority.2`), so the choice is a pure function of the token |
| **Repeated faults** over a run: every package is a `flip-flop` of start and stop, `stagger`ed at an interval (default 10 s); `package-gen` rotates mixes of packages with quiet periods (`combined.clj` `PackageGen`) | one window per fault kind per schedule; the `process` strategy places two to four different kinds in successive windows; a second `N=`, `O=`, `T=` or `C=` silently replaces the first (they are not in `repeated_fault`; `run_cfg` keeps the last) | no kill-restart-kill cycles, no fault during recovery from the previous one, no mixes that change over the run; a silent replacement of a second network or clock token | high: recovery-during-recovery bugs; medium for the silent replacement (a correctness issue of hydo itself) | S to refuse repeats of N, O, T, C; M to allow lists | first refuse a repeated `N=`, `O=`, `T=` and `C=` as `K=` is refused (no recorded schedule has one; check with the corpora). Then allow several windows in one token, `K=n1@3+2/7+2`, and a `--cycles` explorer option that draws flip-flops. The guest part is limited to 511 bytes (`GUEST_MAX_BYTES`), which bounds the number of windows |
| **Partitions** by shape (`grudge` in `combined.clj`, `nemesis.clj`): `:one`, `:majority` (clean halves), `:minority-third`, `:majorities-ring` (every node sees a majority, no two the same one), `:primaries` (each primary alone), `bridge` (two halves joined by one node), `complete-grudge` from components | link faults between two services (`N=a-b`), between one service and every other (`N=a-*`, an isolation), one direction (`O=`), on loopback by address or on veths with `network: bridge` (MANIFEST.md "Link faults", "Service networks"); one `N=` per schedule | a split into halves, a ring or a bridge needs several links at once, which one `N=` token cannot express | high for consensus systems (split brain, stale leaders, non-transitive connectivity) | M | extend `N=`'s target: components separated by `/` (`N=n1,n2/n3,n4,n5@3-9:loss,100%`: every link across components, both ways) and named shapes over a group (`N=nodes.halves.1`, `nodes.ring.0`, `nodes.bridge.0`, `nodes.primary`), resolved deterministically by the host and applied with the existing u32 or veth filters, one filter per cut link. Lifting reuses the M71 drain |
| **Packet behaviours** on a node set (`packet-nemesis`): delay, loss, corrupt, duplicate, reorder, rate, with correlation (`net.clj` `all-packet-behaviors`) | the same netem kinds on a port or a link (`Netem` in `guest.rs`: delay, loss, duplicate, reorder, corrupt); no rate limit | node sets (above) and a bandwidth limit | medium | S | the node-set target applies here too; `rate,<kbit>` as a new `Netem` kind |
| **Clock**: `bump` (a step of -288 s to +288 s, exponentially distributed, on a node subset), `strobe` (back and forth by delta every period for a duration), `reset`, `check-offsets`, `clock-scrambler` (`nemesis/time.clj`, `nemesis.clj`) | `T=<at>:<seconds>`, a step of the whole guest's `CLOCK_REALTIME` in whole seconds (`features/clock.rs`, through the agent's `clock_set`); `guest.clock_after_cut` | per-service clocks (all services share one kernel), sub-second steps, strobe | medium: lease, lock and TTL bugs depend on clock differences between nodes; the whole-guest step finds only bugs that need a jump everywhere | S for sub-second steps and a whole-guest strobe; L for per-service clocks | see "Per-service clocks" below |
| **File corruption** (`nemesis/file.clj`, `combined.clj`): `bitflip` with a probability, `truncate`, `copy-file-chunks` (copy chunks within a file, which makes valid-looking misdirected data), `snapshot-file-chunks` and `restore-file-chunks` (roll chunks back to older contents), and `helix-gen` (every node corrupts a different, stable region, so the cluster keeps one intact copy of each) | `G=` flips, zeroes or truncates one file of one service, while it runs or while it is down (MANIFEST.md "Storage faults of one service") | chunk copies, rollbacks of chunks or of a whole data directory, and the same corruption on several nodes at different regions | high for systems that claim to repair from replicas (TigerBeetle, NATS JetStream 3.2 and 3.3) | M | new `G=` kinds `copy` and `restore,from=<step>` (the host copies the bytes at step `from` and writes them back at `at`); a node set target (`G=n1,n2,n3@...:flip,helix`) where `helix` gives node *i* the chunks *i mod n*. A whole-directory rollback is a separate kind: `G=n1@8+2:rollback,from=3` copies the directory at step 3 and puts the copy back while the service is down (a node restored from an old backup) |
| **Lose un-fsynced writes** of one node (LazyFS `lose-unfsynced-writes` through `lazyfs.clj`); LazyFS also crashes at the k-th call of an operation on a matching file (`crash::timing=before::op=fsync::from_rgx=...`), keeps only some writes of a sequence (`torn-seq`), and persists only some parts of one write (`torn-op`) | `L=` drops what one service had not written back to its own file system, but keeps what the kernel handed to the disk and the disk did not flush (open item 61 (a)); whole-guest `C=`, `cf=<k>` (a power cut at the k-th disk FLUSH) and the lying disk | the disk-cache half of `L=`; a cut aimed at the k-th fsync of a matching file of one service; torn sequences of one service | high for storage engines and logs (an fsync placed one call too late) | M for the disk-cache half (engine work, open item 61 (a)); M for an fsync-aimed cut | for the aimed cut: the agent cannot see a service's fsyncs, so this needs either the engines (count FLUSH requests from the service's partition, as `cf=` counts them for the disk) or an LD_PRELOAD shim. The engine route keeps hydo's rule that a fault is a pure function of the schedule: `L=n1@cf3` would cut n1's partition at its third FLUSH |
| **Membership changes** (`nemesis/membership.clj`, marked experimental): a state machine of the cluster view per node, with join and remove operations that the database supplies, pending operations remembered and resolved | none | adding and removing members while the system runs | medium: valuable for databases and brokers with reconfiguration, but each system needs its own commands | L | manifest addition `membership:` with `join`, `leave` and `view` commands per service. A reconfiguration is a command, not a signal, so it fits the composer (a command kind run at a round) or reactions better than a fault token; the host controller would keep the cluster view as Jepsen's state machine does. Defer until a user asks |
| Process **hammer** (`hammer-time`: SIGSTOP a named process on a node) | `P=` on a service's whole process tree, by steps or timed in microseconds | none | | | |

### Generators and the shape of a test

| Jepsen | hydo today | gap | value | cost | mapping onto hydo |
|---|---|---|---|---|---|
| Combinators over time: `phases`, `mix`, `stagger`, `delay`, `time-limit`, `flip-flop`, `reserve` (thread pools per workload), `each-thread`, `until-ok`; client and nemesis generators run at once; a final generator heals and reads back | the test composer (`W=`: commands by kind, rounds, `parallel`, `per_round`, `round_ms`; `first`, `anytime`, `eventually`, `finally`), numbered steps, faults at step boundaries or timed (`P=...~`, `X=`), reactions (`A=`), the heal, `retry`, `final:`, `eventually` checks | repeated fault cycles (above); per-thread pools exist as composer kinds; no time limit per phase in guest time | low beyond the fault cycles | | the composer already covers most of it; the gap is the fault side |
| `jepsen.independent`: lift a single-key test to many keys run concurrently, so expensive checks stay short per key | the linearizability checker is per key (P-compositional); the list-append checker is multi-key | none for checking; the ready-made SQL workloads choose keys from the seed | | | |
| Process reincarnation: a process whose operation ends `:info` is retired and a new process id takes its place | the record format requires one operation in flight per process; an open invocation counts as unknown (HISTORIES.md) | none | | | |

### Checkers and workloads

| Jepsen | hydo today | gap | value | cost | mapping onto hydo |
|---|---|---|---|---|---|
| `linearizable` (Knossos models, WGL search) | `linearizable` with models `register` and `cas`, per key, store versions, sound small witnesses (HISTORIES.md) | the queue, set and mutex models of Knossos | medium (queues: see messaging.md) | S per model | `hydo-history/src/register.rs` takes a model; a queue model is part of messaging.md |
| Elle `list-append` | `list-append` with G0, G1a, G1b, G1c, G-single, G2, process and realtime variants, lost appends, stale reads, incompatible orders (HISTORIES.md) | **G-nonadjacent** (snapshot isolation is not checked against cycles with two or more non-adjacent rw edges; open item 20); predicates and deletes | high: snapshot isolation is the level most SQL systems claim | S | add the search for cycles whose rw edges are never adjacent to `listappend.rs`; Elle's `consistency_model.clj` lists the implication graph to follow |
| Elle `rw-register` (reads and writes of registers, versions inferred from writes read and from order inside transactions) | none; registers are checked only for linearizability | transactional checks of systems that have no append (most key-value and SQL stores used with plain reads and writes) | high | M | `hydo-history/src/rwregister.rs`, a port of `rw_register.clj`; the ready-made SQL `register` workload can record transactions of several registers for it |
| Elle's **model boundary**: from the anomalies found, the weakest models ruled out (`:not`) and the stronger ones also ruled out (`:also-not`), and the models still possible (`possible-models`) (`consistency_model.clj` `anomalies->impossible-models`, `boundary`) | the checker reports whether the claimed `consistency:` holds | a user learns only pass or fail of one level, not which level the system did keep | medium: it makes comparisons between systems readable | S | a `boundary` field in the check's details (results addition) |
| `bank` (`tests/bank.clj`): total conserved, no negative balance, with plots | `hydo-sql bank` (M182): the same properties, live and final | plots | | | |
| `long-fork` (`tests/long_fork.clj`), `causal`, `causal-reverse`, `adya` (G2 tests) | none of these as workloads; G2 is found by list-append | long fork is the anomaly that separates parallel snapshot isolation from snapshot isolation | low to medium | S each | workloads of `hydo-sql`; the checkers are small |
| `set` and `set-full` (`checker.clj`): per element a timeline of present and absent reads, stable and lost elements, stale reads, quantiles of the time until an element is stable or lost | `hydo-sql set check` and `hydo-check subset` at the end of a run | per-element timelines during the run, stale-read counts and latencies | medium | S | a `set` history kind in `hydo-history` with the same records as list-append reads; the quantities go into messaging.md's measures |
| `counter` (`checker.clj`): every read is between the sum of acknowledged and the sum of attempted increments | `hydo-sql counter check`: only the lower bound, at the end (open item 60 (e)) | the upper bound, and reads during the run | medium | S | record increments and reads as a history; the attempted sum comes from the invocations, so an increment killed in flight still counts in the upper bound |
| `queue` (every dequeue comes from an enqueue, O(n)), `total-queue` (what goes in must come out, with lost, unexpected, duplicated and recovered counts), `unique-ids` | `hydo-check exactly-once`, `subset`, `no-duplicates` over store readings | checks over the history of sends and receives, with unknown outcomes; leases, order, groups | high (the user's priority) | M | messaging.md |
| `tests/kafka.clj`: sends and polls with offsets; aborted reads, inconsistent offsets, lost writes up to the highest observed offset, duplicates, poll skips and non-monotonic polls (internal and external), precommitted reads, write cycles through Elle, unseen messages over time, realtime lag | `hydo-read kafka` and `hydo-check exactly-once` at the end | everything during the run | high | M | messaging.md, the `log` order claim and the offsets |
| `log-file-pattern`, `unhandled-exceptions` | error logs as findings (`errors:`, `hydo errors`), stronger: signatures, a reviewed known list, shrinking | none | | | |
| `stats` (ok, fail, info counts per operation) | partial: the composer's per-command outcomes | counts per operation kind in the results | low | S | a results addition |

### Seeing a run

| Jepsen | hydo today | gap | value | cost | mapping onto hydo |
|---|---|---|---|---|---|
| `perf`: latency and rate graphs per operation, with the nemesis's activity drawn as bands (`checker/perf.clj` `nemesis-regions`) | `report.md`, `exploration.json`, `hydo diff`, `hydo explain`, `hydo inspect events`; no graphs | no picture of how latency and errors move while a fault is active | medium: it is how people read a fault test | M | `hydo report --html <run dir>`: one static page from `result.json`, the control transcript (fault start and heal stops with guest times) and the histories; latency and rate per operation in guest time with fault bands. Guest times are deterministic, so the page reproduces with the run |
| `timeline.html`: every operation as a bar per process | none | the same page, a second panel | low | S | as above |
| `clock-plot` (clock offsets over time) | the clock step is logged | a plot once per-service clocks exist | low | S | as above |
| Elle's cycle pictures (graphviz of each anomaly) | text explanations of every edge | pictures | low | S | optional, in the same page |

## Per-service clocks

All services of a hydo guest share one kernel and so one `CLOCK_REALTIME`. Jepsen bumps and
strobes the clock of each node separately, which is what exposes a leader that trusts its own
lease while a follower's clock is ahead.

**Time namespaces.** Linux has time namespaces since 5.6 (so the 6.6 guest kernels have them,
though hydo's configurations do not enable `CONFIG_TIME_NS`: `hv/kvm/linux/hydo-kvm.config` has
`CONFIG_NET_NS=y` and no `CONFIG_TIME_NS`). A time namespace offsets only `CLOCK_MONOTONIC` and
`CLOCK_BOOTTIME` (and their raw, coarse and alarm variants), not `CLOCK_REALTIME`, and its
offsets can be written to `/proc/<pid>/timens_offsets` only before the first process enters it.
So the stock kernel cannot give a service its own wall clock, and cannot change an offset while
the service runs.

**A kernel patch.** hydo builds its own guest kernels, so a patch is possible. It needs:

1. a `CLOCK_REALTIME` (and `_COARSE`, `CLOCK_TAI`) offset in `struct timens_offsets`, set in the
   namespace's vvar page by `timens_setup_vdso_data` (the page already carries an offset per
   clock base, so the vDSO's `do_hres_timens` path applies it without a vDSO change);
2. the system-call paths: `clock_gettime`, `gettimeofday` and `time` for the realtime clocks;
3. absolute timers on `CLOCK_REALTIME` translated back to the host's clock, as
   `timens_ktime_to_host` does for the monotonic clocks: `clock_nanosleep` with `TIMER_ABSTIME`,
   `timerfd`, POSIX timers, and futexes with `FUTEX_CLOCK_REALTIME`;
4. writable offsets for a namespace that has processes, so that a step or a strobe can happen
   during the run: rewrite the vvar page under its sequence counter and run the clock-was-set
   notification for the namespace's realtime timers.

Kernel-made times stay on the host's clock: file modification times, socket timestamps, the
kernel log. A program that compares a file's mtime with `time()` would see the offset; the guide
would say so. Item 4 is the part upstream rejected for general use; for a test kernel the risk
is only in hydo's own runs.

Cost: L. The patch touches the timekeeping core of four guest kernels (the hvx kernel, the KVM
kernel and the two arm64 kernels). To keep every recorded value, it ships as an opt-in kernel
variant used only by images whose manifest asks for per-service clocks (as M99 made a second
arm64 kernel for `network: bridge`), and the image record names the kernel by sha256, so older
images keep booting the kernels they were built for.

**The schedule.** `T=` gains a target and kinds without a new letter: `T=n2@5:+1.5` (a step of
n2's clock by 1.5 s), `T=n2@5:strobe,200,10,3000` (back and forth by 200 ms every 10 ms for
3,000 ms of guest time). The host controller writes the offset through the agent (a new agent
command, or `exec` of a small writer). A service gets its own time namespace when it starts, as
it gets its own network namespace with `network: bridge`.

**Cheaper first steps.** Sub-second whole-guest steps (`T=5:-0.250`) and a whole-guest strobe need
no kernel work (the agent's `clock_set` gains nanoseconds; a strobe is a timer in the agent or a
series of controller commands at stops). LD_PRELOAD of libfaketime per service (hydo already
ships it for image builds) reaches dynamically linked programs that read the clock through libc,
but not Go programs or other programs that call the vDSO directly, so it would be a partial
answer and is not proposed.

## The starting hypotheses, checked

1. *Faults aimed at several nodes or a role; the explorer draws one kill per schedule.*
   Confirmed. The schedule type keeps one kill (`RunCfg.k` in `hydo-control/src/schedule.rs`; a
   second `K=` is refused since M141), and no manifest key names a role. Corrected in one point:
   several services can be killed at once today, but only through reactions, which need an
   assertion event and are written by hand.
2. *Partition shapes from link faults.* Confirmed, and one more problem found: a second `N=` is
   not refused but silently replaces the first, so a hand-written partition of several links
   runs as one link.
3. *Elle-complete transaction checking.* Partly. hydo's list-append checker already finds G0,
   G1a, G1b, G1c, G-single and G2 with realtime and process variants; missing are G-nonadjacent,
   the rw-register checker and the model boundary.
4. *Queue and stream checkers.* Confirmed: hydo checks queues only by comparing store readings
   at the end (`hydo-check exactly-once`, `subset`, `no-duplicates`).
5. *Lose unsynced writes of one service; roll a node back.* Partly. `L=` already loses what one
   service had not written back (Jepsen's `lose-unfsynced-writes` through LazyFS), except the
   disk-cache half (open item 61 (a)). Rollbacks (Jepsen's `snapshot-file-chunks` and
   `restore-file-chunks`) and chunk copies are missing.
6. *Per-service clocks through Linux time namespaces.* Corrected: stock time namespaces offset only
   the monotonic and boot clocks, and only before the first process enters, so per-service wall
   clocks need a kernel patch ("Per-service clocks" above).

## Ranked plan

The rank weighs the value for users (the user's priority first), the cost, and the risk to
recorded values. Every item is opt-in: a manifest without the new keys and a schedule without
the new syntax run exactly as today.

1. **Messaging properties and measures** (messaging.md). The user's priority, and the largest
   checker gap (Jepsen's queue, total-queue, unique-ids and Kafka workload). Host-side checking,
   additions to the history format, the manifest and the results: no fingerprint moves. M to L.
2. **Refuse a repeated `N=`, `O=`, `T=` or `C=`.** Today the last one wins without a word, which
   is the kind of silent pass M184 removed elsewhere. Check the recorded corpora first. S.
3. **Node sets and roles for `K=`, `P=`, `X=`, `G=`, `L=`** (comma lists, `groups:` with a primary
   probe, selectors resolved by the host) and an explorer option `--node-sets`. It also makes
   schedules portable between systems with different service names, which messaging.md's
   comparison report needs. M.
4. **Partition shapes for `N=`** (components, halves, ring, bridge, primary) and an explorer option
   `--partitions`. M.
5. **Elle completeness**: G-nonadjacent in the list-append checker (open item 20), the model
   boundary in the details, an `rw-register` checker, the counter's upper bound, set timelines.
   Host side only. S to M each.
6. **Repeated fault windows** (`K=n1@3+2/7+2`) and `--cycles`. M.
7. **Storage**: rollback of a service's directory to an earlier copy, chunk copies and helix
   corruption across a node set (host side, no engine change); then the disk-cache half of `L=`
   and an fsync-aimed cut of one service (engines; open item 61 (a)). M each.
8. **A run's page with fault bands** (`hydo report --html`). M.
9. **Clocks**: sub-second steps and a whole-guest strobe (S), then per-service clocks through a
   time-namespace kernel variant (L).
10. **Membership changes.** L, and only when a user's system needs them.

## Open questions

- Should a repeated `N=` (and `O=`, `T=`, `C=`) be refused now (item 2), or first allowed as a
  list? Refusing is the smaller change and stops a silent replacement.
- Are the node-set and role selectors (items 3 and 4) wanted in the schedule text, or should the
  explorer resolve them and print only concrete service names? Selectors keep a schedule
  meaningful across systems and leader changes; concrete names keep schedules short.
- Per-service clocks need a kernel variant (L). Is the lease-and-clock class of bugs important
  enough for the user's systems to pay for it before the cheaper items?

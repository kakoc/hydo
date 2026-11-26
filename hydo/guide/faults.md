# Faults, knobs, and exploring

The full reference is in `hydo guide manifest`, in the sections `Fault tokens`, `Freeze at a
call`, `Clock steps to a time`, `Clock after a power cut`, `Link faults`, `One-shot jobs` and
`Groups of services and partitions`.

## Which faults a project allows

`faults:` in `hydo.yaml` lists the kinds the explorer may draw (default: all except `freeze`):

| kind | what happens | schedule token |
|---|---|---|
| `kill` | SIGKILL a service's whole process tree, restart it later (setup + cmd + ready) | `K=<svc>@<step>+<down>`, timed `X=` |
| `pause` | SIGSTOP / SIGCONT, also in the middle of a request | `P=<svc>@<step>+<n>`, `P=<svc>@<step>~<ms>:<ms>` |
| (none) | the workload's own processes stop for some guest time, as under a CPU limit (section "Pause the workload") | `P=workload@<step>~<ms>:<ms>` |
| (none) | named threads of the workload stop for some guest time, and its other threads run on (section "Pause threads of the workload") | `P=workload/<thread>@<step>~<ms>:<ms>` |
| `network` | netem delay, loss, duplication, and reordering on traffic to a service's `port:`, or on a link between two services with `addr:`. With `network: bridge` the link carries their real traffic, and the explorer draws only links between services that list each other in `depends_on`. | `N=<svc>@<from>-<to>:<netem>`, `N=a-b@...` |
| `oneway` | the same in one direction only | `O=<svc>@<from>-<to>:req\|resp:<netem>` |
| `cut` | power cut: only data that reached the disk survives; a fresh boot recovers and runs the final checks | `C=<step>`, host `cf=<k>` (at the k-th disk flush) |
| `clock` | step the realtime clock (like an NTP step), or set it to the next instant of a time of day or a calendar boundary (section "Clock steps to a time"). One service's own clock can step alone (section "One service's clock ahead of the others") | `T=<step>:<seconds>`, `T=<step>:@<point>`, `T=<svc>@<step>:<change>` |
| `disk` | I/O errors, a lying disk that drops acknowledged writes | host `disk=ioerr:<p>`, `disk=lying:<p>` |
| `freeze` | stop a service at the n-th packet it sends to or receives from a peer (on a Mac: a release with the second guest kernel) | `F=<svc>><peer>@<step>#<n>:<ms>` |
| `protocol` | faults on fakes and proxies (`hydo guide outside-services`) | `H=...` |
| `corrupt` | bit flips, zeroed ranges or truncation of one file in a service's `storage:` directory, while the service runs or while it is down. On the Mac and KVM it also makes bad disk sectors under a range of the file. | `G=<svc>@<step>[+<down>]:flip\|zero\|trunc\|bad\|rot[,n=][,len=][,range=][,file=][,seed=]` |
| `powerloss` | a power loss of one service: what it had not written back to its own file system (`storage: {dir:, size_mib:}`) is lost, the others run on | `L=<svc>@<step>+<down>[:strict]` |

`<step>` counts the workload's steps: a step is one numbered part of `workload.run`, or one round
of commands of the test composer (`workload.steps` sets how many). A fault at step n happens
before step n runs.

`G=` takes a list or a group's selector too, as `K=` does: `G=n1,n3@10+2:flip` corrupts one file
of each service (section "Faults on a cluster"). `L=<svc>@<step>+<down>:strict` is a harsher
power loss. The service loses every write it had not synced, also what Linux had written back.
With `network: bridge` its peers hear nothing from it while it is down.

Leave out kinds that cannot apply (no `network` for a project whose services have no port).
`corrupt` and `powerloss` need `services[].storage` (`hydo guide manifest`, "Storage faults of
one service"): the directory under /data where the service keeps its files, with `size_mib` for
a file system of its own.
A schedule looks like `disk=none dseed=1 | run K=db@5+2 C=12 W=0000002a,p4,q2,r200`: host tokens
before the `|`, guest tokens after `run`.

## Host tokens

The host tokens set up the machine the guest runs on. Each is one `key=value` word before the
`|`; a schedule without one uses its default.

| token | what it does |
|---|---|
| `disk=none` | The default. The data disk has no write cache: a write reaches the disk when the guest writes it. A power cut (`C=`) then loses only what the guest still held in memory, never a write the disk took. |
| `disk=cut` | The data disk keeps a volatile write cache, as real disks do. A power cut (`C=` or `cf=`) loses every write the guest did not flush (`fsync`). Use it to test durability. |
| `disk=ioerr:<p>` | Seeded I/O errors on the data disk; `<p>` is the error rate in the engine's units (larger means more errors). |
| `disk=lying:<p>` | A disk that drops some writes it acknowledged; a power cut then loses them even when they were flushed. |
| `dseed=<hex>` | The seed of the disk's decisions: which requests fail or are dropped (default 0). The same seed gives the same decisions. |
| `cf=<k>` | A power cut at the k-th disk flush of the workload, instead of `C=` (hvx only; needs `disk=cut`). The disk loses power at that flush, and the run is cut at the next stop: the workload and the services are frozen, and the recovery boot follows. |
| `tj=<hex>` | Seeded tick jitter: some timer ticks come late, which changes thread interleavings. On hvx a delay counts guest branches; on the Mac and on KVM it counts VM exits. |
| `tjpm=<n>` | With `tj=`: how many ticks per 1,000 are delayed (default 10). |
| `tjmax=<n>` | With `tj=`: the longest delay (default 500,000 branches on hvx, 1,000 exits on the Mac and KVM). |
| `torn=<p>[:any]` | The Mac and KVM, with `disk=cut`: at a power cut, `<p>` percent of the unflushed writes of several sectors keep only a part of their sectors (section "Faults of the disk's blocks"). |
| `mis=<k>[:<sectors>]` | The Mac and KVM: the k-th write of the workload goes to another place on the disk. |
| `lse=<sector>+<count>[:rot]` | The Mac and KVM: reads of these sectors fail, or with `:rot` return damaged bytes. |
| `hang=<k>[+<ms>]` | The Mac and KVM: the k-th disk request of the workload gets no answer, or an error after `<ms>`. |
| `pct=<hex>`, `pctn=<n>` | The Mac and KVM: `<n>` forced preemption points at seeded VM exits. |
| `spin=rewind` | The Mac and KVM: run the schedule in rewind mode, which ends busy waits that make no VM exits deterministically. hydo switches to it by itself when a run needs it. |

The explorer and `hydo explore --sweep` write the other host tokens themselves (inserted ticks
`pp=` and `xt…` on hvx, vCPU turns `q=` with several vCPUs, the run budget `rb=`).

## Faults of the disk's blocks

A storage engine's own rules are about sectors, and a real disk breaks them in four ways that
the faults of the preceding table do not make. The Mac and KVM engines make each one on the
data disk. hvx refuses them before any VM. Each is decided from `dseed=` and from the requests
counted from the workload's start, so a schedule repeats exactly:

| token | what the disk does |
|---|---|
| `torn=<p>[:any]` | With `disk=cut`, at the power cut: `<p>` percent of the unflushed writes of two or more sectors are torn. A torn write keeps its first sectors, or with `:any` some of them, and the rest keep their old bytes. |
| `mis=<k>[:<sectors>]` | The k-th write goes to another place: `<sectors>` away (`+16`, `-8`), or 1 to 16 blocks of 4 KiB away. The place it was meant for keeps its old bytes. |
| `lse=<sector>+<count>[,...][:rot]` | Reads of these sectors fail with an I/O error, or with `:rot` return damaged bytes. A write of a whole sector repairs it. |
| `hang=<k>[+<ms>]` | The k-th request (a read, a write or a flush) gets no answer, or with `+<ms>` an I/O error after that many milliseconds. |

`G=<svc>@<step>[+<down>]:bad` and `:rot` put a latent sector error under a range of one file of
the service's `storage:`. hydo finds the file's sectors on the ext4 data disk itself and drops
the guest's page cache, so the next read of the range goes to the disk:

```sh
hydo run "disk=none | run G=recs@6:bad,range=114688-118784,file=records"
```

The console says which sectors it chose: `BAD SECTORS under recs's /data/recs/records.dat bytes
114688..118784: lse=45792+8`. Each run with a block fault writes what the disk did to
`<run dir>/one.block-faults.txt` (`write 5 meant for sector 45792 went to sector 45688`). Use that
file to choose `<k>`: the requests are numbered from 1 at the workload's start.

A program that waits on a request that never completes waits without end. When the workload times
out in a run with `hang=<k>`, the run ends with `VIOLATION [disk.hang]`. When the guest waits
120 s of its time on such a request, the run ends with `ERROR workload: the data disk never
answered a request`. A system that sets its own deadline for disk calls and fails the request
passes. The example `hydo/onboard/examples/disk-faults` has a careless and a careful record store
under every fault (its RESULTS.md); `hydo explore --block-faults` draws the faults.

## Pause the workload

A container with a CPU limit stops every thread of the container when it has used its CPU time
for the period. A test that depends on thread timing can fail only then. The timed pause stops
the workload's own process tree, the test program of `workload.kind: test`, in the same way:

```sh
hydo run "disk=none | run P=workload@0~4:45"
```

Before step 0, hydo starts a timer in the guest. After 4 ms of guest time, hydo stops every
process of the workload with SIGSTOP. After 45 ms more, hydo continues them with SIGCONT. Guest
time is the guest kernel's clock, so the numbers mean the same on every engine. The pause ends on
its own: nothing in the workload has to run for it.

- `workload` may stand in a list with services: `P=workload,db@3~20:100` stops both.
- The pause by steps, `P=workload@<step>+<n>`, is refused: the workload does not run between
  steps.
- A schedule holds one `P=`, so a pause of the workload and a pause of a service do not go
  together.
- `faults:` does not need `pause`.

`hydo explore --pause-workload` draws such a pause for half of the schedules, from a stream of
its own:

- The step is one where the workload runs.
- The start is at most `--pause-max` ms after the step's start (default 1000). Half of the
  starts are drawn evenly, the other half on a log scale, so about a quarter fall in the first
  10 ms. Set `--pause-max` near the step's length in guest time.
- The length is 10, 20, 50, 100, 200, or 500 ms.

The shrinker tries the schedule without the pause, the pause at half its length (down to 10 ms),
and the pause at the step's start. `hydo guide flaky-tests` has a worked example.

## Pause threads of the workload

A race between two threads of one process needs one thread held off while the other runs. A
pause of the workload stops both. A thread pause stops only the threads it names:

```sh
hydo run "disk=none | run P=workload/tests::wal_reco@0~4433:500"
```

After 4,433 ms of guest time in step 0, hydo stops every thread of the workload whose name is
`tests::wal_reco`. After 500 ms more, it lets them run again. The other threads run on.

- The name is the thread's name as Linux keeps it in `/proc/<pid>/task/<tid>/comm`: at most 15
  bytes. A longer name is refused with its first 15 bytes, for example `tokio-runtime-w`.
- A name has letters, digits, and `_ . : + -`. `*` matches any characters, and `?` matches one
  character, such as a space.
- `#<k>` picks the k-th thread of that name, counted from 1 in the order the threads were
  created: `P=workload/tokio-rt-worker#2@0~100:200`. Without `#<k>`, every thread of that name
  stops.
- The threads are those of every process of the workload when the pause starts.
- When no thread matches, nothing stops. The control transcript then names the threads the
  workload had: `PAUSED nothing: no thread of the workload matches workload/nosuch (timer; its
  threads: busybox, collection.test, tests::wal_reco, tokio-rt-worker (4))`.

To find a thread's name, run that schedule with a name no thread has, at a start where the threads
exist. The guest kernel's cgroup v2 freezer stops the threads, in a cgroup of the workload's own
cgroup tree.

The pause runs on every engine for an image built at fingerprint epoch 3 or later, that is, by
hydo 0.3.4 or later. Such an image boots a guest kernel with cgroups on the Mac and KVM
engines too. An image built before boots the kernels it was recorded with, and those have no
cgroups on the Mac and KVM engines. Those engines then refuse the pause before any VM boots, and
the message says to rebuild the image with `hydo build`. hvx's guest kernel has always had
cgroups, so hvx runs the pause on every image.

The freezer needs the cgroup2 file system on `/sys/fs/cgroup`. For an image built at fingerprint
epoch 4 or later, hydo mounts it at every boot (`guest.cgroup2`, MANIFEST.md "The cgroup2 mount"),
and the pause uses that mount. For an older image, or with `guest.cgroup2: false`, the pause mounts
it itself when it starts.

`hydo explore --pause-thread <name>[,<name>...]` draws such pauses, as `--pause-workload` draws
pauses of the workload. With both options, the target is drawn among the workload and the names.
Two more options narrow the draw:

- `--pause-min <ms>`: the earliest start (default 0). The starts lie between it and `--pause-max`.
- `--pause-lengths <ms>,...`: the lengths to draw from (default 10, 20, 50, 100, 200, and 500).

## Faults on a cluster

Declare a cluster's nodes as a group, so that one token acts on several of them:

```yaml
groups:
  - name: nodes
    services: [n1, n2, n3]
    primary: /bin/print-leader     # optional: prints the current leader's service name
```

| token | what it does |
|---|---|
| `K=n1,n2,n3@5+2` | kills the three services before round 5, restarts them before round 7 |
| `K=nodes.all@5+2` | the same, for every member of the group |
| `K=nodes.majority@5+2` | kills floor(n/2)+1 members (`nodes.majority.1` picks the next subset) |
| `K=nodes.primary@5+1` | kills the member that `primary:` prints before round 5 |
| `P=nodes.minority@3+2` | pauses floor(n/2) members; `X=` takes the same targets |
| `G=nodes.majority@5+2:flip,file=.blk` | kills floor(n/2)+1 members, flips a bit in one `.blk` file of each, starts them again before round 7; `G=n1,n3@5+2:...` names them |
| `N=nodes.halves@3-9:loss,100%` | cuts the links between two halves of the group (both ways) |
| `N=nodes.isolate.2@3-9:loss,100%` | cuts the third member off from the others |
| `N=nodes.primary@3-9:loss,100%` | cuts the leader off from the others |
| `N=nodes.ring@3-9:loss,100%` | each member hears only a majority, a different one for each member |
| `N=nodes.bridge@3-9:loss,100%` | two halves that reach each other only through one member |
| `N=n1/n2,n3@3-9:loss,100%` | cuts every link between the components `n1` and `n2,n3` |

A partition needs `addr:` on its services. The run's log names the services each selector chose
(`K: nodes.primary = n2`), and the schedule keeps the selector. `hydo explore --cluster-faults`
draws these tokens for a manifest with `groups:` and shrinks a selector towards one member.

## Upgrades and rollbacks

An upgrade token switches a service to another version of itself in the middle of a run, on the
same data. Old and new versions then run side by side, as in a rolling upgrade. List the versions
in the manifest, and put every version's programs in the image:

```yaml
services:
  - name: etcd1
    version: "3.5.5"                   # the version cmd: runs
    versions: [{name: "3.6.0"}]        # cmd:, setup: and ready: may differ per version
    cmd: /opt/etcd-$HYDO_VERSION/etcd --data-dir /data/etcd1
```

| token | what it does |
|---|---|
| `U=etcd1@5:3.6.0` | stops `etcd1` (SIGTERM, SIGKILL after 10 s) before step 5 and starts it on 3.6.0 |
| `U=etcd1@5+4:3.6.0` | the same, and rolls it back to its previous version before step 9 |
| `U=etcd.all@5/3:3.6.0` | a rolling upgrade of the group `etcd`, one member every 3 steps |
| `U=etcd1@5:3.6.0 U=etcd1@9:3.5.5` | an upgrade and a downgrade, as two tokens |

Each command of the service sees its version as `$HYDO_VERSION`. The members of a list or a group
are switched one after another, each once the one before it is ready again. A service that does
not start on its new version ends the run with `VIOLATION [upgrade.started]`. A power cut keeps
each service on the version it ran, also in the recovery boot. `hydo explore --upgrades` draws
these tokens for the services with `versions:`; their failures are in the class `upgrade`. The
manifest reference is the section "Versions of a service and upgrades" of `hydo guide
manifest`, and the example is `hydo/onboard/examples/etcd-upgrade`.

## Knobs (durability settings the explorer may relax)

```yaml
knobs:
  - token: R                      # one capital letter, not a built-in token (K P N O C T Q X F H W B)
    name: Redis appendfsync
    default: always               # the safe value the image starts with
    values: [always, everysec, "no"]
    set: /bin/redis-cli CONFIG SET appendfsync $HYDO_VALUE    # changes it at run time
```

A setting that needs a restart cannot be a knob; say so in the report. A failure found with a
relaxed knob and a power cut is classified as an unsafe setting, not as a bug.

## Freeze at a packet

For bugs that need a service stopped between two messages of a conversation, list `freeze` in
`faults:`. Optionally add `freeze_match:`: payload strings that mark interesting packets, as
lists under `replies:` and `requests:`.

To find the packet, run `hydo explore --sweep freeze --fc-adaptive`. It sweeps the matched
streams (or every eighth packet), then every packet in a widening window around each failure. So
it maps a narrow failing band with a fraction of the runs of `--sweep freeze`, which tries every
packet.

## Clock steps to a time

A system that acts at a time of day or on a date needs the clock to reach that time while the
faults happen. Examples are a biller at minute 10 of every hour, a monthly invoice on the first
of the month, and a certificate that expires at midnight. `T=<step>:@<point>` sets the clock,
before the step, to the next instant of the point, in UTC. A point is `@00:10` (a time of day),
`@:10` (a minute of every hour), or `@hour`, `@day`, `@month` or `@year` (the start of the next
one). An offset can
follow, such as `@:10-2s` or `@month+10m`. The step always goes forward.

```sh
hydo run "disk=none | run T=0:@:10-2s K=redis@1+4"   # 2 s before the next minute 10, then Redis down
```

List the times in `hydo.yaml` so that the explorer draws them among its clock steps:

```yaml
faults: [kill, pause, network, cut, clock]
clock_points: ["@:10-2s", "@month+10m"]
```

Without `clock_points:` the explorer draws only steps by fixed amounts (30 s, 5 minutes, 1 hour,
1 day, 30 days and a year, forward or back).

## One service's clock ahead of the others

`T=<step>:<seconds>` moves the clock of every service at once. A lease, a lock's time to live, or
an order of timestamps breaks when one node's clock differs from the others'. Give that service
`clock: own` in `hydo.yaml` (x86_64 images), then step its clock alone:

```sh
hydo run "disk=none | run T=n2@3:5"         # before step 3, n2's clock jumps 5 s ahead
hydo run "disk=none | run T=n2@3:-1500ms"   # 1.5 s behind
hydo run "disk=none | run T=n2@0:200ppm"    # from step 0, n2's clock runs fast by 200 millionths
```

The other services, the workload and the checks keep the guest's clock. A read of the clock costs
the service about 40 µs of guest time on hvx. The explorer does not draw these steps yet.
The section "One service's own clock" of MANIFEST.md says which programs it covers.

## Clock steps and durations on the wall clock

The number in `T=<step>:<seconds>` is in seconds. The shrinker halves a step while it looks for
the smallest failing one, so a minimal schedule can hold an uneven number: `T=7:-10125` steps
the clock back by 10,125 seconds. The report says it in words, with the hours and minutes:
`the system clock jumps back by 10125 seconds (2 h 48 min 45 s)`.

A system that measures leases, timeouts or visibility on the wall clock fails under such a step.
A step back by D seconds makes a lease last D seconds longer, and a step forward ends it early.
Messages stay invisible, a replica waits, or a lock expires too soon. NTP and operators step a
real machine's clock too. So such a failure is a bug of the system, not of the test: hydo found
it in Valkey (valkey-io/valkey#5406). The fix in the system is a monotonic clock for durations.

When an exploration finds such a failure:

- Keep it as a finding. While a fix is pending, `hydo ci accept <key> --reason ...` marks it
  as accepted (`hydo guide ci`).
- Keep `clock` in `faults:`. If you remove it, write the reason in a comment beside `faults:`.

## Find the durations measured on the wall clock

The clock audit reads the project's source code and lists each place that measures a lease, a
timeout, a throttle, a lag or a delay with the wall clock. For each place it proposes clock
steps aimed at that code. Run it in the repository:

```sh
hydo rules scan --kind clock                 # the 5 most likely places, with their schedules
hydo rules scan --kind clock --all           # every place
hydo rules scan --kind clock --json          # every place as JSON, each with its rank
```

The audit reads Go, C and C++, Rust, Java and Kotlin, Python, JavaScript and TypeScript, Ruby,
C# and SQL. It skips test files, examples, and vendored code. It also skips the test code inside
a source file, by each language's conventions:

- Rust: a `#[cfg(test)]` module and a `#[test]` function;
- Go: `func TestX(t *testing.T)`;
- Python and Ruby: `def test_...` and `class Test...`;
- Java, Kotlin and C#: a method with `@Test`, `[Fact]` or `[Test]`;
- JavaScript: a `describe(` or `it(` block;
- C and C++: googletest's `TEST(`.

The report's head counts what it left out. A place is one of three shapes:

| shape | what the code does | example |
|---|---|---|
| `elapsed` | subtracts an earlier wall-clock reading from a later one and compares the result with a limit | `if now.Sub(lastRefresh) < refreshMin` |
| `deadline` | adds a duration to a wall-clock reading and keeps the result | `SET expires_at = now() + make_interval(secs => @ttl)` |
| `check` | compares a wall-clock reading with a stored deadline | `WHERE expires_at < now()` |

The audit follows a reading through small functions, variables, fields, and parameters. For
example, Go's `time.Now().UTC()` drops the monotonic reading, so a `Now()` method that returns
it is a wall-clock reading too. A project's own wrapper of the clock counts the same way: in C++
the audit reads past namespaces and template arguments, and past a cast such as
`std::chrono::duration_cast<Duration>(system_clock::now().time_since_epoch()).count()`. So
Kvrocks's `util::GetTimeStamp()` and `GetTimeStampMS()` are readings, and the replica's
acknowledgement throttle `now - last_ack_time_secs_ >= 1` is a place. Each place says where its
reading comes from, and the limit with its value when the code states it.

A place is `high` when the code compares the duration with a limit or keeps a lease, and
`medium` when only its names suggest a deadline. Only `high` places get schedules. A reading
that is only logged or kept as a time stamp is not listed.

The report shows one place per function, the most likely first. With `hydo/hydo.yaml` (or
`--manifest FILE`), the places in the code that the services or the workload run come before
all others. The audit maps two kinds of program:

- a binary of a Cargo package (`[[bin]]`, `src/main.rs`, `src/bin/`), with the packages of the
  tree that it depends on (dev-dependencies are left out);
- a Go main package, named after its folder, with the packages of its module that it imports.

A service runs a program when its `cmd`, `entrypoint`, `setup` or `ready` names it, or when its
image's entrypoint does (from `hydo-compose.lock.json`), or else when its image has the
program's name. The workload runs a program that `workload.run` or a file of `hydo/workload/`
names. The report's head says how it mapped them, for example `tikv1, tikv2 and tikv3 run
tikv-server (the image's entrypoint), built from cmd/tikv-server and 60 packages it uses`, and
each place says who runs it. Code in other languages is not mapped: then the places are ranked
by their words alone, and the head says so. Among the places of the same group, these come
first:

- a limit that the code states, between 50 ms and one hour;
- replication and leases, then database connections and request timeouts;
- code whose function name or filename shares a word with the commands of `hydo/hydo.yaml`, such
  as `replication` for a Valkey started with `--dual-channel-replication-enabled`;
- code in the repository's main languages.

Code that logs, reports, caches, or benchmarks comes last. Each `high` place gets three
schedules:

- `back-before`: the clock goes back, then the fault that makes the code run.
- `back-after`: the fault, then the clock goes back while the system recovers.
- `forward-after`, or `forward` for a lease: the clock goes forward by more than the limit.

The fault comes from the words of the code. A replica is killed for replication code, the
database for a connection pool, the lease holder for a lease, and answers are lost for a
request timeout. The schedules name the services of `hydo/hydo.yaml` (or `--manifest FILE`)
when a service has that role, and a placeholder such as `<replica>` when none has. When exactly
one service runs the place's code, the schedules name it instead of `<service>` or `<leader>`.
When several do, or none, or the audit cannot tell, a line under the schedules says why the
placeholder stays. A step back
is at least 5 minutes and ten times the limit. A step forward is longer than the limit.

Try a schedule with `hydo explore --schedule "<schedule>"`, which runs it once. It does not
shrink a failure: leave tokens out by hand to find the ones the failure needs. Move the steps
to where the code runs in your workload. In the Valkey example the audit ranks
`shouldStartChildReplication` (`repl-diskless-sync-delay`, 5 seconds) fourth. Its `back-after`
schedule `disk=none | run K=valkey3@2+3 T=10:-300` is the example's saved
regression for valkey-io/valkey#5406, and it fails on the Mac engine.

The report has a second section: the code that keeps a wall-clock reading as the order or the
version of data. Some systems keep the write with the highest timestamp. After a step back of the
clock, a new write gets an older timestamp than the writes before it, and such a system loses it.
Such a place stores a reading under a name about an order (`timestamp`, `ts`,
`version`). It is `high` when the code also compares two values of that name, in a merge, a
maximum, or a sort key:

```text
2. src/api/s3/copy.rs:293, in handle_copy_metaonly [high]
     let new_timestamp = now_msec();
   It keeps a wall-clock reading as timestamp, and the code orders data by comparing two values of it (src/api/s3/put.rs:723). ...
     back           disk=none | run T=20:-300
     back-long      disk=none | run T=20:-3600
     back-after     disk=none | run K=<service>@2+3 T=10:-300
```

A reading kept higher than the earlier value, such as `max(prev + 1, now_msec())`, is the safe
form, and the audit leaves it out. The writes run in every workload step, so the steps back need
no fault. `back` and `back-long` step the clock back in the middle of the workload. `back-after`
steps it back after a restart of the service that runs the code. In the Garage example the
copy's place is second of 4, and its saved regression `disk=none | run T=17:-3600` loses a copy.
`--json` lists these places as `clock_orders`.

## Clock after a power cut

Every boot starts its clock at `guest.clock_start`. Without `guest.clock_after_cut`, the recovery
boot after a power cut starts its clock at `clock_start` again, so time runs backwards across the
cut. No real machine does that. Set `guest.clock_after_cut: 60`, so the recovery boot continues
the first boot's clock plus 60 seconds, in three cases:

- A check compares a time written before a power cut with one written after it. Without the
  key, the check can report a false violation.
- The system stores times and checks them when it starts: databases with hybrid clocks
  (YugabyteDB, CockroachDB), leases, certificates. Without the key, a service refuses to start
  in the recovery boot ("clock skew"), and the run ends with `VIOLATION service cannot recover`.
  That is a false finding.
- The system records leases or deadlines and serves again only after they run out: a takeover
  lease, a visibility timeout, a lock. Without the key, the recovery boot's clock starts before
  the recorded leases, so the system waits longer than a real machine would. A drain or an
  `eventually_` command can then give up, and the run reports a false finding.

The key covers both kinds of power cut: `C=` and a cut at the k-th disk flush (`cf=`).

## BUGGIFY

`buggify: true` says the programs carry BUGGIFY fault points from an assertion SDK
(FoundationDB style: the code takes its own error path at chosen points). The explorer's
`--buggify` then draws the seed token `B=`. See `hydo guide sdks`.

## Exploring

```sh
hydo explore --seeds 1-50 --strategy process     # a Jepsen-style fault process: the best so far
hydo explore --seeds 1-50 --strategy swarm       # fault categories switched on and off per run
hydo explore --sweep kill                        # every kill point (also cut, net, pause, clock, ...)
hydo report                                      # each problem, with the command that shows it
hydo report --details                            # classes, signatures and minimal schedules
hydo repro <explore dir>                         # re-run every failure, compare fingerprints
hydo save <explore dir>                          # minimal schedules -> hydo/regressions/
```

Useful options of `hydo explore`:

- `--more-faults`: timed kills and k-th-flush cuts. Only the hvx engine has them; the explorer
  refuses the option with `--backend kvm` and on a Mac (`hydo guide capabilities`).
- `--new-faults`: pauses, clock jumps, and one-way loss. The strategies `process` and
  `swarm-pos` draw them without it.
- `--timing`: thread-timing jitter.
- `--concurrency`: concurrent clients, through a manifest knob `Q`.
- `--durable-only`: only durable settings and an honest disk.
- `--protocol-faults`, `--grpc-faults`, `--buggify`.
- `--protocol-cover`: one `H=` token per schedule, chosen so that the schedules reach every
  route of every fake and proxy in every step (section "Reach every route in every step").
- `--storage-faults`: `G=` and `L=` for services with `storage:`. Add `--corrupt-several` to
  make half of the `G=` tokens hit several services: a group's selector, or a list of two.
- `--block-faults`: on the Mac and KVM, one fault of the disk's blocks in 60% of the schedules
  (section "Faults of the disk's blocks").
- `--cluster-faults`: kills, pauses, and partitions of the services of `groups:`.
- `--pause-workload`: pauses of the workload's own processes (section "Pause the workload").
- `--pause-thread <name>`: pauses of named threads of the workload, with `--pause-min` and
  `--pause-lengths` (section "Pause threads of the workload").
- `--vcpus N --turns`: several vCPUs (hvx and KVM) and drawn turn lengths `q=`. `--vcpus N
  --sweep turns` searches the turn lengths of one schedule for a change of verdict (`hydo guide
  flaky-tests`).
- `--steer <id>` or `--steer-match REGEX`: an experimental steered search. It keeps the
  schedules whose runs came closest to failing, by a number the system prints, and changes them a
  little. On the one test it was measured on it found the failure later than the plain
  exploration. The section "Steer the search by a number the test prints" of
  `hydo guide flaky-tests` shows it.
- `--search state`: an experimental state search. A run takes up to 2 snapshots at stops whose
  console shows something rare. Later runs, the branches, start from such a snapshot. Each
  branch draws new faults for the remaining steps. On KVM and on the Mac it also draws new
  timing, with the host token `br=`. The search runs on one worker and writes `state-search.jsonl`. On the bug
  benchmark and on etcd 3.5.5 it found no failure sooner than the plain exploration.
  - `--state-mix N`: branches per 1,000 schedules (default 500). `0` runs the plain
    exploration's schedules with the same bookkeeping.
  - `--state-corpus N`: the most snapshots kept (default 4 on hvx, 32 otherwise).
  - `--state-rare N`: a console feature is rare when at most N runs showed it (default 2).
  - `--state-snaps N`: the most snapshots of one run (default 2).
  - `--state-stop`: stop after the round of the first failure.

  Use `--no-shrink` with it: the shrinker does not know `br=`. The design and the numbers are in
  `docs/design/state-search.md` in hydo's repository.
- `--shrink-causes N`: shrink up to N failures of one signature, each with other fault kinds
  than the ones already chosen (default 1: only the first failure). One last line can have
  several causes; with `--shrink-causes 3` a "fee missing" from a power cut and one from a clock
  step back each get their own minimal schedule. `report.md` names each shrink's fault kinds.
- `--workers N`.
- `--shrink-detected-damage`: shrink also a service that does not start again after hydo
  damaged its files or its disk's blocks. Without it, hydo takes such an error for the system's
  detection of the damage and does not shrink it (`hydo guide workflow`, "Which failures are
  shrunk").
- `--rerun-repeats`: run a first run or a shrink candidate whose schedule the exploration
  already ran in a VM again. Without it, such a run takes the earlier run's result
  (`hydo guide workflow`, "A schedule that ran once runs once").
- `--no-diagnose`: no extra run at the end. Without it, for each problem whose runs stopped with
  an error (at most 4), `hydo explore` runs that schedule once more, as `hydo run` does, into
  `replays/<run>/` of the exploration's folder. That run's diagnosis reads the failing service's
  log, and `hydo report` quotes the log's last lines under the problem.

Before its runs, `hydo explore` prints one line with the fault kinds it draws and the ones that
`--new-faults` adds. The whole fault space, as JSON, is `fault-space.json` in the exploration's
folder.

hydo's own benchmark showed that where faults land, and in what combination, matters more than
how the next schedule is chosen. So prefer `--strategy process` with `--more-faults`. With
`--backend kvm` and on a Mac, use `--strategy process` alone.

One schedule holds one `K=`, one `P=`, one `X=`, and one `C=`; a second one is refused. Several
`N=` tokens are one fault when they share the window and the netem arguments. Every token names
something the image has: a service, a group, a link between services with `addr:`, a fake or
proxy, or a knob of `knobs:`. A token for anything else is refused before the run, with a message
that names what the image has.

To crash or stop several services at once, name them in one token (`K=n1,n2@5+2`) or select them
from a group (section "Faults on a cluster"). To act at the moment the system reports an event,
use reactions: write `A=<event>:kill-<svc>`. `<event>` is the id of an assertion record that the
system writes to the console (`hydo guide manifest`, section `Reactions`).
`hydo explore --reactive fault --react-events <id>` draws them.

`kill-<svc>` acts when the host has seen the record, so the service finishes what it was doing.
To kill it between two commits, write `A=<event>+<ms>:kill-now-<svc>`. A relay in the guest
kills the service `ms` milliseconds after the record, before any thread of the service runs on.
`A=<event>#<k>+<ms>:<action>` acts at the k-th record of the event instead of the first, and
`@<step>` arms the reaction at that step.

The window moves with the program's bytes, so do not guess the delay. Give the client an event
just before the operation, and aim an exploration at it:

```sh
hydo explore --reactive-at meili.swap_acked@20
```

It tries every delay from 0 to 50 ms after the event, each with `kill-now-<svc>`, `kill-<svc>` and
a power cut, one schedule per seed: 153 schedules for one service. `--react-delays 0..200/5`
changes the delays and `--react-actions kill-now` the actions. Name several events with commas,
for example `meili.swap_acked@20,meili.swap_acked#3`. One seed is one cell of the grid: seed `k`
runs cell `k`. Without `--seeds` the seeds are the grid's, 1 to 153 here, and the grid runs once
with the plain workload. `--seeds 1-3` runs only the grid's first three cells, not the grid three
times. Seeds past the grid run it again, each with its seed's composition: `--seeds 1-306` runs
it twice, and `--seeds 1-459` three times. The explorer names the cells of seeds that are not
whole rounds of the grid. Each failure is shrunk and replayed as in any exploration. Take the Meilisearch example on the Mac engine, with an image built at the
fingerprint epoch 5. The swap ran twice first at seed 29, a `kill-meilisearch` 9 ms after the
swap's `202`. The grid of 153 schedules took 4 minutes and 24 seconds with 2 workers.

The window often opens when the server applies the write, after it has answered. Give that
moment an event too: let the client poll the operation's status and write a record when the
server reports it started or finished. A reaction cannot watch a service's log file. The client's
record is the event (`hydo guide manifest`, section `Reactions`).

`--kill-now` makes `hydo explore --reactive fault` draw `kill-now-<svc>` instead of `kill-<svc>`.
Half of its fault reactions are such a kill of any service, and the schedule then has no `K=`
or `P=` on that service. So a project with one service gets these kills too. A schedule has at
most one `kill-now-` reaction.

`A=<event>+<ms>:powerloss-<svc>` cuts the power of one service at that moment, as
`L=<svc>@<step>+<down>` does before a step. What the service had not written back to its own file
system is lost, and it stays down until the heal, as after `kill-<svc>`. It needs a service with
`storage: {dir:, size_mib:}`. `hydo explore --reactive fault` draws it in a quarter of its
fault reactions when `faults:` allows `powerloss`. `--react-actions powerloss` aims a grid at
it, one action per service with `storage:`:

```sh
hydo explore --reactive-at kvrocks.failover_done --react-delays 0..20000/500 --react-actions powerloss
```

A reaction whose event never comes after it is armed does nothing, and the run says so. `hydo
run` prints `hydo: warning: the reaction <token> never fired: ...` with how many records of its
event came, `result.json` has the warning, and `hydo report` shows it. An exploration counts
these runs, and its report says how many runs did not test their reaction. Fix the event's name,
the step that arms it, or the workload that writes the event.

A reaction can also act inside the workload's own work:

- `clock+<s>` or `clock-<s>` steps the clock by `s` seconds at that moment.
- `stop-workload-<ms>` stops the workload's process tree for `ms` milliseconds.
- `stop-workload/<thread>[#<k>]-<ms>` freezes only the workload's threads of that name.
- `kill-workload/<name>[#<k>]` kills the workload's processes of that name, and the rest goes on.

`hydo fuzz --vm` uses them to put a fault inside one call of a function (`hydo guide fuzz`).

## Reach every route in every step

A retry bug at an outside service often needs one route faulted in one step. For example, a
lost answer to the order route in the step that places the order. `--protocol-faults` draws
each `H=` token's route and step at random, so it can miss that pair for many schedules.
`--protocol-cover` walks the pairs instead:

```sh
hydo explore --seeds 1-24 --protocol-cover
```

1. Before the first schedule, `hydo explore` runs a probe: a delay of `step + 1` ms on every
   fake and proxy in every step, for example `H=api@0-1:delay=1 H=api@1-2:delay=2`. The fake's
   record of that run says which routes each step called.
2. A cell is a route, a step, and a rule kind (`status`, `delay`, `drop`, `reset`, and for a
   route with a webhook `dup`, `reorder`, `late`, `lost`). The cells of the pairs that the probe
   saw called come first, in step order; the other cells follow.
3. Seed `s` takes cell `(s - 1) mod C`, where `C` is the number of cells. Its token acts on
   every matching request of that one step. The first round of seeds faults each called pair
   once, and `C` consecutive seeds reach every cell.

`protocol-cover.txt` in the exploration's folder lists the probe and each cell with its seeds.
The rule's values, such as the status code, come from the option's own random stream, so a
seed's other faults stay as they were. With `--protocol-faults` too, the cover's token is the
schedule's first `H=` token, and the drawn ones follow it. A probe that needs more than 4
schedules (each guest part holds at most 500 characters) is not run; then every pair counts as
called.

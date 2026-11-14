# Services and guest settings

One `services:` entry for every long-running process. `hydo guide schema` lists every key; the
rules that matter most:

- `name`: lowercase. Fault tokens use it (`K=db@5+2` kills `db` before step 5).
- `cmd`: runs in the foreground (no daemonizing, no `&`). Its data goes under `/data` (the
  persistent data disk and the fault target; list the directories in `data.dirs`), scratch files
  under `/tmp`. The root filesystem is read-only.
- `setup`: one-time initialization, written to be idempotent, because it runs before every start
  (also after a kill and in the recovery boot after a power cut).
- `ready`: a cheap command that exits 0 when the service answers. Examples are a ping from the
  service's own client, `SELECT 1`, an HTTP health URL, or `grep -qa "<started line>" $HYDO_LOG`.
  For a service that speaks no HTTP, write `ready: tcp:<port>`. The service is ready when a
  connection to that port succeeds. The address is the service's `addr:`, or 127.0.0.1 without
  one. `tcp:<host>:<port>` names another address. hydo runs it as
  `nc -z -w 2 <address> <port>` with the guest's busybox.
- `port`: the port that clients use (network faults act on it). For several services that call
  each other, prefer `network: bridge` with an `addr:` (10.89.0.x) per service. Each service then
  gets its own network namespace and private localhost. Faults on the link between two services
  act on their real traffic (`hydo guide manifest`, section `Service networks`). On a Mac this
  needs a release with the second guest kernel (`hydo guide capabilities`). Without it, `addr:` is a
  loopback address (127.x.y.z) that a link fault sees only if the program's outgoing connections
  leave from it. A program that binds to its own address needs the numeric `addr:` in its
  command line (`--bind 10.89.0.11:7000`), because many programs accept no name there. Names
  work for reaching peers: each service's name resolves through the guest's `/etc/hosts`.
- `depends_on`; `recovery: false` for a stateless tier that the final checks do not need after a
  power cut.
- `start_after: started` for the members of a cluster whose `ready` needs its peers (a quorum, a
  replica count, SQL that needs two storage nodes). hydo starts the services in order and by
  default starts the next one only when the one before it is ready, so such a check never
  passes: `ERROR service start (<first member>)` after `ready_timeout`. With the key on every
  member after the first, the members start together and are checked afterwards, all before the
  workload (`hydo guide manifest`, section `Starting services together`). Alternatively, keep
  `ready` local (the member's own port or health page) and wait for the cluster in a `first_`
  command. `hydo compose` writes the key where Compose's `depends_on` means it: its short form,
  and `condition: service_started`. It prints how each service starts, and why.
- `restart:` (Compose's words) is in force from the service's start. A service that crashes
  while it starts is started again within its `ready_timeout`, as under Docker. For example, it
  crashed because a service it depends on was not ready yet.
- `logs`: the service's own log files besides its output. Give absolute paths, or shell patterns
  (`/data/db/yb-data/tserver/logs/*.INFO`). Databases often write their real log there, not to
  their output. The diagnosis of a failed run and `hydo inspect logs` show their last lines too.
- `restart`: what happens when the process ends by itself during the test, in Docker Compose's
  words (`no`, the default: it stays down until the heal; `always`; `unless-stopped`;
  `on-failure[:max]`). Give a service the policy it has in production (the Compose import copies
  `restart:`). The reference is `hydo guide manifest`, section `Restart policies`.
- `restart_may_fail: true` only for a service whose restart after the faults can fail for a
  reason outside the test. An example is an API whose database sessions hang on a disk that lost
  power at a `cf=` cut. The run then goes on without `workload.retry`, and a power cut's recovery
  boot still gives the verdict. Without a cut the verdict is ERROR, naming the service. The
  reference is `hydo guide manifest`, section `A restart after the faults that may fail`.
- `job: true` for a one-shot job, such as a database migration, whose command the services that
  wait for it run in their `setup:`. Its own service only idles, so the explorer never draws a
  fault on it. `hydo compose` writes the key for Compose's `condition:
  service_completed_successfully` (`hydo guide manifest`, section `One-shot jobs`).
- `provision`: schemas, topics, users: what must exist before the window under test. Seed data
  that belongs to the test is a `first_<name>` command instead.
- Images: `image:` plus `mounts:` (bind, volume, tmpfs) and `user:`; tools inside an image run as
  `hydo-oci <service> <command>`. An image root is read-only. A program that writes into its own
  files needs `writable: true`, for example for a cache in its home directory, or files generated
  into its install directory. The error is often `ENOENT` or `Read-only file system`. The
  writable layer is kept on the data disk: it survives a kill, and a power cut as far as it was
  flushed (`hydo guide manifest`, section `A writable root for image services`). Prefer it to a list of tmpfs mounts found by
  trial.

Guest settings:

- `guest.ram_mib` (default 1024, at most 8192) must hold every service. To size it, run once
  with a large value, for example 4096, and add `--memory`:
  `hydo run "disk=none dseed=1 | run" --memory`. It prints each service's peak memory at the
  end of the run, for example `hydo: memory: redis peak 12 MiB, now 12 MiB (1 process)`. Give
  the guest the sum of the peaks, plus room for the workload, the page cache, and the kernel.
  `--memory` replays the run once more to read the peaks, so the run's fingerprint does not
  change. A process that a fault killed took its peak with it, so read the peaks of a run
  without faults.
- `guest.cpu: v3` when a binary needs AVX or AVX2 (x86_64 only).
- `guest.cpu_cache` is on by default (x86_64). Keep it on with `guest.cpu: v3` when an image is
  built on Red Hat Enterprise Linux 8 or a rebuild of it (AlmaLinux 8, Rocky Linux 8, UBI 8).
  Without it, that glibc's AVX `memcpy` crashes, because CPUID describes no caches.
- `guest.clock_after_cut: 60` when a check compares times written before and after a power cut.
- `guest.outcomes_after_cut: true` when a check reads `$HYDO_OUTCOMES`. Without it, that file is
  empty in the recovery boot after a power cut.

Other top-level keys:

- `knobs:`: settings that trade durability for speed. Each has the safe value as `default`, a
  `set` command that changes it at run time, and one capital letter as `token`
  (`hydo guide faults`).
- `faults:`: the fault kinds that make sense for the project (`hydo guide faults`).
- `errors:`: error logs as findings (`hydo guide error-logs`).
- `buggify: true`: the fault points of an SDK, called BUGGIFY (`hydo guide sdks`).
- `fakes:` and `proxies:` (`hydo guide outside-services`).
- `checks:`: history checks (`hydo guide checks`).
- `params:`: build variants (`hydo build --set k=v`).

A project without a long-running service (a library, an embedded database, a command-line tool)
is fine: leave
`services:` empty and let the commands run the program.

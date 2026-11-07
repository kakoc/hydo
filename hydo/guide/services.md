# Services and guest settings

One `services:` entry for every long-running process. `hydo guide schema` lists every key; the
rules that matter most:

- `name`: lowercase. Fault tokens use it (`K=db@5+2` kills `db` before round 5).
- `cmd`: runs in the foreground (no daemonizing, no `&`). Its data goes under `/data` (the
  persistent data disk and the fault target; list the directories in `data.dirs`), scratch files
  under `/tmp`. The root filesystem is read-only.
- `setup`: one-time initialization, written to be idempotent, because it runs before every start
  (also after a kill and in the recovery boot after a power cut).
- `ready`: a cheap command that exits 0 when the service answers (a CLI ping, `SELECT 1`, an HTTP
  health URL, or `grep -qa "<started line>" $HYDO_LOG`).
- `port`: the port that clients use (network faults act on it). For several services that call
  each other, prefer `network: bridge` with an `addr:` (10.89.0.x) per service: each service gets
  its own network namespace and private localhost, and faults on the link between two services
  act on their real traffic (`hydo guide manifest`, "Service networks"; on a Mac it needs a
  release with the second guest kernel, `hydo guide capabilities`). Without it, `addr:` is a
  loopback address (127.x.y.z) that a link fault sees only if the program's outgoing connections
  leave from it.
- `depends_on`; `recovery: false` for a stateless tier that the final checks do not need after a
  power cut.
- `restart`: what happens when the process ends by itself during the test, in Docker Compose's
  words (`no`, the default: it stays down until the heal; `always`; `unless-stopped`;
  `on-failure[:max]`). Give a service the policy it has in production (the Compose import copies
  `restart:`). `hydo guide manifest`, "Restart policies".
- `restart_may_fail: true` only for a service whose restart after the faults can legitimately fail
  (for example an API whose database sessions hang on a disk that lost power at a `cf=` cut): the
  run then goes on without `workload.retry`, a power cut's recovery boot still gives the verdict,
  and without a cut the verdict is ERROR naming the service. `hydo guide manifest`, "A restart
  after the faults that may fail".
- `provision`: schemas, topics, users: what must exist before the window under test. Seed data
  that belongs to the test is a `first_<name>` command instead.
- Images: `image:` plus `mounts:` (bind, volume, tmpfs) and `user:`; tools inside an image run as
  `hydo-oci <service> <command>`. An image root is read-only; a program that writes into its own
  files (a cache in its home directory, files generated into its install directory: the error is
  often `ENOENT` or "Read-only file system") needs `writable: true`, a container's writable layer
  kept on the data disk: it survives a kill, and a power cut as far as it was flushed (`hydo guide
  manifest`, "A writable root for image services"). Prefer it to a list of tmpfs mounts found by
  trial.

Guest settings: `guest.ram_mib` (default 1024, at most 8192) must hold every service; `guest.cpu: v3` when a
binary needs AVX or AVX2 (x86_64 only); `guest.clock_after_cut: 60` when a check compares times
written before and after a power cut.

Other top-level keys: `knobs:` (settings that trade durability for speed: the safe value as
`default`, a `set` command that changes it at run time, one capital letter as `token`; `hydo
guide faults`), `faults:` (the fault kinds that make sense for the project; `hydo guide faults`),
`errors:` (error logs as findings; `hydo guide error-logs`), `buggify: true` (BUGGIFY points from
an SDK; `hydo guide sdks`), `fakes:` and `proxies:` (`hydo guide outside-services`), `checks:`
(history checks; `hydo guide checks`), `params:` (build variants, `hydo build --set k=v`).

A project without a long-running service (a library, an embedded database, a CLI) is fine: leave
`services:` empty and let the commands run the program.

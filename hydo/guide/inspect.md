# Why a run failed, and looking inside a run without changing it

Use this page when a run did not pass and you do not know why. Use it also when you need the
state behind a verdict to explain it: rows in a database, a file, a log. Do not change the image to find out:
an image with a debug command has other timing, and the failure can disappear. hydo replays a
recorded run exactly, so it can bring the run back to any of its stops and look there; the
recorded run is never changed.

Every `hydo inspect` call prints one JSON document; `--text` prints a view for people. A call that
fails prints `"type": "inspect-error"` with `message` and `try` (commands that would work) and exits
with status 2.

## 1. Read the diagnosis first (free)

`hydo run` writes `diagnosis` into the run directory's `result.json` when a run fails, and prints
it as `hydo: diagnosis: ...`. For an ERROR, and for a violation that names a service
(`service cannot recover ... (<svc>)`), it has already replayed the run once. At the stop where
the verdict was given, it read the evidence (`evidence`):

- every service's log (its output, and the files its `logs:` names), and the setup and
  provisioning logs;
- the readiness check, run once;
- the kernel's memory and crash messages, `free`, and `ps`.

`hydo run --no-diagnose` skips that replay.

```sh
hydo inspect diagnose <run dir>              # the host-side part only, instantly
hydo inspect diagnose <run dir> --evidence   # plus the evidence (one replay of the run)
```

What to read in it:

- `summary`.
- `failure`: one of `service-exited`, `service-not-ready`, `service-setup`, `provision`,
  `restart`, `mount`, `timeout`, `vm`, `control`, `violation`, `other`.
- `process`: the process whose end caused it, with `status`, `signal_name`, and
  `signal_by_hydo: false` for a signal that hydo did not send.
- `probe`: a readiness check, with `why` (`deadline` or `died`), `tries`, and `last_status`.
- `likely`: conclusions with their reason.
- `kernel`.
- `evidence`: each item with `what`, `source`, `text`, and `truncated`.
- `next`: commands to look further.

Typical causes and what they look like:

- a service exits with status 1 at once: its log (evidence `the log of <svc>`) ends with the
  program's own error, such as a bad option, a missing directory, or a read-only path. The image
  root is read-only, so give the program a tmpfs or a directory on the data disk;
- `service-not-ready` with `why: deadline`: the readiness check runs but fails; the evidence runs
  it once and shows what it prints (`Connection refused` on another port, a missing client);
- `signal_name: KILL` with `signal_by_hydo: false`: the out-of-memory killer; `kernel` names the
  process (`Out of memory: Killed process ...`): raise `guest.ram_mib` or make the service smaller;
- status 127: a program is not in the image or not on PATH.
- `VIOLATION service cannot recover (committed data unrecoverable) (<svc>)` after a power cut:
  the service did not start in the recovery boot. The diagnosis gives its exit and its last log
  lines. The cause can be a real finding (the program refuses its own files after the power
  cut) or a setup problem. The verdict alone does not say that data was lost.
- a violation of a check while a service was down (`failure: service-exited` or
  `service-not-ready` with a check's verdict): the check read a service that did not answer.
  Start the check with a test that the store answers, and `exit 2` when it does not.

## 2. Find the moment (free)

```sh
hydo inspect events <run dir> --kind exit,probe,fault,verdict
hydo inspect events <run dir> --grep redis --from run:1 --to run:900 --limit 50
```

Each event has a `point` (`boot:172`, `run:812`): the form `--at` takes. Kinds: `command` (a
command hydo typed), `exit` (a process hydo started ended), `probe` (a readiness check ended),
`done`, `refused`, `fault` (a kill, pause, network fault, power cut), `note`, `report`, `assertion`,
`guest` (a line the guest printed), `kernel`, `snapshot`, `engine`, `end`, `verdict`. A stop is the
state *before* the commands hydo typed at it, so to see the state just after a kill, take the next
stop. `conversations` says, per conversation (`boot`, `run`, `recovery`), which stops can be kept
as a snapshot.

## 3. Hold the stop and look (one replay, then cheap calls)

```sh
hydo inspect start <run dir> --at run:812       # -> {"session":{"id":"i-3f2a9c1e",...}}
hydo inspect logs    i-3f2a9c1e redis --lines 80
hydo inspect exec    i-3f2a9c1e -- redis-cli GET k:11
hydo inspect exec    i-3f2a9c1e --service db --sh 'psql -U app -c "select * from orders"'
hydo inspect read    i-3f2a9c1e /data/redis/appendonly.aof --tail 2000
hydo inspect ls      i-3f2a9c1e /data
hydo inspect ps      i-3f2a9c1e
hydo inspect sockets i-3f2a9c1e --service db
hydo inspect advance i-3f2a9c1e --ms 2000        # let the system run 2 s further, then look again
hydo inspect stop    i-3f2a9c1e
```

`logs` shows the service's output and then the last lines of every file its `logs:` key names
(`hydo guide services`). Without the key, a database's own log under its data directory is read
with `read` or `exec`.

`start` costs one replay up to the stop (about as long as the run took to get there). Each later
call costs a restore and a few engine stops (`cost` in every answer). In a `snapshot` session
every call starts from the stop's state, so a command that writes leaves nothing for the next
call. `advance` is the one call that moves the session's state forward.

Some stops cannot be kept as a snapshot: stops after the workload's end, stops in a recovery
boot, and stops under timing tokens (`tj`, `xt`, `pp`, `q`, `cf`, `pct`). Such a session is
`live`, and calls run one after another on one branch. `--freeze`
stops every process at the stop first (for reading files and processes exactly; a stopped server
does not answer queries). A session ends after 15 minutes without a call (`--idle SECS`).
`hydo inspect list` shows your sessions.

Instead of a session id, every call takes a run directory and `--at`: then it replays, does the one
thing and ends (each such call costs a replay):

```sh
hydo inspect logs <run dir> redis --at end
```

Limits:

- Output is cut at `--max-bytes` (default 16 KiB; `truncated: true`).
- A command runs for at most `--timeout` ms of guest time (default 10,000).
- `--service` runs in the service's own network namespace (`network: bridge`).
- A program inside an image service's root runs with `hydo-oci <svc> <program>`.

## 4. Try several things from one state (branches)

In a `snapshot` session, `exec --branch NAME` runs the command on a branch of the stop and keeps
the state after it as `NAME`; the next call with `--branch NAME` starts there. `--from OTHER` makes
a new branch from another branch instead of the stop. Every answer has `state_fp`, the guest's
whole state after the call: two branches with the same `state_fp` are in the same state.

```sh
hydo inspect exec <session> --branch fix --sh 'cd /data/ws && sed -i s/a/b/ app.py'
hydo inspect exec <session> --branch fix --sh 'cd /data/ws && python -m pytest -q'
hydo inspect exec <session> --branch other --sh 'cd /data/ws && python -m pytest -q'
```

A session holds at most 61 branches on the KVM and Mac engines (5 on hvx), and a branch cannot be
dropped yet.

## A worked example

An image whose provisioning fills memory ends with `ERROR provision failed`:

```
$ hydo run hydo-out/explore-ob-redis-oom-disk.img "disk=none | run"
RESULT one fp=5bb29ba3997bd7bf ... verdict=HYDO-ORACLE: ERROR provision failed
hydo: diagnosis: the provisioning command failed (killed by SIGKILL, status 137)
hydo: likely: the kernel's out-of-memory killer: [   13.760807] Out of memory: Killed process 88 (busybox) total-vm:394628kB, anon-rss:198316kB, ...
```

`result.json`'s `diagnosis.process` says `"handle": "provision", "signal_name": "KILL",
"signal_by_hydo": false`; `evidence` holds the provisioning's log and `free -m` at that stop. The
fix is in `hydo.yaml` (`guest.ram_mib`, or a provisioning step that does not hold everything in
memory); no image change was needed to find it.

A violation (a check failed, no process failed) is explained by the state behind it. Find the
fault with `hydo inspect events <run dir> --kind fault,verdict`, and `start` at the stop after it.
Then read what the check reads: the database rows, the acknowledged ids in `/tmp/hydo/acked`, and
the service's log.

## Share a failure as one page

`hydo report --html` writes what the run directory knows as one web page. The page holds its own
style and its one small script and loads nothing from the network. It opens from disk, and you
can attach it to an issue or keep it as a CI artifact.

```sh
hydo report --html <run dir>                          # <run dir>/report.html
hydo report --html <run dir> --against <passing run dir>
hydo report --html hydo-out/explore -o failures.html  # an exploration
hydo report --html <hydo ci run output dir>           # a CI run, each finding against its base
```

The page shows:

- the verdict and the schedule, the system and the engine, the host's CPU, and the hydo
  version with its fingerprint epoch;
- each failure with its minimal schedule and the command that replays it, with a copy button;
- the failing run's timeline: the events of `hydo inspect events`, the faults, the service
  starts and ends, the checks, and the forced thread switches when the run recorded them;
- where the failing run first differs from a passing run, as `hydo diff` finds it. For a run,
  give the passing run with `--against`, or a `hydo diff` or `hydo explain` directory with
  `--diff`. For an exploration, the page compares each minimal schedule with the passing
  schedules the shrinker ran next to it;
- the properties that held, failed, or were never reached;
- for a CI run, whether each finding is new, pre-existing, still present, or fixed.

A timeline keeps at most 400 events, the main ones first, and a log at most 4,000 bytes. The page
says what it left out and which command shows all of it.

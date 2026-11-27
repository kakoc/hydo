# Rule suggestion: rules that nobody wrote (Milestone 236, tentative number)

Status: first version. `hydo rules scan` and `hydo rules check` are built. The onboarding skill
proposes rules from the scan's evidence and shows the user only the rules that `hydo rules check`
validated. The proof runs are in `docs/HISTORY.md`, Milestone 236.

## The problem

A rule, or check, is a statement about the system's state that must hold after every run. The
onboarding skill proposed rules from a list per domain (a shop, a bank, a job queue) and asked
the owner to review them. That helps only as far as the lists go. Three onboardings of business
systems showed what the lists miss:

- Formance Ledger (Milestone 215): no rule over the ledger's own state found anything. The
  rule over the client's acknowledged operations found the lost and duplicated payments.
- Lago (Milestone 218): both kinds of rule found the known bug lago-api 6901, and only the
  generic rule found usage that a clock step dropped.
- Blnk (Milestone 224): only a domain rule, the overdraft limit, found Blnk's overdraft bug.

The goal is bugs that nobody wrote a test for, found by rules that nobody wrote. The method must
not depend on the domain: it reads the same kinds of sources for a shop, a booking system, a CRM,
an inventory, or a ticketing system.

## The method

The method has three steps: propose, validate, show.

### Propose: rules from what the project promises

A project states its promises in five places. The skill reads each of them:

| source | what it promises | example of a rule |
|---|---|---|
| the database schema | `CHECK`, `UNIQUE`, `FOREIGN KEY`, `NOT NULL`; a status column and its values | no two rows share an idempotency key; no invoice in state `finalized` without a number |
| the API | the operations, their status codes (`409`, `422`), idempotency headers, limits | a request repeated with the same key creates one object |
| the documentation | words such as `never`, `exactly once`, `at most`, `idempotent`, `guarantee`, `must` | a balance never goes below its overdraft limit |
| the project's own tests | the assertions after an operation | a refund never exceeds the captured amount |
| the client library | what a caller may rely on: retries, keys, error meanings | a failed call left no effect |

`hydo rules scan <dir>` lists this evidence with the file and the line. It reads SQL files,
schema files of common frameworks, OpenAPI files, Markdown, and test files, and knows no domain.
It does not write rules: the skill turns the evidence into rules.

Each proposed rule is an entry of a candidates file (`hydo guide rules` has the format):

- `says`: the rule in plain words;
- `evidence`: the files and lines it comes from;
- `kind`: `sql` is a query that must return no rows. `command` is a check command, for example
  the generic rule over the client's acknowledged operations or a `hydo-sql check` line.
  `messages` and `history` are claims that need the workload's records;
- `confidence`: `high` when the project states the promise in words or in a constraint, `medium`
  when the code implies it, `low` when only the domain suggests it;
- `cases`: what the rule judges, as a count (a query or a command), so that a rule over nothing
  is found;
- `within`: optional, the seconds a rule about what the system does after the workload may take;
- `mutate`: optional, a change of the state that must break the rule.

The generic rule is always proposed: faults must not change the business result that the
client's acknowledged operations determine. In two of the three onboardings it found failures
that every domain rule missed.

### Validate: clean runs before the user sees a rule

`hydo rules check <candidates file>` runs the image's workload without faults on several
composition seeds (default 3), and evaluates every candidate after each run. A clean run is a
run of the schedule `disk=none | run W=<seed>`. The project's own checks are left out of these
runs, so that the candidates are judged alone. The command reports each rule as one of:

| status | meaning | what the skill does |
|---|---|---|
| `holds` | it held in every clean run, and judged at least one case | show it to the user |
| `vacuous` | it held, but its `cases` count was 0 in every run | do not show it; fix the workload or the rule |
| `fails-clean` | it failed in a run without faults | report it apart: the rule is wrong, or the system has a bug on its normal path |
| `unevaluated` | it could not be evaluated (exit status 2, a missing program, a timeout) | fix the rule |
| `not-run` | its kind needs the workload's records (`messages`, `history`) | write it into the project and run it |

With `--mutate`, one more clean run checks the rules that hold and have a `mutate` entry. In
that run each such rule runs, then its change is made, then it runs again and must fail. A rule
that still holds is blind to the change it was meant to catch (`blind`). One run serves all the
changes. A rule that already fails before its own change was broken by an earlier change, and
gets a run of its own.

The command reuses what exists. Each candidate becomes a final invariant of the run, added by
the host controller to the image's plan, so no image is rebuilt. The `eventually_` commands of
the workload stay and run before the candidates. Each candidate runs in a wrapper whose last line
reports the candidate's exit status and case count. The wrapper fails only when the candidate
failed, so a candidate that cannot be evaluated does not turn the run into an error. The command reads those lines from the run's
`HYDO-ASSERT` records. `--write-checks DIR` writes each rule that holds as a check file
(`final_<id>.sql` or an executable `final_<id>`), ready for `hydo/checks/`.

### Show: plain words first

The skill shows the user only the rules that hold, each with its plain statement first, then
its evidence and its confidence. The rules that failed without faults are a separate list: each
one is a wrong rule or a bug on the normal path, and the user decides which.

## What the proof runs taught

The proof ran on Blnk, Lago, and listmonk, which hydo had never seen (`docs/HISTORY.md`,
Milestone 236; the candidates files are in `docs/design/rule-suggestion/`):

- A rule that fails without faults is most often a wrong rule. Blnk's rule over the client's
  operations failed until it used Blnk's own name for a hold's commit. In listmonk the failing
  rule showed an inconsistency of the app itself.
- A rule that holds on clean runs has not been checked yet. Blnk's overdraft rule held on three
  compositions and failed on the one with concurrent draws. `--schedule` replays a known
  failure's schedule against the candidates.
- `cases` must count the rows that the rule's query judges. A Blnk bulk rule counted the bulks,
  looked at rows that Blnk never applies, and held on the failure it was meant to find.

## The clock audit (Milestone 248, tentative number)

On 2026-11-14 four bugs that hydo found had one cause: code that measured a duration with the
wall clock instead of a monotonic clock. They were in Valkey (valkey-io/valkey#5406), Temporal
(temporalio/temporal#13099), River (finding 2 of the River example), and a private queue. A step
of the clock back made a wait last longer by the size of the step, and a step forward ended a
lease early. `hydo rules scan --kind clock` finds such code and aims clock steps at it. It is
evidence of a sixth kind: it proposes schedules, not rules.

### How it reads the code

The audit works on text, without a compiler. It reads Go, C and C++, Rust, Java and Kotlin,
Python, JavaScript and TypeScript, Ruby, C#, Erlang and Elixir (since Milestone 382), and SQL. It
skips tests, examples, and benchmarks. Bundled third-party code has a section of its own (Milestone
382, below). It runs in four steps:

1. It removes comments and the contents of strings from each line. SQL keeps its strings.
2. It finds the calls that read the wall clock, for example `gettimeofday`, `time(NULL)`,
   `SystemTime::now()`, `System.currentTimeMillis()`, `time.time()`, `Date.now()` and SQL's
   `now()`. Go's `time.Now()` keeps a monotonic reading, so it counts only against a value that
   lost it: `time.Now().UTC()`, `.Round(0)`, `.Unix()`, or a time read from storage.
3. It follows a reading through the code until nothing changes:
   - a function of at most 30 lines, with a name about time, that returns a reading;
   - a variable, a field or a parameter that is set to a reading.
4. It reports a line that uses a reading in one of three shapes:
   - `elapsed`: a subtraction compared with a limit;
   - `deadline`: a reading plus a duration;
   - `check`: a reading compared with a stored deadline.

Several guards keep the precision:

- A value is a reading only when the expression starts with one, so `buf = format(buf, now)`
  is not followed.
- A hop from one variable to another needs a name about time on one side, so `x.BuildId =
  y.Version` is not followed.
- A name with a second implementation that reads a monotonic clock is not followed.
- A line that only logs, prints or records a metric is dropped, and so is an overflow guard
  such as `LLONG_MAX - now`.

A place is `high` when it compares a duration with a limit or keeps a lease or a deadline, and
`medium` when only its names suggest a deadline. The limit's value comes from the code when it
is written as a constant, a `#define`, or a default in a table of settings. The unit of a bare
number comes from the name of the reading (`mstime`, `unixtime`).

### The ranking

The report shows one place per function, the 5 most likely first, and one line for the rest
(`--all` shows every place; `--json` has every place with its rank). The rank adds points:

| what | points |
|---|---|
| a limit the code states, from 50 ms to one hour | 4 (another stated limit: 1) |
| a lease or replication; a database connection or a request timeout | 5; 3 |
| a word of waiting, throttling or retrying (`refresh`, `retry`, `lag`, `timeout`) | 1 |
| a word of the function's or the file's name in the commands or environments of hydo.yaml | 3 |
| a measured time (`elapsed`) | 1 |
| a minor language of the repository (less than a fifth of its source files) | -5 |
| code that logs, reports, caches, or benchmarks | -6 |

The words of hydo.yaml leave out the project's name, the image's name and plain words such as
`server` or `port`. For Valkey the services' commands hold `replication` and `repl`, so the
functions of `replication.c` gain 3 and the cluster and Sentinel code does not. Equal ranks keep
the path order.

### The schedules

Each function with a `high` place gets three schedules:

- the clock goes back, then the fault that makes the code run;
- the fault, then the clock goes back while the system recovers;
- the clock goes forward by more than the limit.

A step back is the first of 5 minutes, 1 hour, 1 day, and 30 days that is longer than ten times
the limit. A step forward is the first of 30 seconds, 5 minutes, 1 hour, 1 day, and 30 days that
is longer than 1.5 times the limit. Without a limit both steps are 5 minutes. The fault
comes from the words of the function, the file and the line, in this order:

| words | fault |
|---|---|
| lease, leader, election, lock, heartbeat | the lease holder is killed (a group's `primary`) |
| replica, replication, follower, `repl`, `sync` | a replica is killed (a service whose command makes it a replica) |
| database, persistence, connection, pool, session | the database is killed (the one database service) |
| timeout, deadline, request, retry, `ack` | 30% of the answers of a service are lost |
| none of these | a service is killed |

The services come from `hydo/hydo.yaml`. When no service has the role, the schedule holds a
placeholder such as `<replica>`.

### What the proof runs measured

The tagged sources of the three public cases, read by the audit:

| project | source files | places | high | functions | the known line, and its rank with the example's hydo.yaml (without) |
|---|---|---|---|---|---|
| Valkey 9.1.3 | 255 | 159 | 144 | 96 | `replication.c:4962` (`refreshGoodReplicasCount`): 2 (4); `replication.c:5502` (`shouldStartChildReplication`): 4 (6) |
| Temporal 1.32.2 | 1,515 | 191 | 109 | 91 | `common/persistence/sql/sqlplugin/db_handle.go:98`, limit 1 second: 1 (3) |
| River v0.51.2 | 595 | 50 | 25 | 15 | `riverpgxv5/internal/dbsqlc/river_leader.sql` (lines 19, 27, 30, 36 and 49): 2 (2) |

Without hydo.yaml, Valkey's cluster and failover code ranks before its replication code, because
nothing tells the audit that the services run neither cluster mode nor Sentinel.

A hand review read a seeded sample: 40 places of Valkey, 40 of Temporal, and all 50 of River. It
counted a place as correct when the place measures a duration or keeps a deadline with the wall
clock:

- Valkey: 38 of 40 (2 were overflow guards, now excluded).
- Temporal: 34 of 40 (a fake test clock's `Now()`, a method named `Sub`, and calendar
  arithmetic of the scheduler).
- River: 50 of 50.

Five of the 130 places only fed a log line or a metric. The review used the audit before its last
fixes. The overflow guards and the method named `Sub` are gone from the counts in the table; the
fake test clock and the calendar arithmetic remain. Six projects that keep
their timeouts on a monotonic clock gave 8 places in all:

- hashicorp/raft v1.7.1 and bb8 0.8.5 gave none.
- urllib3 2.2.3 gave 2: a `Retry-After` date, and a test server.
- libuv v1.48.0 gave 3: the uptime from the boot time, on three systems.
- HikariCP 5.1.0 gave 2: its millisecond clock source.
- node-lru-cache v10.2.0 gave 1: a time stamp in its dump.

Each of the 8 reads the wall clock on purpose. None is a monotonic timeout that the audit took
for a wall-clock one.

For the Valkey example, the audit names `valkey3`, the last replica of `hydo.yaml`. On the Mac
engine its `back-after` schedule `disk=none | run K=valkey3@2+3 T=10:-300` failed with the
example's recorded fingerprint `d3c8c0fed4a3dbd8`. The `back-before` schedule passed, because
the replica's request is stamped after that step. The `forward-after` schedule passed too: a
step forward only shortens the 5-second wait.

### Orders and versions (Milestone 350)

The Garage onboarding (Milestone 349) found a bug of another shape. Garage's CopyObject takes the
timestamp of its new version from `now_msec()`, while PutObject keeps it above the key's newest
version. Garage keeps the version with the highest timestamp. So after a step back of the clock,
a copy onto an existing key is acknowledged and has no effect. No duration is measured there,
and the audit did not see it.

The audit has a second kind of place now, the shape `order`. A line is one when it keeps a
reading of the clock as the order or the version of data:

- The reading is the value of a name about an order: `timestamp`, `ts`, `version`, `seq`,
  `revision`, `mtime`, `updated`. A variable counts when the same function stores it in such a
  field (`let new_timestamp = now_msec();` and later `timestamp: new_timestamp,`). A name about
  a duration or a record, such as `created`, `expires`, or `lease`, does not count.
- The line reads the clock itself, or calls a function whose name says that it does (`now`,
  `clock`, `timestamp`). A time made from a stored value, such as Go's `time.Unix(0, 0)`, does
  not count, and neither does a stamp formatted for a log.
- A reading kept above the earlier value, `max(prev + 1, now_msec())`, is the safe form and does
  not count.

A place is `high` when the code compares two values of its name: two values of one field on a
line with a comparison or a maximum (`other.ts > self.ts`), a field that a maximum or a sort keys
on (`.map(|v| v.timestamp).max()`, `sort_by_key`), or a field of `self` in a function named for
an order key (`fn cmp_key`). Only `high` places get schedules. The places have a section of their
own in the report, ranked by the same points, with their own 5 shown, so the durations keep their
ranks. Each gets three schedules:

| name | schedule | what it tests |
|---|---|---|
| `back` | `T=<middle>:-300` | a write after the step gets an older version than the writes before it |
| `back-long` | `T=<middle>:-3600` | the same, with a step longer than the time between two writes of one key |
| `back-after` | `K=<service>@2+3 T=10:-300` | the writes of a service that restarted get older versions |

On Garage v2.5.0, with the example's hydo.yaml, the audit names 4 functions, all `high`, and the
copy's `now_msec()` (`src/api/s3/copy.rs` line 293) is the second. M349's regression of the copy
is `back-long` at step 17. The other three are timestamps of a key's permissions on a bucket and
the constructor of Garage's last-writer-wins register; `next_timestamp` and the register's
`update` keep their readings above the earlier ones and are not listed. Valkey and River have no
such place, and Temporal has 4, versions stamped with `time.Now().UnixNano()` that a merge
compares.

The same milestone fixed what the limits said. The audit explained Garage's places with "10
minutes by default (src/block/manager.rs:49)", a constant of another file, because it took the
type `Duration` of `Duration::hours(24)` for the limit's name. A type is no limit now. A plain
word such as `timeout`, `idle`, or `interval` is read in the place's own file only, and a name
of two words or in capitals in every file, as before. A literal duration of Rust, such as
`Duration::hours(24)`, gives the limit when no name does. Garage's `now_msec()` returns a chain
that starts with `SystemTime::now()` on one line and goes on over three more, and a Rust tail
expression is now read from the first line of its chain.

### A project's own wrapper of the clock (Milestone 355)

The Kvrocks onboarding (Milestone 351) found that the audit named no function in Kvrocks 2.17.0,
although its finding 2 is a wall-clock throttle. Kvrocks reads the clock through its own helper:

```cpp
template <typename Duration = std::chrono::seconds>
auto GetTimeStamp() {
  return std::chrono::duration_cast<Duration>(std::chrono::system_clock::now().time_since_epoch()).count();
}
inline uint64_t GetTimeStampMS() { return GetTimeStamp<std::chrono::milliseconds>(); }
```

The audit already followed a small function with a name about time that returns a reading. It
missed this one for three reasons of C++ text, which it now reads in C and C++ files only:

- A reading starts at its namespaces: `std::chrono::system_clock::now()` and
  `util::GetTimeStamp()` start where `std::` and `util::` start, so an assignment's right-hand
  side starts with the reading.
- A call with template arguments is a call: `GetTimeStamp<std::chrono::milliseconds>()`.
- A cast of one reading keeps it a reading: `duration_cast<...>(`, `time_point_cast<...>(` and
  `static_cast<...>(`, when the cast's argument is not a difference.

A function that returns a reading is also no limit any more, so `util::GetTimeStampMS() -
last_connect_timestamp < 1000` does not name `GetTimeStampMS` as its limit.

On Kvrocks 2.17.0 with the example's `hydo.yaml` the audit names 22 functions in 255 source
files, where it named none. The fourth is finding 2's place, `src/cluster/replication.cc` line
670 in `sendReplConfAck` (lines 666 to 682), `high`, with its reading traced to `now =
util::GetTimeStamp()` and `system_clock::now()`. The third is the reconnect throttle of the same
file, line 362. The JSON and text reports of Valkey 9.1.3, Temporal 1.32.2, River v0.51.2 and
Garage v2.5.0, each with its example's `hydo.yaml`, are the same as before, byte for byte, so
their known places keep their ranks.

### Bundled code, a project's own clocks, and Erlang (Milestone 382)

Most clock findings of the onboardings came from blind explorations, and the audit had named
none of their places. Typesense's two are in h2o and braft, which Bazel fetches outside the
project's folder. VictoriaMetrics's three read a cached clock (`fasttime.UnixTimestamp()`) or a
deadline of `time.Now().Add(d).UnixMilli()`. Restate's and GreptimeDB's read their own wrappers
of the clock, and RabbitMQ's finding 3 is in Erlang, which the audit did not read. Five changes
close these gaps; each is in `hydo/crates/hydo-cli/src/clockscan.rs`.

1. **Bundled code.** The audit reads `vendor/`, `third_party/`, `deps/`, and Bazel's
   `bazel-<name>/external/`, each library by its name. A folder of `deps/` that the root's
   `.gitignore` keeps (`!/deps/rabbit/`) is the project's own. Bundled code has a section of its
   own after the project's places, and there a library's timers come first: a place whose
   function or file names a timer, a timer wheel, an event loop, or a scheduler gets 4 more
   points. The keys that bundled code sets are its own, so a library's `Now()` or `timestamp`
   field makes no reading in the project's code, and the project's main languages, functions and
   ordered names are counted without it.
2. **A project's own clock, by data flow.** A function of at most 30 lines whose returned value
   comes from a reading inside it is a reading, whatever its name. Inside means the clock itself,
   a cached clock, or a variable of the function that holds one; a parameter does not count, so a
   conversion such as Prometheus's `FromTime(t)` is no clock. The other new readings:
   - a cached clock: a cell that an atomic store sets from a reading (`x.Store(v)`,
     `atomic.StoreInt64(&x, v)`, Rust's `x.store(v, ...)`), and every load of it;
   - Go's `time.Time` with its monotonic reading from a ticker's channel, or `time.Now()` given to
     a function whose name speaks of time, once `.Unix()`, `.UTC()` or `FromTime(...)` drops it;
   - a Go chain such as `time.Now().Add(d).UnixMilli()`, a deadline;
   - a function that returns now plus a duration (`butil::milliseconds_from_now(ms)`, from its
     name or from `return nanoseconds_from(time, ns)`), a deadline wherever it is called;
   - Rust paths from their first segment (`chrono::Utc::now()`), a time's own type made from a
     reading (`MillisSinceEpoch::new(...)`), a tail over several lines, and a method's call
     without its receiver among the parameters;
   - C's `gettimeofday(&loop->_tv_at, NULL)`, which sets the field `_tv_at`, and a field read
     through `->`.
3. **A stored reading compared later.** The shape `rewind` compares a time with a reading that
   the code stored in a field. A new reading against it counts in a timer's code (h2o's `if (now
   < ctx->last_run)`). A timestamp of data against it counts when its name marks a start
   (Prometheus's `if s.T > w.startTimestamp`). Both are `high`.
4. **Erlang and Elixir.** The audit reads `erlang:system_time`, `os:system_time`,
   `erlang:now()`, `os:timestamp()`, the `calendar` and `erlang` local and universal times,
   Elixir's `System.system_time` and `DateTime.utc_now`, and not `erlang:monotonic_time`. A
   reading is followed through a match (`Ts = ...`), a key of a map or a record (`#{ts => Ts}`,
   `maps:get(system_time, Meta)`, `S#state.last_run`), and a fold's clause that renames a key
   (`fun (ts, V, Acc) -> Acc#{system_time => V}`). This is how Ra's command timestamp reaches
   RabbitMQ's stream coordinator.
5. **A fallback.** When the project's own code has no place, the report says so and gives
   schedules aimed at no place: a restart and a step back for each service, a step back before a
   failover of each group with `primary:`, a step back of a day, and a step forward of an hour.
   `--json` has them as `fallback`.

The words of a due time (`run_time`, `duetime`, `due_at`) and of a retention (`retention`) now
make a deadline.

**The known places.** The tagged sources, each with its example's `hydo.yaml`. Typesense's
three repositories are at the commits of its `WORKSPACE`, in `bazel-typesense/external/`, a link
as Bazel makes it. Ra 3.2.0 is in RabbitMQ's `deps/ra`, as erlang.mk fetches it. A rank is
among the places with schedules of the project's own code, or of the bundled code's section.

| project | finding | the place | before | after |
|---|---|---|---|---|
| Typesense v30.2 | 2: h2o's event loop | `h2o/include/h2o/socket/evloop.h:116`, a timer at now plus its delay | not read | 4 of 45, bundled |
| Typesense v30.2 | 3: braft's election timer | `braft/src/braft/repeated_timer_task.cpp:129` | not read | 2 of 45, bundled |
| Typesense v30.2 | 3: brpc's timer thread | `brpc/src/bthread/timer_thread.cpp:372` | not read | 23 of 45, bundled |
| VictoriaMetrics v1.156.0 | 1: the retention watcher | `lib/storage/table.go:458` | not listed | 32 of 73 |
| VictoriaMetrics v1.156.0 | 2: the flush deadline | `lib/storage/raw_row.go:248` (and the checks at 256 and 330) | 3 of 21 (the check at 330) | 2 of 73 (the store of the deadline) |
| VictoriaMetrics v1.156.0 | 3: the cached clock against a deadline | `lib/handshake/buffered_conn.go:112` | not listed | 24 of 73 |
| Kvrocks 2.17.0 | 2: the replica's acknowledgement | `src/cluster/replication.cc:670` | 4 of 22 | 4 of 22 |
| Restate v1.8.1-hotfix.2 | the scheduler's `run_at` | `crates/vqueues/src/scheduler/vqueue_state.rs:357` | not listed | 10 of 17 |
| GreptimeDB v1.3.1 | the metasrv's leases | `meta-srv/src/procedure/region_migration/downgrade_leader_region.rs:286`; the failure detector `meta-srv/src/failure_detector.rs:136` | not listed | 3 and 4 of 9 |
| RabbitMQ 4.4.1 | 3: the coordinator's retry | `deps/rabbit/src/rabbit_stream_coordinator.erl:2350` (the timer's delay) and `:2393` (the retry time) | Erlang not read | 2 and 6 of 33 |
| Prometheus v3.16.0 | 2: the scrape's alignment | `scrape/scrape.go:1456` | 3 of 4 | 4 of 5 |
| Prometheus v3.16.0 | 3: the WAL watcher's start | `tsdb/wlog/watcher.go:560` | not listed | 3 of 5 |

The files read and the functions with schedules (the project's own; bundled code):

| project | files before | files after | functions before | functions after |
|---|---|---|---|---|
| Valkey 9.1.3 | 255 | 255; 314 | 96 | 97; 5 |
| Temporal 1.32.2 | 1,515 | 1,515 | 91 | 92 |
| River v0.51.2 | 595 | 595 | 15 | 16 |
| Garage v2.5.0 | 205 | 205 | 9 | 9 |
| Kvrocks 2.17.0 | 255 | 255 | 22 | 22 |
| Typesense v30.2 | 208 | 208; 1,113 | 14 | 14; 45 |
| VictoriaMetrics v1.156.0 | 931 | 931; 4,176 | 21 | 73; 76 |
| Restate v1.8.1-hotfix.2 | 1,155 | 1,155 | 8 | 17 |
| GreptimeDB v1.3.1 | 1,833 | 1,833 | 4 | 9 |
| RabbitMQ 4.4.1 | 9 | 1,161; 53 | 0 | 33; 1 |
| Prometheus v3.16.0 | 612 | 612; 6 | 4 | 5 |

**What stayed.** The reports of Kvrocks 2.17.0 and Garage v2.5.0 are the same as before, byte
for byte, both the text and the JSON. Valkey 9.1.3, Temporal 1.32.2 and River v0.51.2 keep their 5 places
shown and every known place's rank:

- Valkey gains two places of the shape `rewind`: `manualFailoverCheckTimeout`'s `mf_end <
  mstime()` at rank 49, and `clusterCron`'s oldest pong at rank 80. Its `deps/` has a section of
  its own: 314 files, 5 places with schedules, all in libvalkey.
- Temporal's `task_generator.go:355` (`closeTime.Add(retention)`) becomes `high`, at rank 65.
- River's four retention horizons of `RunHandle.java` become `high`; the first is at rank 15.

Each of these compares a time of the wall clock. The JSON of the three keeps every other place,
with ranks moved by the places added before them.

**Precision.** A seeded sample of 30 of the 236 new places with schedules (seed 382), each read in
its code. 25 measure a duration, keep a deadline, or keep an order with the wall clock, so a
step moves them. The 5 others are vendored Go code of VictoriaMetrics. Two reach a buffer named
`buf` through a function that formats a time (lz4, go-json). Three are `order` places that keep
the time of a request or a query, not of data: gRPC's xDS metadata, an AWS signature, and
VictoriaLogs' query time.

**The cost.** The CPU time of one `--json` scan on the shared bare-metal machine, before and
after:

| project | bundled files | CPU seconds | memory |
|---|---|---|---|
| Temporal 1.32.2 | 0 | 6.5, 6.6 | 75 MB, 77 MB |
| Valkey 9.1.3 | 314 | 2.4, 4.3 | 45 MB, 62 MB |
| Typesense v30.2 | 1,113 | 1.0, 4.8 | 38 MB, 106 MB |
| RabbitMQ 4.4.1 | 53 | 0.0, 5.2 (Erlang read) | 10 MB, 75 MB |
| VictoriaMetrics v1.156.0 | 4,176 | 1.7, 24.0 | 42 MB, 209 MB |

The bundled code costs time in proportion to its size; a project without it costs what it did.

### What the clock audit does not do

- It reads text, so a reading passed through an interface with several implementations, or
  through a channel, a queue, a closure, or a callback, is not followed. GreptimeDB's
  `build_active_filter` and Restate's `gardal` clock are two.
- A library of bundled code is ranked by its words. Nothing tells the audit which library the
  services lean on. So Typesense's own 14 functions come before h2o's and braft's timers, and
  brpc's timer thread is at rank 23 of 45.
- A limit without a value in the code ranks low. VictoriaMetrics's retention watcher is at rank
  32 of 73.
- An order or a version that a SQL statement sets, or that a function of another file gets as a
  parameter, is not found. The comparison that makes such a place `high` is matched by the
  field's name in every file, so two structures with a field `timestamp` share it. A field set
  from a converted Go `time.Time` is followed in its own file only (Milestone 382); the others
  are followed by their name in every file.
- Erlang's maps and records are read by their keys, whatever the record. A message that carries
  a reading to another process is not followed.
- The schedules place the steps at fixed workload steps (the fault before step 2, the step
  back before step 10 of a 20-step workload). Code that runs at another time needs the steps
  moved by hand.
- The private queue's code was not available, so its pattern is checked only by a unit test.

## The write routes (Milestone 344, tentative number)

`hydo rules scan --kind writes` lists the routes of a project's API that write. An onboarding
writes one workload command per client operation, and an operation that nobody lists is not
tested. Meilisearch's index swap, its dump, and the payload of a document addition held the three
bugs that hydo found there, and the onboarding found those operations by reading the sources.

### How it reads the code

The scan is in `hydo/crates/hydo-cli/src/writescan.rs`. It reads text, as the clock audit does,
and it uses the audit's rules for what to leave out: test files, test code inside a source file
(Milestone 324), examples, vendored code, and generated code. Each file becomes one text without
comments, with its strings, and with each call's name, receiver, and arguments.

- A router call is a method named `post`, `put`, `patch`, or `delete` whose first argument is a
  path, such as Go's `r.Post("/write", h)` or express's `app.post("/orders", h)`. A Go file needs
  a router package among its imports, and a JavaScript file needs a server framework, so that a
  client's `axios.post(...)` is not a route. Receivers such as `http`, `client`, and `axios` are
  clients.
- Rust's axum and actix-web routes are `.route(<path>, <methods>)` and
  `web::resource(<path>)` with the methods of their router expression, such as
  `get(a).post(b)` and `web::post().to(h)`. A route table of an attribute macro,
  `"<path>" => [get(a), post(b)]`, belongs to the `struct` after it, and `sub(Type)` mounts
  another table.
- Decorators and annotations name the function after them: FastAPI and Flask, NestJS, Spring and
  JAX-RS, and the attributes of actix-web and rocket. A Django URL pattern names a view, and the
  view's methods (`def post`, a REST framework base class, `@require_POST`) give the routes.
- An enumeration of endpoints in Rust (Milestone 350) is an `enum` whose variants are an API's
  operations, chosen by the project's own code instead of a router library. A match on the
  request's method gives each variant its method, when its arm names the variant or calls a
  function of the file whose arms name the variants: `Method::PUT => Self::from_put(...)`, and
  in `from_put` the arms `EMPTY header "x-amz-copy-source" => CopyObject` and `EMPTY =>
  PutObject`. A table in a macro's invocation that lists `POST CreateKey (body),` does the
  same. The handler is the first call of the arm that matches the variant (`Endpoint::PutObject
  { key } => handle_put(...)`), or of the block of an `if let`, or else the first method of a
  type named for the variant (`impl RequestHandler for CreateKeyRequest`). An operation that no
  code handles is left out. The route's path is `<Enum>::<Variant>`, its resource the variant's
  first noun, and its class comes from the variant's words.
- A `.proto` service gives one route per method, with its HTTP mapping when it has one. A Go type
  that embeds `Unimplemented<Service>Server` and a Rust `#[async_trait] impl Service for T` whose
  methods take a `Request<...>` give the methods of a gRPC server without a `.proto` file in the
  project.
- An OpenAPI or Swagger file gives one route per operation, and its `operationId` names the
  handler.

The prefixes are joined from three places: the calls that enclose a route (axum's `.nest`,
actix-web's `web::scope`, chi's `r.Route`), the groups that a file assigns (gin's and labstack's
`r.Group`, gorilla's `PathPrefix(...).Subrouter()`, FastAPI's `APIRouter(prefix=...)`,
Flask's `url_prefix`), and the mounts by name (`.nest("/p", routes())`, `sub(Type)`, and a
group passed to a function, as in Prometheus's `api.Register(router.WithPrefix("/api/v1"))`).

A handler named in an expression is its innermost identifier, so `wrapAgent(api.deleteSeries)`
names `deleteSeries`, and the scan finds its definition among the functions of the code. A
route that two places declare, such as a `.proto` method and its gateway's OpenAPI file, is
listed once with both places.

### Reads and classes

A `POST` route or an RPC method is a read when the last segment of its path, or the first word
of its handler's name, is a word of reading (`query`, `search`, `get`, `list`, `fetch`, and 50
others), and neither has a verb of change. Such routes are counted and left out.

Each route that writes gets a class by the words of its path and its handler: `runtime` for the
process's own state (health, metrics, profiling, a reload), `session` for a sign-in or a token,
`admin` for an operation on the stored data as a whole (a snapshot, a dump, a compaction), and
`data` for the rest. The list ranks resources by their best route, and the routes of a resource
by class, so durable writes come before logins and metrics.

### What the proof runs measured

The scan ran on the sources of six examples at their tags, with a debug build on this Mac. A
sample of 20 routes per project, spread evenly over the ranked list, was judged by reading each
handler: a route is right when its handler changes state of the system. Prometheus has 14
routes, so all of them were judged.

| project | tag | files read | routes that write | resources | reads left out | handlers found | right in the sample | seconds |
|---|---|---|---|---|---|---|---|---|
| Meilisearch | v1.57.0 | 576 | 83 | 23 | 10 | 83 | 19 of 20 | 5 |
| Iggy | server-0.10.0 | 2,038 | 38 | 17 | 2 | 38 | 20 of 20 | 10 |
| Prometheus | v3.16.0 | 616 | 14 | 7 | 12 | 14 | 12 of 14 | 2 |
| GreptimeDB | v1.3.1 | 1,834 | 53 | 29 | 26 | 53 | 19 of 20 | 11 |
| Hatchet | v0.120.5 | 2,411 | 99 | 36 | 12 | 99 | 20 of 20 | 7 |
| Memos | 26.11.0 | 742 | 49 | 24 | 4 | 49 | 19 of 20 | 2 |

109 of the 114 judged routes write. The five that do not are these:

- Meilisearch's `POST /indexes/{index_uid}/fields` lists fields with a filter in its body.
- Prometheus's `POST /labels` and `POST /series` are queries sent with POST.
- GreptimeDB's Flight `DoAction` answers `unimplemented`.
- Memos's `Transcribe` transcribes audio and stores nothing.

Every write that the examples' RESULTS.md pages name was in the list:

| project | the writes of its workload | found |
|---|---|---|
| Meilisearch | the addition, the update and the deletion of documents, the filterable attributes, the index swap, the edit by function, the dump, the snapshot | 8 of 8 |
| Iggy | a stream, a topic, a consumer group, sent messages, a stored consumer offset | 5 of 5 |
| Prometheus | remote write, a snapshot | 2 of 2 |
| GreptimeDB | the SQL endpoint, which takes the inserts, the DDL and the ADMIN functions | 1 of 1 |
| Hatchet | a run triggered by the SDK, a scheduled run, a cron, a cancellation, a worker's completion | 5 of 5 |
| Memos | a memo, its update, a comment, a reaction, a share link | 5 of 5 |

Meilisearch's list has 83 routes. 44 of them are the per-setting routes, two for each of 22
settings, which a route table inside `macro_rules!` declares; the scan expands it from the
macro's invocation. Without them the list has 39.

The scan's own mistakes outside the sample:

- A prefix that a router adds in another way is missing: Prometheus's `/api/v1`, which
  `http.StripPrefix` adds, and the `/v1` of GreptimeDB's SQL and dashboard routes, which a
  router built in another function receives.
- Iggy's `poll_messages_committing` commits offsets, but its first word `poll` reads.
- A gRPC method found by name can point at another type's method of that name, as Hatchet's
  `SendStepActionEvent` does.

Garage v2.5.0 (Milestone 349) gave no route: its S3, K2V and admin APIs are enumerations of
endpoints. With Milestone 350's reading of them the scan read 205 files and listed 42 routes in
10 resources, 34 `data` and 8 `admin`, each with its handler. It left out 3 POST operations whose
names read. Every write of the workload's table in Garage's RESULTS.md is there: PutObject,
DeleteObject, DeleteObjects, CopyObject, the four multipart operations, and CreateBucket. So are
the admin API's operations that the workload's first command calls, such as
`ConnectClusterNodes` and `ApplyClusterLayout`. The six projects above gave the same JSON as
before, byte for byte.

### Command tables (Milestone 355)

A Redis-compatible server has no routes: its commands are a table in C or C++, and the Kvrocks
onboarding (Milestone 351) listed its 143 write commands by hand. The scan reads such a table now,
by its shape and not by a project's names. An entry is a call or an initializer whose first
argument is a string that names a command, with an integer argument (the arity) and an argument
that flags it:

- a flags string of flag words only, such as `"write"`, `"write use-memory @string"` or
  `"read-only"` (a command's description, which speaks of write commands, is no flags string);
- flag constants joined with `|`, such as `CMD_WRITE|CMD_DENYOOM` or `CO::WRITE | CO::FAST`.

A file with at least three entries is a table. Each entry flagged as a write is a route with the
method `CMD` and the command's name as its path. Its handler is the class of the call's template
argument (`MakeCmdAttr<CommandSet>`), a function argument named for a command (`setCommand`), or
`.HFUNC(Set)` after the entry. Its resource is the group that the entry names (`@string`,
`COMMAND_GROUP_LIST`), else the stem of the table's file without `cmd_` or `t_`. A generated
`.def` table counts, and a C or C++ file counts among the files read only when it holds a table.

On Kvrocks 2.17.0 the scan reads 21 files and lists 142 write commands in 18 resources, 137 `data`
and 5 `admin` (`FLUSHALL`, `FLUSHDB`, `FLUSHMEMTABLE`, `FLUSHBLOCKCACHE`, `RESTORE`), each with
its handler's class and line. They are every entry of its tables with a `write` flag. The hand
count of 143 also took a line of the Cuckoo filter's commands, which are commented out in
2.17.0. On Valkey 9.1.3, whose table is the generated `src/commands.def`, it lists 118 commands
with `CMD_WRITE` and their functions. The JSON and text reports of Meilisearch, Iggy,
Prometheus, GreptimeDB, Hatchet, Memos and Garage are the same as before, byte for byte.

### What the write scan does not do

- It reads text, so a route that a program builds at run time, or registers through a function
  of its own, is not found.
- A prefix that another file adds through a variable of a module (an express router mounted by
  `app.use("/p", router)` from another file, a FastAPI router included with a prefix) is not
  joined.
- A protocol of a project's own, such as Iggy's binary protocol over TCP, is not read. Neither
  are GraphQL mutations and warp's filters.
- A route whose verb of change is inflected (`poll_messages_committing`) can be taken for a
  read.
- An enumeration of endpoints gives no URL, because the URL comes from the query and the
  headers. A variant that a function of another file chooses, or such a router in another
  language, is not found.
- A command table is read line by line, so an entry that spans lines is not found, and a
  subcommand (`CONFIG SET`) is listed under its own name. A table that registers commands
  through a function call per command, without a flag argument, is not read.

## What is not built

- The candidates file has no JSON schema yet. A change of its format raises its `format:` number.
- `rules-check.json` is not part of the results format.
- `messages` and `history` candidates are not validated by `hydo rules check`.
- The mutation is a change of the stored state after the run. A change of the workload's
  program is not built.

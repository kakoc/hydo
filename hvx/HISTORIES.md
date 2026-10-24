# Operation histories and ready-made checkers

A workload client records every operation it performs as one console line when it starts it
and one when it learns the outcome. After the run the **host** rebuilds the history from the
saved consoles and checks it with a ready-made checker, in the style of Jepsen's Knossos /
Porcupine (linearizability) and Elle (transactional isolation):

| check `type` | what it proves | model |
|---|---|---|
| `linearizable` | the operations can be ordered, consistently with real time, as a single copy of a key-value register (or compare-and-set object) would execute them; checked per key | `register` (read, write) or `cas` (read, write, cas); optional store-assigned versions |
| `list-append` | lists that clients append unique elements to and read whole lose no acknowledged append, never show an element twice or an element nobody appended, agree on one order per key, and (depending on `consistency:`) never let a process's reads go back in time; plus a dependency-graph search for G0 / G1c / G-single / G2 cycles | `append`, `read`, multi-key `txn` |

This replaces hand-written invariant scripts for the properties infrastructure systems must
keep. A hand-written "every acknowledged key is readable" check misses stale reads, reads that
go back in time, and anomalies that need two keys; it also tends to report false violations
for operations whose outcome was unknown. The checkers handle unknown outcomes the way Jepsen
does.

Files: `hvx/history/hydohist.py` (parser + checkers, Python 3.6+, stdlib only),
`hvx/history/hydo-check` (command line), `hvx/history/test_hydohist.py` (unit tests),
`hvx/history/hydo_hist.sh` (shell recorder), `hvx/stack/hydohist` (Go recorder, package
`hydostack/hydohist`), `hvx/onboard/hydo-init-history.sh` (guest init extension).

## Record format (fixed)

```
HYDO-HIST <history> <process> <type> <time> <f> <arg>...
```

- One record is one line, written with a single `write()`. Text before the tag on the same line
  is ignored (the parser searches for `HYDO-HIST `).
- `<history>`: `[A-Za-z0-9_.-]+`, the name a check refers to (`history:`).
- `<process>`: a token (a number in practice). **A process has at most one operation in
  flight**, as in Jepsen: concurrent clients use different process ids. A completion is paired
  with the open invocation of the same process in the same history.
- `<type>`: `invoke` (the request is about to be sent), `ok` (it succeeded), `fail` (it
  definitely had no effect), `info` (the outcome is unknown: a timeout, a lost connection, a
  crash). An invocation that is still open when the same process invokes again, or when the
  history ends (a power cut, a killed client), is treated as `info`.
- `<time>`: guest time in nanoseconds since boot (deterministic under hydo), or `-`. Informational:
  the checkers order events by the **position of the records**, not by time (below).
- `<f> <arg>...`: the operation and its arguments as tokens; layouts per model below. A completion
  repeats the invocation's layout and adds results; `fail` / `info` completions may repeat the
  invocation's arguments or give none.
- **Tokens** are separated by spaces. A value with spaces, `%`, `@`, control characters, or (in
  lists and transactions) `,` `[` `]` `:` is percent-encoded (`%20`, `%25`, `%40`, ...; the
  recorders' `hydo_hist_enc` / `hydohist.Enc` do it). The token `%` alone is the empty string.
  The token `nil` is "absent" (a key that does not exist); a literal value `nil` is written `%6Eil`.

**Register / CAS** (`type: linearizable`):

| operation | invoke | ok |
|---|---|---|
| read | `read <key>...` (keys informational) | `read <key> <value> [<key> <value>...]` — one pair per key read (a range read lists many) |
| write | `write <key> <value>` | `write <key> <value>[@<version>]` |
| compare-and-set | `cas <key> <old> <new>` | `cas <key> <old> <new>[@<version>]` — ok means it swapped |

A value may carry the **version** the store assigned it, as `value@version` (an integer):
etcd's `mod_revision`, a ZooKeeper `version`, a Consul `ModifyIndex`. A write's completion carries
the version the store acknowledged, a read's the version it returned. The model then also
requires that a read returns the version of the write it sees and that later writes of a key get
larger versions. This is how the etcd example catches issue 14685 (a member whose revisions are
off by one returns the right value with the wrong version).

**List-append** (`type: list-append`), elements unique per key:

| operation | invoke | ok |
|---|---|---|
| append | `append <key> <elem>` | `append <key> <elem>` |
| read | `read <key>` | `read <key> [e1,e2,...]` (`[]` = empty or absent) |
| transaction | `txn <mop>...` with `a:<key>:<elem>` and `r:<key>` | `txn <mop>...` with the reads filled in: `r:<key>:[e1,e2]` |

**Declarations and status lines** (printed by the guest init for manifests with `checks:`):

```
HYDO-CHECK {"id":"etcd.kv_linearizable","type":"linearizable","model":"register","initial":"nil","history":"kv","msg":"..."}
HYDO-CHECK-FINAL {"id":"etcd.kv_linearizable","rc":0}
```

`HYDO-CHECK` (at the start of the workload) makes a saved console self-describing: the host takes
the check's spec from it. `HYDO-CHECK-FINAL` is the exit status of the check's `final:` read-back;
non-zero means the history is incomplete, and an otherwise passing check becomes an ERROR.

Example (from the etcd example):

```
HYDO-HIST kv 2 invoke 41230000000 write k77 v77
HYDO-HIST kv 2 ok 41290000000 write k77 v77@83
HYDO-HIST kv 2 invoke 41290000000 read k77
HYDO-HIST kv 2 ok 41330000000 read k77 v77@83
HYDO-HIST kv 21 ok 61020000000 read k0 v0@5 k1 v1@6 ... k89 nil
```

### Ordering, power cuts, determinism

The host orders events by their position in the console, not by their time stamps. Every record
is one `write()` to the same console, so if the completion of A was printed before the invocation
of B, A really completed before B began (the reply was received before A's record was written,
and B's record was written before its request was sent). Records written at nearly the same moment
may appear in either order; the checkers then treat the operations as concurrent, which can hide
a violation but never invents one.

A history survives a power cut because each record reaches the host's console buffer when it is
written. After a cut the recovery boot's records (the `final:` read-back of the recovered state)
are appended: the host concatenates the workload console and the recovery console, in that order.
Guest time restarts in the recovery boot; position does not. Operations in flight at the cut have
no completion and count as unknown outcomes.

Everything is a pure function of the consoles, and the consoles are part of a run's fingerprint:
a reproduced run gets the same history and the same verdict.

The console is 64 KiB per VM (since the snapshot the run starts from). A record is 40–60 bytes, so
a run can record roughly 500 operations next to its other output; a range read of many keys is
one long line. Record short keys and values.

## The checkers

### `linearizable`

Per key (P-compositionality: a history is linearizable iff every key's sub-history is), the
Wing–Gong–Lowe search with Lowe's memoization, as in Porcupine and Knossos. Operations:

- `fail`: removed (it did not take effect).
- `info` reads: removed (their result is unknown). `info` writes and CAS: kept with an infinite
  completion time — they may take effect at any point after they began, or never (taking effect
  after everything else is indistinguishable from never). An unknown-outcome CAS takes effect only
  if the value matches at that point.
- `initial: nil` (default): every key starts absent. `initial: any`: the first observation defines
  it (keys written before the history began).
- A search that exceeds 2,000,000 steps on a key (`--budget`) leaves the key undecided; the check
  is then an ERROR, not a pass.

**Explanation:** for the failing key whose witness begins earliest, the **minimal witness**: a set
of that key's operations that cannot be ordered, where removing any one operation makes it
linearizable. It is found by removing operations one at a time while the set stays
non-linearizable and every value a read or CAS observes is still produced by a write in the set
(otherwise dropping a write would "explain" nothing). Common shapes get a sentence: "an
acknowledged write is lost or a read is stale: a read that began after the write of v5 completed
returned nil", "the write was acknowledged with version 83, a later read returned version 82".

### `list-append`

`consistency:` names the level the system claims (Elle's names):

| level | real-time edges, lost appends, stale reads | per-process order: monotonic reads, read-your-writes | forbidden cycles |
|---|---|---|---|
| `strict-serializable` (default) | yes | yes | G0, G1c, G-single, G2 |
| `strong-session-serializable` | – | yes | G0, G1c, G-single, G2 |
| `serializable` | – | – | G0, G1c, G-single, G2 |
| `strong-snapshot-isolation` | yes | yes | G0, G1c, G-single |
| `strong-session-snapshot-isolation` | – | yes | G0, G1c, G-single |
| `snapshot-isolation` | – | – | G0, G1c, G-single |

Direct checks (always): `duplicate-elements` (a read shows an element twice),
`garbage-read` (an element nobody appended), `aborted-read` (G1a: an element of a failed append),
`intermediate-read` (G1b), `internal` (a transaction does not see its own earlier appends),
`incompatible-order` (two reads of a key, neither a prefix of the other). With real time:
`lost-append` (an acknowledged append is missing from a read that began after it completed — this
is "lost acknowledged write" after a power cut) and `stale-read`. With process order:
`monotonic-read` ("its reads went back in time") and `read-your-writes`.

Cycles: the version order of each key is its longest read; edges ww (consecutive appends in that
order), wr (a read whose last element some transaction appended), rw (a read that misses the next
element), plus real-time edges (transitively reduced) and process edges for the levels that have
them. Strongly connected components are searched for the shortest G0 (ww only), G1c (ww + wr),
G-single (exactly one rw) and, for serializable levels, G2 (at least one rw) cycle; a cycle that
needs real-time or process edges is named `G-single-realtime`, `G0-process`, and so on. An
unknown-outcome append that some read observed counts as committed. Not detected: snapshot-isolation
violations whose cycles have two or more non-adjacent rw edges (G-nonadjacent) — reported only
under the serializable levels — and anomalies that need predicates or deletes.

**Explanation:** each anomaly with the transactions involved (process, micro-ops, outcome, guest
time); a cycle lists every edge with its reason ("T3 read x=[1], missing 2, which T1 appended
next; T1 completed before T2 began; ...").

## Verdicts and where they appear

`run_checks` turns each check into `HYDO-ASSERT` records of type `always` (catalog and one
evaluation; `details` carry `ops`, `keys`, the anomaly or failing key, and the one-line summary in
`out`) and a combined verdict:

- a guest verdict that is already a VIOLATION or ERROR stays (the guest's own invariants come
  first, as in the init);
- else the first failing check, in declaration order: `HYDO-ORACLE: VIOLATION [<id>] <summary>`;
- else an undecided check, or one whose `final:` failed: `HYDO-ORACLE: ERROR check <id> could not
  be evaluated: ...`;
- else `HYDO-ORACLE: OK ... | checks=<n> held (<first id>: <summary>)` (just
  `HYDO-ORACLE: OK checks=<n> held (...)` for a manifest whose only properties are checks).

Where the checks run:

- **`explore.py`** (and everything built on its worker pool: the shrinker, `run_list.py`, branch
  runs): after every run whose console has `HYDO-CHECK` lines, `host_checks()` writes
  `consoles/<tag>.checks.log` (the records; read together with the consoles by the property table
  and the failed-assertion list) and `consoles/<tag>.checks.txt` (the explanation), and replaces the
  run's verdict. Failure classes, shrinking and reports work unchanged; the guest's own verdict is
  kept as `guest_verdict`.
- **`hydo run` / `hydo repro`** (`hvx/dist/hydo`): `one.checks.log`, `one.checks.txt`, the
  combined verdict in `run.json`.
- **By hand:** `hvx/history/hydo-check <outdir>` (an `hvx_explore one` output directory),
  `hydo-check <console> [<recovery console>]`, or `hydo-check --check '<json spec>' <console>` for a
  console without declarations. Exit status 0 OK, 1 violation, 2 error, 3 no checks.

## Recorders

**Shell** (`hvx/history/hydo_hist.sh`; in the prelude of every manifest with `checks:`, so the
workload, `retry`, `final:` and invariant commands can call it):

```sh
hydo_hist kv 0 invoke write k1 v1
if [ "$(redis-cli SET k1 v1)" = OK ]; then hydo_hist kv 0 ok write k1 v1
else hydo_hist kv 0 info write k1 v1; fi          # a timeout: unknown outcome
hydo_hist kv 0 invoke read k1
v=$(redis-cli GET k1) && hydo_hist kv 0 ok read k1 "${v:-nil}" || hydo_hist kv 0 info read k1
hydo_hist kv 0 ok write k1 "$(hydo_hist_enc "$value")@$rev"   # a free-form value with a version
```

Records go to `/dev/console` (`HYDO_HIST_OUT=<file>|off` overrides). The time comes from
`/proc/uptime` (10 ms resolution).

**Go** (`hvx/stack/hydohist`, import `hydostack/hydohist`):

```go
kv := hydohist.New("kv")
op := kv.Invoke(3, "write", "k1", hydohist.Enc(v))
if err == nil { op.Ok("k1", hydohist.Ver(v, rev)) } else if rejected { op.Fail() } else { op.Info() }
op = kv.Invoke(3, "read", "k1")
op.Ok("k1", hydohist.ValOrNil(v, found))
tx := kv.Invoke(4, "txn", hydohist.Append("x", "1"), hydohist.Read("y"))
tx.Ok(hydohist.Append("x", "1"), hydohist.ReadResult("y", "1", "2"))
```

Writes to `/dev/console` (`HYDO_HIST_OUT=<path>|stderr|off`), one `write()` per record, time from
`CLOCK_MONOTONIC`.

**Other languages** (the Rust SDK and any client): print the lines above, one `write()` each,
with the invocation before sending the request and the completion after the reply.

## Manifests

`checks:` in `hydo.yaml` (MANIFEST.md, "History checks"); a manifest may have only checks and no
invariants. Examples: `examples/redis/hydo-hist.yaml` (a register history and a list-append
history on Redis) and `examples/etcd/hydo-hist.yaml` (etcd puts and gets with mod_revisions, read
back through every member).

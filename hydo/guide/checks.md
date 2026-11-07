# Checks in hydo/checks/

The full reference: the sections "Invariants", "Liveness", "History checks", "Reading the stores"
and "Side-effect checks" of `hydo guide manifest`. Shell templates for common properties:
`hydo guide invariant-patterns`.

## Check files

| file name | when it runs | header keys |
|---|---|---|
| `final_<name>` | once at the end of the run (after the heal, the retries and the `eventually_` commands); after a power cut, in the recovery boot | none |
| `always_<name>` | also every `every` seconds of guest time while the workload runs | `# hydo: every=1` |
| `eventually_<name>` | polled after the anchor until it holds or the deadline passes: bounded liveness | `# hydo: within=20 after=heal poll=1` (`after=start` counts from the start of the workload) |
| `linearizable_<name>` | the file is the final read-back; the host checks the recorded history | `# hydo: history=kv model=register` (or `model=cas`, `initial=any`) |
| `list_append_<name>` | the same, for list appends | `# hydo: history=log consistency=strict-serializable` |

Every check file can carry `# hydo-msg: <one line>`, the property's description in reports.

Exit status of a `final_`, `always_` or `eventually_` check: `0` = holds, `1` = violated, `2` =
cannot be evaluated now (the store is down, a read timed out). A check that times out counts as
2; its time limit is `test.command_timeout` (default 60 s; after a power cut a clustered store
may need longer to elect its leaders). The explorer groups failures by the last line with its
numbers blanked, so print lists of ids on an earlier line and keep the last line to counts
("lost 3 of 20 acknowledged messages"): a list of ids in it makes every count its own finding.
The last output line is the explanation; it becomes the details of the property's
`HYDO-ASSERT` record and, for the first violation, of the verdict line
`HYDO-ORACLE: VIOLATION [<id>] <details>`. Start every check by testing that the store answers,
and `exit 2` if not, so a killed service is not reported as a violation. An `eventually_` check
returns 0 when satisfied and anything else for "not yet"; a missed deadline is a violation.

Use `always_` for properties that can break and heal during the run (stale reads, a negative
balance seen for a moment). An `always_` check runs only while the workload lasts, so keep the
period short (1 or 2 s) or rely on `final_` checks.

## Ready-made store readers: `hydo-read` and `hydo-check`

`hydo-read <store> <args>` prints what a store holds as sorted JSON Lines (exit 2 = could not
read). Stores: `mysql <dsn> "<query>"`, `postgres <dsn> "<query>"`, `kafka <bootstrap> <topic>`,
`redis <addr> get|scan|hgetall|smembers|lrange|zrange|cmd ...`, `etcd <endpoint> get|status`,
`hydo acked|failed|told-failed|unknown` (the client's records), `file`, `jsonl`, `range <a> <b>`.

`hydo-check <check> <sources>` compares readings with one explanation line and the check exit
codes, so a check file can be one line:

```sh
#!/bin/sh
# hydo-msg: every acknowledged order is on the Kafka topic exactly once
exec hydo-check exactly-once --unreadable violation --left 'acked=hydo acked' \
     --right 'kafka=kafka 127.0.0.1:9092 orders'
```

Checks: `subset`, `equal`, `exactly-once`, `disjoint` (nothing reported failed was applied),
`no-duplicates`, `count --eq|--min|--max N`, `never-wrong-value --cache C --truth T` (a cache
versus the source of truth; a missing cache key is fine). A source is `name=<hydo-read words>`
plus a projection (`--field F`, `--item 'k:{id}'`, `--key F --value F`, `--strip-prefix P`,
`--add-prefix P`). `--unreadable violation` turns an unreadable store into a violation, for a
final check after recovery where an unreadable store is itself lost data.

hydo stages `/bin/hydo-read` and `/bin/hydo-check` only when a command or check file mentions
them. The arm64 build stages the arm64 binary; whether a release bundles them is in
`hydo guide capabilities`.

## History checks (linearizability, list-append)

For properties every data store must keep, record the operations and let a ready-made checker
decide. In the commands, call the recorder (source `/etc/hydo/prelude.sh` to get it):

```sh
hydo_hist kv $HYDO_CMD_ID invoke write k1 5      # before the request
hydo_hist kv $HYDO_CMD_ID ok write k1 5          # after an acknowledged reply
hydo_hist kv $HYDO_CMD_ID info write k1 5        # no reply: the outcome is unknown
```

Go programs use `hydohist` (module `github.com/kakoc/hydo/sdk/go`), Rust `hydo_assert::history` or `hydo_test::HistoryCheck`,
JavaScript `require("hydo-assert/history")` and Python `from hydo_assert import history`; all write the
same lines.
The check file `checks/linearizable_<name>` reads every key back at the end and records the reads,
so a write lost in a crash shows up. The host checks the history after the run (`linearizable`
for registers and compare-and-set objects per key, Knossos/Porcupine style; `list_append`, Elle
style, at the `consistency:` level the system claims) and names a minimal witness. The record
format and the models: `hydo/HISTORIES.md` in hydo's repository. Example:
`hydo/onboard/examples/redis-composed-hist/` in hydo's repository.

## Side effects at fake outside services

When the system talks to a fake (`hydo guide outside-services`), its record answers "what did the outside
world see?":

```sh
#!/bin/sh
# hydo-msg: no order was charged twice
exec hydo-world check at-most-once payments.charge --by ref
```

Kinds: `at-most-once <ep>.<route> [--by ref|idem]`, `effect-for-acked <ep>.<route> --ref 'o{id}'`
(every acknowledged id produced its effect; as an `eventually_` check: within N seconds),
`exactly-once`, `none-after <ep>.<route> --after event:<name>` (no payment after its
cancellation; the workload adds events with `hydo-world event <name> <ref>`), `count`.

## Assertions inside the program

The services themselves can state properties with an assertion SDK (always, sometimes,
reachable, unreachable, eventually), and both kinds of record mix on the console. See `hydo guide sdks`.

## What not to check

- Properties the system documents as weaker under a relaxed setting. Keep the check; the
  explorer records the knob in the schedule and classifies the finding as an unsafe setting.
- Timing (latency bounds): guest time is virtual and faults stretch it on purpose.
- Anything that depends on operations marked `unknown`: the system may or may not have applied
  them, and either is correct.

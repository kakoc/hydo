# Differential testing between versions (Milestone 298, design and first version)

Status: built, as `hydo compare-versions`. The authority on the output is the Rust type
`hydo_results::CompareDoc` (`compare.json`, a results addition of format version 1) and the
example under `hydo/schema/examples/results/v1/mac-redis-compare/`. This note explains the
decisions and what is left. The code is `hydo/crates/hydo-cli/src/compare.rs`, and the state
dump is `hydo/crates/hydo-control/src/schedule/dump.rs`.

## The question

Before every upgrade, every dependency bump, and every refactor, a team wants to know whether
the new version behaves as the old one did. A test suite answers it only for the cases
someone wrote down. A comparison of two versions answers it for any schedule. Version A and
version B run the same workload with the same faults. Then hydo compares what each run's client
was told and what each system stored.

On ordinary machines such a comparison drowns in noise. Two runs of one version already differ
in timing, in the order of concurrent operations, and in which writes a crash loses. hydo
repeats a run exactly: one image and one schedule give one run. So two runs of one schedule on
two versions differ only where the versions differ. Every difference is a fact about the
change. hydo answered this question for itself in Milestones 101 to 107. Each port of the Python
product to Rust had to give the same fingerprints and the same console formats.

## The command

```sh
hydo compare-versions "disk=none | run" "disk=cut | run C=12" --param fsync=always,everysec
hydo compare-versions --regressions --git v2.3.24,v2.4.0 --build 'sh build.sh'
hydo compare-versions --seeds 1-20 --images old=a.cpio.gz,new=b.cpio.gz --strategy process
```

- The two versions come as `hydo versions` makes them (Milestone 221), A first. A version is a
  manifest parameter's value, a git revision with a build command, or a ready image. The image
  cache of `hydo versions` keeps each image by its inputs.
- The schedules are given, or they are the saved regressions beside version A's image, or they
  are drawn from seeds. A drawn schedule comes from `hydo explore`'s own generator
  (`hydo_explore::Drawer`), from version A's fault space, with the explorer's drawing options.
- Each run is a child `hydo run`, as in `hydo versions`. A version that does not build, or a
  schedule that a version refuses, ends that schedule's comparison with a reason.

The exit status is 0 when the versions agree on every schedule, 1 when a schedule's runs differ,
and 2 when a side could not run. A CI job can gate an upgrade on it.

## What is compared

Each level is optional (`--levels`), and the document names the levels it compared. Each level
is a set of streams. A stream is a sequence of records of one kind, in one conversation (the run,
or the recovery boot after a power cut).

| Level | Stream | Where it comes from |
|---|---|---|
| `verdicts` | `verdict` | the verdict's kind and the check it names (`violation redis.acked_writes_survive`) |
| `verdicts` | `findings` | each finding of `result.json`: kind, id, and status, sorted |
| `observations` | `outcomes` | the lines of `$HYDO_OUTCOMES`: the controller's reads of the file, in the control transcript, matched with the agent's replies in the agent log |
| `observations` | `acked`, `failed` | the controller's lists, `hydo-explore-acked:` and `hydo-explore-failed:` |
| `observations` | `history <name> <process>` | each `HYDO-HIST` record of the history and messages checks, without its guest time |
| `state` | `state <service>` | each line of the service's `compare:` dump (`HYDO-STATE` notes) |
| `state` | `check <id>` | what each check printed: the `HYDO-ASSERT` record's output, and the `HYDO-CHECK-OUT` lines |

These choices keep out what a version change moves for no reason of behaviour:

- A `HYDO-HIST` record's guest time is left out. Other binaries run at other speeds, so every
  time moves with any change.
- The history records are compared per history and process. Two processes that interleave
  otherwise still compare equal when each process saw the same answers.
- `outcomes`, `acked` and `failed` are compared as sets of lines, sorted by operation id. Two
  versions that finish concurrent operations in another order agree.
- Every other stream keeps its order, and the two sequences are aligned as `hydo diff` aligns
  events (Myers' algorithm, `diff::align`). A key that only one version stored shows as that
  key, not as every line after it.
- Fingerprints are not compared: two images give two fingerprints. The report says when the two
  runs also have the same fingerprint, which holds for two builds with the same bytes.

The stored state needs the system's help. A service's `compare:` command prints it, for example
a SQL query or a scan of every key (MANIFEST.md, "State for comparisons"). The host controller
runs it after the verdict, in the boot that gives the verdict, and sends each line as a console
note. A note is outside the control transcript's hash. The command runs after the checks, so it
changes no verdict. Without the key nothing changes, and every recorded value stays.

## Normalization

A field that may differ between versions makes every comparison differ: a generated id, a time
stamp, a version string. The rules blank such fields, and the rest must then be equal:

- `--ignore 'REGEX'` replaces each match by `<ignored>`; `--ignore 'REGEX=>TEXT'` by `TEXT`.
- `--ignore-json PATH` sets the value at a dotted path (`*` is any key or index) of a record
  that is JSON after its first `{` or `[`.
- `hydo/compare.yaml`, read when it exists, or `--normalize FILE`, holds the same rules:

```yaml
levels: [observations, state]
ignore:
  - {regex: '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', as: '<uuid>'}
  - {json: items.*.created_at}
```

The JSON rules apply first, then the regular expressions. The document lists the rules it used.

## The report

For each schedule the report says whether the two versions agree. When they do not, it gives the
first difference in the run's order: the client's observations by workload step, then the
stored state, then the verdicts. It names the stream, the workload steps, what A had, and what B
had. It lists the first difference of every other stream that differs, with how many records
only A and only B have. It gives the `hydo run` command that runs the schedule again on each
version's image. `hydo report DIR` prints the same text, and `hydo report --html DIR` writes a
page with one card per schedule and a folded table of every difference.

## The search

`--seeds A-B` draws one schedule per seed and runs it on both versions. A schedule whose runs
differ is shrunk as the explorer shrinks a failure. The candidates are the explorer's own
(`Ex::candidates`), simplest first. A candidate is kept when its runs still differ in the same
level and stream as the original's first difference. The shrink stops at a round with no kept
candidate or after `--shrink-budget` candidates (20 by default), each run on both versions. The
document keeps each drawn schedule and each minimal schedule with the schedule it was shrunk
from.

`--again DIR` reads the runs of an earlier comparison once more with the rules and levels given
now. A new rule is tried without a VM.

## The proof on the Mac engine

All runs used this tree's programs in the 0.3.4 Mac release, one comparison at a time under the
shared VM lock. `docs/HISTORY.md`, Milestone 298, has the commands and the fingerprints.

| Pair | Schedules | Answer |
|---|---|---|
| Redis 7.2.4, `appendfsync always` against `everysec` (a parameter) | `disk=none \| run`, `disk=cut \| run C=12` | the first agrees (126 records); the cut differs: `always` kept the 12 keys, `everysec` lost all of them |
| the same | seeds 1 to 10, shrunk | 4 of 10 differ; minimal: `disk=ioerr:1 \| run`, `disk=lying:1 \| run C=1`, and a kill with packet loss |
| two builds of one Redis manifest (`--images`), the same bytes | three schedules | no difference, the same fingerprints |
| Redis `hz 1` against `hz 10` | four schedules, with a cut, a kill and a disk error | no difference, other fingerprints |
| the same | seeds 1 to 10 | 9 agree; under 14 % packet loss one request ends `unknown` on one side and `ack` on the other |
| Formance Ledger v2.3.24 against v2.4.0 (two tags, built from source) | `disk=none \| run` | differs: only v2.4.0 logs the account race of its `UpsertAccounts` rewrite |

Three of the answers say something a test suite would not:

- With `appendfsync always`, one failed disk write stops Redis, and hydo cannot start it again.
  With `everysec`, the same schedule acknowledges all 20 writes. A team that changes the setting
  for speed also changes what a bad disk does.
- `hz` changes no answer on the four schedules. Under packet loss it moves which request runs
  past the client's 2 s deadline. A comparison of a timing change should leave out network
  faults, or blank the outcome words.
- Formance's v2.4.0 runs into the race of pull request 1064 (`feat: introduce schemas`) in a run
  without faults: PostgreSQL logs `duplicate key value violates unique constraint
  'accounts_ledger'`, and the ledger logs `upserting accounts`. The client's retry with its
  idempotency key hides it, so every check holds on both versions. The comparison shows it as a
  finding that only B has. The same run shows a weak point of the workload: its reverts choose
  a transaction by the order of earlier answers. Two operations finished in the other order,
  and one revert chose another transaction.

## What is left

Open item 149 lists what the first version leaves out.

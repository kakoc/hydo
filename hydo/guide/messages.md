# Messaging checks (queues, logs, publish-subscribe)

Use a `type: messages` check for a system that passes messages: a queue, a log or stream, a
publish-subscribe bus, or an event bus. The project declares what its system promises, its
client records what it did, and hydo checks exactly the declared claims and measures the history.
hydo knows no messaging system or API. The design and its reasons are
`docs/design/messaging.md` in hydo's repository.

## Declare the check

Add the check to `checks:` in `hydo.yaml`. Every key but `id`, `type` and `history` is optional,
and every default claims nothing.

```yaml
checks:
  - id: jobs
    type: messages
    history: jobs                    # the history the client records
    final: /bin/jobs-client drain    # the drain: it reads every message at the end
    acknowledgement: explicit        # explicit | on-receive | none
    retention_ms: 604800000          # a message older than this may be gone (an excuse)
    clock_steps: undecided           # undecided | strict
    claims:
      delivery: at-least-once        # none | at-most-once | at-least-once | exactly-once
      duplicates: forbidden          # allowed | forbidden | {by: dedup-id | message, window_ms: <n>}
      after_ack: no-redelivery       # allowed | no-redelivery
      processing: once               # any | once
      lease: {ms: 30000}             # none | {ms: <n>} | {ms: per-delivery}
      order: {kind: fifo, scope: key, batch: ordered}   # kind: none | fifo | fifo-blocking | log | causal
      dead_letter: {after: 5, to: jobs-dead}            # none | {after: <n>[, to: <channel>]}
      delay: per-message             # none | per-message | {ms: <n>}
      fan_out: every-group           # none | every-group
      members: exclusive             # any | exclusive
      linearizable: none             # none | fifo | unordered (needs acknowledgement: on-receive)
    measures:
      quantiles: [0.5, 0.95, 0.99, 1]
      channels: all                  # all | [<channel>, ...]
```

`hydo build --check` refuses a declaration whose claims contradict its facts, and names both keys:

- `linearizable` without `acknowledgement: on-receive`;
- `processing`, `after_ack`, `lease`, `dead_letter` or `order: fifo-blocking` without
  acknowledgements;
- `delivery: at-most-once` or `exactly-once` together with a `lease`;
- `delivery` with loss, `fan_out`, `processing`, or `dead_letter` with `to:`, without a `final:`.

Unknown keys are errors, so a misspelled claim stops the build.

## Share declarations: `include:`

A bundle is a YAML file of the project with the keys of a declaration. It cannot have `id`,
`type`, `history`, `final` or `include`. `include:` merges the bundles in order, then the
entry's own keys. A later key replaces an earlier one whole. Inside `claims:` and `measures:`
the rule applies to each key.

```yaml
    include: [bundles/work-queue-with-leases.yaml]
    claims:
      lease: {ms: 60000}             # replaces the bundle's lease
```

`hydo build` resolves the bundles, and the image carries the result. A bundle changed later does
not change what an existing image claims. `hydo build --check` prints the resolved declaration of
each messages check. hydo ships no bundle. These four describe common shapes; copy one into the
project and change it.

`work-queue-with-leases.yaml`:

```yaml
claims:
  delivery: at-least-once
  lease: {ms: 30000}
  after_ack: no-redelivery
  processing: once
```

`ordered-groups.yaml`:

```yaml
claims:
  delivery: at-least-once
  order: {kind: fifo-blocking, scope: key, batch: ordered}
  lease: {ms: per-delivery}
  after_ack: no-redelivery
```

`keyed-log.yaml`:

```yaml
retention_ms: 604800000
claims:
  delivery: at-least-once
  duplicates: {by: message, window_ms: 0}
  order: {kind: log, scope: key}
```

`fan-out.yaml`:

```yaml
claims:
  delivery: at-least-once
  fan_out: every-group
```

## Record the history

The client writes one `HYDO-HIST` record before each request and one after the reply, as for the
other history checks (`hydo guide checks`). A message id `m` is unique per sending intent. A
retry of the same intent reuses the id.

| operation | invocation | completion |
|---|---|---|
| send | `send <channel> <item>...` | the items, with the system id `o=` and, in a batch, `s=` |
| receive | `recv <channel> <group>` | `recv <channel> <group> <item>...` (none: an empty receive) |
| acknowledge | `ack <channel> <group> <item>...` | the same |
| give back | `nack <channel> <group> <item>...` (`;r=0`: reject) | the same |
| extend a lease | `extend <channel> <group> <item;v=<ms>>...` | the same |
| commit offsets | `commit <channel> <group> <key>@<offset>...` | the same |
| subscribe | `sub <channel> <group> from=start\|from=now` | the same |
| end of the drain | (none) | `drained <channel> <group>` |

An item is the message id with attributes, in this order: `m17;k=g2;x=d17;d=5000;o=81;n=2;h=a91f;v=30000;s=ok`.

- `k`: the ordering key. `x`: the deduplication id. `d`: the requested delay in ms.
- `o`: the system id or offset. `n`: the attempt count. `h`: the handle an acknowledgement presents.
- `v`: the lease in ms. `s`: the item's outcome in a batch. `r=0`: a reject on a nack.

The rules for the values:

- `s` is `ok` (the item took effect), `fail` (it certainly took no effect) or `info` (unknown).
  A batch whose request got no answer is `info` for every item; write the operation's type as
  `info` then.
- `o` may be any text. Two sends with the same `o` are one stored copy. Some claims need whole
  numbers: `order: log` and the gap in a lost message's explanation read `o` as an offset. A
  system id that is not a number works for every other claim.
- `d`, `n` and `v` are whole numbers.
- A message id, `h`, `k`, `x` and `o` may hold any characters, encoded as in the other history
  records. Encode these characters: `%`, `@`, a space, a control character, `,`, `[`, `]`, `:`,
  `;` and `=`. Each becomes `%` and two hexadecimal digits per byte of UTF-8 (`a;b` is `a%3Bb`). Write an empty value as
  `%`. The Rust SDK's recorder encodes the values itself.

A send that the system answers with "this deduplication id already exists":

- a retry of the same intent (the same message id): record `ok`, with the stored message's
  system id in `o=` when the answer carries it. hydo then counts one stored copy, delivered
  once. Do not record `fail`: the message is in the system.
- another message with the same `x=`: the system stored nothing of its own for it, so record
  `fail`. With `duplicates: {by: dedup-id}` that is the promise kept.

The group `-` is the channel's one group, and it exists from the start. A named group must
receive the messages sent after its first completed `sub` or `recv`, or all of them after
`sub ... from=start`. A process has one operation in flight, so concurrent consumers use
different process numbers.

The Rust SDK writes these records with the cargo feature `messages` of `hydo-assert`, which is
new in 0.3.1 (`features = ["messages"]` on the dependency):

```rust
use hydo_assert::history::messages::{Item, Messages};
let q = Messages::new("jobs");
let op = q.send(3, "jobs", &[Item::new("m17")]);
op.ok(&[Item::new("m17").id(81)]);              // or op.fail(), op.info()
q.recv(5, "jobs", "-").ok(&[Item::new("m17").handle("a91f").lease_ms(30000)]);
q.ack(5, "jobs", "-", &[Item::new("m17").handle("a91f")]).ok_same();
q.drained(9, "jobs", "-");
```

Clients in other languages write the same lines with their history recorder's generic records.

## Write the drain

The drain is the check's `final:` command. hydo runs it after the heal, and in the recovery boot
after a power cut. It receives from every group until the group is empty and acknowledges what it
receives. Then it writes `drained <channel> <group>`. A claim that reads the end of the history
without a drained group is undecided, and the run's verdict is an ERROR.

A drain that stops too early reports acknowledged messages as lost, or writes no `drained`
record. Write it to these three rules:

1. Outlast every lease and visibility timeout. A message that a consumer received and did not
   acknowledge before a fault comes back only when its lease or visibility timeout runs out.
   The quiet window is the time of empty receives after which the drain calls a group empty.
   Make it longer than the longest lease and visibility timeout of the system.
2. Outlast the system's recovery after a power cut. In the recovery boot the system can refuse
   or redirect requests until a takeover lease runs out or a leader is elected. Treat an error,
   a refusal and a redirect as "not yet": wait a second and ask again until the deadline. Count
   the quiet window again from each error, so that recovery time does not count as quiet.
3. Fit inside the drain's time limit. hydo runs the drain under `workload.timeout` (default 300
   seconds) and kills it after that; the verdict is then an ERROR that says its `final:` read-back
   exited with a non-zero status. Set the drain's own deadline below `workload.timeout`, and raise
   `workload.timeout` in `hydo.yaml` when the deadline needs more time.

`test.command_timeout` limits the composer's commands and the check files in `checks/`, not the
drain. `test.drain_timeout` is the composer's wait for running commands after the last round,
also not this drain. Measure the drain's own deadline and quiet window on a clock that a clock
fault does not move: `/proc/uptime` in a shell, `std::time::Instant` in Rust. A `T=` step moves
`date`.

Here is a drain in shell for one channel and its group. `myq` stands for the system's own client:
`myq receive` prints `<message id> <handle>` or nothing, and exits non-zero on an error. The
message id is the one the sending command recorded, carried in the message body.

```sh
#!/bin/sh
# drain <channel> <group>: the final: command of a messages check with history: jobs
. /etc/hydo/prelude.sh                        # hydo_hist
ch=$1 grp=$2 p=100000                         # p: a process number no other command uses
deadline=240                                  # seconds: below workload.timeout (300 by default)
quiet=40                                      # seconds: above the longest lease and recovery
now() { read -r up rest < /proc/uptime; printf '%s\n' "${up%.*}"; }   # T= does not move it
end=$(( $(now) + deadline )); last=$(now)
while [ "$(now)" -lt "$end" ]; do
  hydo_hist jobs $p invoke recv "$ch" "$grp"
  if ! out=$(myq receive "$ch" "$grp"); then  # an error, a refusal or a redirect: not yet
    hydo_hist jobs $p info recv "$ch" "$grp"  # unknown: the system can have handed one out
    last=$(now); sleep 1; continue
  fi
  if [ -z "$out" ]; then                      # an empty receive
    hydo_hist jobs $p ok recv "$ch" "$grp"
    if [ $(( $(now) - last )) -ge "$quiet" ]; then
      hydo_hist jobs $p ok drained "$ch" "$grp"
      exit 0
    fi
    sleep 1; continue
  fi
  set -- $out                                 # $1: the message id, $2: the handle
  hydo_hist jobs $p ok recv "$ch" "$grp" "$1;h=$2"
  hydo_hist jobs $p invoke ack "$ch" "$grp" "$1;h=$2"
  if myq ack "$ch" "$grp" "$2"; then hydo_hist jobs $p ok ack "$ch" "$grp" "$1;h=$2"
  else hydo_hist jobs $p info ack "$ch" "$grp" "$1;h=$2"; fi
  last=$(now)
done
printf 'no quiet window of %s s on %s %s within %s s\n' "$quiet" "$ch" "$grp" "$deadline"
exit 1
```

The sketch writes values as they are. Encode a message id or a handle with other characters as
the section "Record the history" says. A Rust drain writes the same records with `q.recv`, `q.ack` and
`q.drained`, as in the Rust example of that section.

## Clock steps and durations on the wall clock

Some systems stamp leases, visibility timeouts or delays with the wall clock, the clock that
`date` shows. A `T=` clock step back by an hour then hides every received message for an hour.
The drain finds none of them, and the check reports them lost (`no-loss`). A step forward ends
leases early, and messages come back before their time. NTP and operators step a real
machine's clock too, so such a failure is a bug of the system, not of the test. hydo found it
in Valkey's wait before a replica's full sync (valkey-io/valkey#5406). The fix in the system is
to measure durations on a monotonic clock.

`clock_steps:` decides only the pairs of records that a lease, a delay, or a window judges. With
`undecided` (the default) hydo counts such a pair across a step as undecided; with `strict` it
judges the pair. It never excuses a message that the drain did not receive: a loss after a step
is a violation with either value. When an exploration finds such a failure:

- Keep it as a finding. While a fix is pending, `hydo ci accept <key> --reason ...` marks it as
  accepted, so CI runs pass and the finding stays listed (`hydo guide ci`).
- Keep `clock` in `faults:`. If you remove it, write the reason in a comment beside `faults:`,
  so that a reader knows the plan no longer tests clock steps.

## What the run reports

Each claim is a property of its own, `<check id>.<claim>`. A violated claim names a small witness:
the records that together prove it. Example from `cargo test -p hydo-explore --test it messages_results::`:

```text
HYDO-ORACLE: VIOLATION [jobs.no-loss] m3 was acknowledged to process 3 at 1.012 s and never delivered in channel jobs, which was drained
```

Every messages check also claims `no-phantom`: every delivered message was sent before it was
delivered. A claim is undecided when its records are missing, for example `duplicates` without
system ids (`o=`) or `order: log` without offsets. Pairs that a lease, a delay, or a window cannot
judge are counted, not judged: a `T=` clock step inside the pair's interval, or records of two
boots.

`result.json` has each check under `messages`: the declaration with every default, each claim's
status with its judged and undecided counts, and the measures. `hydo report <run dir>` prints
them. The measures are the sends and their outcomes, the lost messages per group, the excused
ones, the duplicate rate, redeliveries by cause, reorders, latencies, and the availability of
each operation.

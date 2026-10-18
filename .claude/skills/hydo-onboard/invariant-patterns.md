# Invariant check patterns

Every check is a shell command run inside the guest with the manifest's `env` and `shell`
helpers. Exit 0 = holds, 1 = violated, 2 = cannot be evaluated now. Print one line of
details last; it becomes the assertion's details and, for the first violation, the verdict.
Files: `$HYDO_ACKED` (one acknowledged step id per line), `$HYDO_OUTCOMES`
(`<id> ack|failed|unknown`, last line per id wins), `$HYDO_FAILED` (ids whose last outcome
was `failed`).

When a check reads over HTTP, test the status code first: an error page or a JSON error body
is not data, and treating it as data turns a down service into a false violation.

Start every check by testing that the store answers, and `exit 2` if not, so a killed
service is not reported as a violation.

## Acknowledged writes are durable (key-value)

```sh
/bin/redis-cli ping 2>/dev/null | grep -q PONG || exit 2
for i in $(cat $HYDO_ACKED); do
  [ "$(/bin/redis-cli GET k:$i)" = "v$i" ] || { echo "acked key k:$i lost"; exit 1; }
done
echo "acked=$(wc -l < $HYDO_ACKED) all present"
```

## Acknowledged rows exist exactly once (SQL)

```sh
SQL -e "SELECT 1" >/dev/null 2>&1 || exit 2
for i in $(cat $HYDO_ACKED); do
  n=$(SQL -e "SELECT COUNT(*) FROM app.orders WHERE id=$i")
  [ "$n" = 1 ] || { echo "order $i present $n times"; exit 1; }
done
echo "acked=$(wc -l < $HYDO_ACKED) exactly once"
```

## Told failed, so not applied

```sh
SQL -e "SELECT 1" >/dev/null 2>&1 || exit 2
for i in $(cat $HYDO_FAILED); do
  n=$(SQL -e "SELECT COUNT(*) FROM app.orders WHERE id=$i")
  [ "$n" = 0 ] || { echo "order $i was reported failed but is stored"; exit 1; }
done
echo "failed=$(wc -l < $HYDO_FAILED) none applied"
```

## Domain rule as a query that must return 0 (periodic and final)

```sh
n=$(SQL -e "SELECT COUNT(*) FROM app.accounts WHERE balance < 0" 2>/dev/null) || exit 2
echo "negative balances: $n"; [ "$n" = 0 ]
```

Use the same shape for conservation (`SELECT SUM(balance) …` equals the starting total),
orphans (`… LEFT JOIN … WHERE parent.id IS NULL`), and uniqueness (`GROUP BY k HAVING
COUNT(*) > 1`). Mark these `every: 1` as well as `final: true`, with
`type: always_or_unreachable`.

## Monotonic reads (periodic)

Keep the last value seen in a file under `/tmp`; it must never go down.

```sh
v=$(curl -sf http://127.0.0.1:8080/version 2>/dev/null) || exit 2
last=$(cat /tmp/hydo/last_version 2>/dev/null || echo 0)
[ "$v" -ge "$last" ] || { echo "version went back from $last to $v"; exit 1; }
echo $v > /tmp/hydo/last_version; echo "version $v"
```

## A cache never disagrees with the store

```sh
/bin/redis-cli ping 2>/dev/null | grep -q PONG || exit 2
for i in $(seq 0 $((HYDO_STEPS - 1))); do
  c=$(/bin/redis-cli GET item:$i)
  [ -z "$c" ] && continue                       # a miss is allowed
  s=$(SQL -e "SELECT value FROM app.items WHERE id=$i")
  [ "$c" = "$s" ] || { echo "item:$i cache=$c store=$s"; exit 1; }
done
echo "cache consistent"
```

## A message log holds each acknowledged message exactly once

Use the project's own consumer or a small checker binary that reads the whole topic from the
beginning and prints `HYDO-ORACLE: OK …` or `HYDO-ORACLE: VIOLATION …`; the stack example's
`stack.exactly_once` invariant shows how to wrap such a checker.

## What not to check

- Properties the system documents as weaker under a relaxed setting (e.g. "may lose up to
  one second of writes with fsync every second"). Keep the invariant; the explorer records
  the knob in the schedule, and the finding is classified as an unsafe setting.
- Timing (latency bounds): guest time is virtual and faults stretch it on purpose.
- Anything that depends on outcomes marked `unknown`: the system may or may not have applied
  those, and either is correct.

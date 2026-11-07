# Check patterns

Each pattern is a check file in `hydo/checks/` (or the `cmd:` of an `invariants:` entry in a
numbered-steps manifest; the body is the same). Exit 0 = holds, 1 = violated, 2 = cannot be
evaluated now. Print one line of details last. Files: `$HYDO_ACKED` (one acknowledged id per
line), `$HYDO_OUTCOMES` (`<id> ack|failed|unknown`, the last line per id wins), `$HYDO_FAILED`
(ids whose last outcome was `failed`). After a power cut the recovery boot's checks get
`$HYDO_ACKED` and `$HYDO_FAILED` carried over, and an empty `$HYDO_OUTCOMES`: a check that
looks up only the ids listed in `$HYDO_OUTCOMES` checks nothing there. Where a ready-made `hydo-check` line exists it is shown
first; use the shell version where `hydo-read` is not available (`hydo guide capabilities`) or the store is
not one it reads.

Start every check by testing that the store answers and `exit 2` if not, so a killed service is
not reported as a violation. When a check reads over HTTP, test the status code first: an error
page or a JSON error body is not data.

## Acknowledged writes are durable (key-value) — `final_acked_writes_survive`

```sh
#!/bin/sh
# hydo-msg: every acknowledged SET reads back with its value
exec hydo-check subset --unreadable violation \
     --left 'acked=hydo acked --item "k:{id}=v{id}"' \
     --right 'redis=redis 127.0.0.1:6379 scan "k:*" --item "{key}={value}" --timeout 5'
```

```sh
#!/bin/sh
# hydo-msg: every acknowledged SET reads back with its value
/bin/redis-cli ping 2>/dev/null | grep -q PONG || exit 2
for i in $(cat $HYDO_ACKED); do
  [ "$(/bin/redis-cli GET k:$i)" = "v$i" ] || { echo "acked key k:$i lost"; exit 1; }
done
echo "acked=$(wc -l < $HYDO_ACKED) all present"
```

## Acknowledged rows exist exactly once (SQL) — `final_orders_exactly_once`

```sh
#!/bin/sh
exec hydo-check exactly-once --unreadable violation --left 'acked=hydo acked' \
     --right 'db=mysql root:@tcp(127.0.0.1:3306)/app "SELECT id FROM orders"'
```

```sh
#!/bin/sh
SQL() { mysql -h127.0.0.1 -uroot -N -B "$@"; }
SQL -e "SELECT 1" >/dev/null 2>&1 || exit 2
for i in $(cat $HYDO_ACKED); do
  n=$(SQL -e "SELECT COUNT(*) FROM app.orders WHERE id=$i")
  [ "$n" = 1 ] || { echo "order $i present $n times"; exit 1; }
done
echo "acked=$(wc -l < $HYDO_ACKED) exactly once"
```

## Told failed, so not applied — `final_failed_not_applied`

```sh
#!/bin/sh
exec hydo-check disjoint --left 'told_failed=hydo told-failed' \
     --right 'db=mysql root:@tcp(127.0.0.1:3306)/app "SELECT id FROM orders"'
```

## Domain rule as a query that must return 0 — `always_no_negative_balance`

```sh
#!/bin/sh
# hydo: every=1
# hydo-msg: no account balance is ever negative
n=$(mysql -h127.0.0.1 -uroot -N -B -e "SELECT COUNT(*) FROM app.accounts WHERE balance < 0" 2>/dev/null) || exit 2
echo "negative balances: $n"; [ "$n" = 0 ]
```

Use the same shape for conservation (`SELECT SUM(balance) ...` equals the starting total),
orphans (`... LEFT JOIN ... WHERE parent.id IS NULL`) and uniqueness (`GROUP BY k HAVING
COUNT(*) > 1`). As `always_` checks they also run at the end. A shell example of conservation:
`hydo guide examples/example-redis-composed/hydo/checks/final_total_conserved`.

## Monotonic reads — `always_version_never_goes_back`

Keep the last value seen in a file under `/tmp`; it must never go down.

```sh
#!/bin/sh
# hydo: every=1
v=$(curl -sf http://127.0.0.1:8080/version 2>/dev/null) || exit 2
last=$(cat /tmp/hydo/last_version 2>/dev/null || echo 0)
[ "$v" -ge "$last" ] || { echo "version went back from $last to $v"; exit 1; }
echo $v > /tmp/hydo/last_version; echo "version $v"
```

For a store with versions or revisions, a history check (`linearizable_<name>`) is stronger:
`hydo guide checks`, "History checks".

## A cache never disagrees with the store — `final_cache_never_wrong`

```sh
#!/bin/sh
exec hydo-check never-wrong-value \
     --cache 'redis=redis 127.0.0.1:6379 scan "item:*" --strip-prefix item:' \
     --truth 'db=mysql root:@tcp(127.0.0.1:3306)/app "SELECT id, value FROM items" --key id --value value'
```

A missing cache entry is allowed; a cache entry the store does not have, or with another value,
is a violation.

## A message log holds each acknowledged message exactly once — `final_on_topic_once`

```sh
#!/bin/sh
exec hydo-check exactly-once --unreadable violation --left 'acked=hydo acked' \
     --right 'kafka=kafka 127.0.0.1:9092 orders'
```

## An outside effect happened at most once / for every acknowledged request

```sh
#!/bin/sh
# hydo-msg: no order was charged twice
exec hydo-world check at-most-once payments.charge --by ref
```

```sh
#!/bin/sh
# hydo: within=20
# hydo-msg: every acknowledged order produced its receipt email within 20 s
exec hydo-world check effect-for-acked mail.send --ref 'o{id}'
```

## The service answers again after the faults — `eventually_answers`

```sh
#!/bin/sh
# hydo: within=15
# hydo-msg: Redis answers PING within 15 s after the faults are healed
/bin/redis-cli ping 2>/dev/null | grep -q PONG
```

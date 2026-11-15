#!/bin/bash
# One probe of finding 1 without hydo, on the stack of ../docker-compose.yaml (sh stack.sh up):
# schedule N runs of the task `sched` for one instant through Hatchet's API, stop one engine just
# after that instant, start it again, wait, and count the scheduled runs that did not run.
#
#   bash probe.sh <how> <engine> [N] [AFTER]
#     how      kill (SIGKILL, a crash), term (SIGTERM, a clean stop) or none (the control);
#              alone-kill and alone-term first stop the other engine, so that the engine that
#              stops is the only one that reads the queue, and start the other one again at the end
#     engine   hatchet-engine-1 or hatchet-engine-2
#     N        how many runs to schedule (default 150)
#     AFTER    seconds after the instant at which the engine stops (default 0.5)
#
# It prints one line:
#   tag        the probe's tag: its runs are s<tag>_0 ... s<tag>_<N-1> (verify.sh looks again)
#   ran        the scheduled runs that completed within 120 s
#   stuck      the scheduled runs whose trigger message is left in the state ASSIGNED in
#              Hatchet's table MessageQueueItem: an engine read it and never acknowledged it
#   late       the other runs that had not completed within 120 s
set -u
cd "$(dirname "$0")/.." || exit 2
how=$1 eng=$2 n=${3:-150} after=${4:-0.5}
P=${HQ_PROJECT:-hqnative}
tag=$(( $(date +%s) % 100000 ))
out=$(mktemp)
q() { docker compose -p "$P" exec -T postgres psql -U hatchet -d "$1" -tAc "$2"; }
# both engines run before the probe starts
docker compose -p "$P" up -d hatchet-engine-1 hatchet-engine-2 > /dev/null 2>&1
sleep 15
other=hatchet-engine-1; [ "$eng" = hatchet-engine-1 ] && other=hatchet-engine-2
case "$how" in alone-*) docker stop -t 30 "$P-$other-1" > /dev/null; how=${how#alone-}; alone=1 ;; *) alone=0 ;; esac
# the runs are due AT_MS (default 20,000) ms after the first one is scheduled; the API takes a
# few seconds for 150 runs (give 600 runs AT_MS=45000)
timeout 300 docker compose -p "$P" run --rm --no-deps -e HYDO_HIST_OUT=off -e HQ_BURST="$n" -e HQ_AT_MS="${AT_MS:-20000}" \
  -e HYDO_CMD_ID="$tag" worker-2 op schedmany > "$out" 2>&1
acked=$(grep -c '^scheduled ' "$out")
at=$(grep -m1 '^scheduled ' "$out" | awk '{print $4}')
due=$(python3 -I -c 'import datetime, sys, time
t = datetime.datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00")).timestamp()
print(max(0.0, t + float(sys.argv[2]) - time.time()))' "$at" "$after")
sleep "$due"
case "$how" in
  kill) docker kill -s KILL "$P-$eng-1" > /dev/null ;;
  term) docker stop -t 30 "$P-$eng-1" > /dev/null ;;
  none) ;;
  *) echo "probe.sh: how is kill, term, none, alone-kill or alone-term" >&2; exit 2 ;;
esac
[ "$how" = none ] || { sleep 10; docker compose -p "$P" up -d "$eng" > /dev/null 2>&1; }
[ "$alone" = 0 ] || docker compose -p "$P" up -d "$other" > /dev/null 2>&1
# wait up to 120 s for every acknowledged scheduled run to complete
for _ in $(seq 1 40); do
  sleep 3
  ran=$(q app "SELECT count(DISTINCT m) FROM app_run WHERE m LIKE 's${tag}\_%' AND outcome = 'ok'")
  [ "$ran" -ge "$acked" ] && break
done
grep '^scheduled ' "$out" | awk '{print $2}' | sort > "$out.acked"
q app "SELECT DISTINCT m FROM app_run WHERE m LIKE 's${tag}\_%' AND outcome = 'ok'" | sort > "$out.ran"
# the runs named by trigger messages that an engine read and never acknowledged
q hatchet "SELECT DISTINCT convert_from(decode(convert_from(decode(e, 'base64'), 'UTF8')::jsonb->>'data', 'base64'), 'UTF8')::jsonb->>'m'
  FROM \"MessageQueueItem\" i, jsonb_array_elements_text(i.payload->'messages') e
  WHERE i.status = 'ASSIGNED' AND i.payload->>'id' = 'task-trigger'" | grep "^s${tag}_" | sort > "$out.stuck"
missing=$(comm -23 "$out.acked" "$out.ran")
stuck=$(printf '%s\n' "$missing" | grep -c -x -F -f "$out.stuck")
late=$(printf '%s\n' "$missing" | grep -v -x -F -f "$out.stuck" | grep -c .)
[ "$alone" = 0 ] || how="alone-$how"
echo "tag=$tag how=$how engine=$eng after=$after scheduled=$acked ran=$ran stuck=$stuck late=$late"
rm -f "$out" "$out.acked" "$out.ran" "$out.stuck"

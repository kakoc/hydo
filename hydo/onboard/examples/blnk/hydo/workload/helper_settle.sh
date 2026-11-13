# The wait for the queue, shared by eventually_1_settle and eventually_2_settle.
# settle: wait until every transaction Blnk saved has reached a final state, and every
# acknowledged commit or void of a hold has its transaction. When some stay QUEUED, run Blnk's
# documented queue recovery (POST /transactions/recover with the smallest threshold it takes, 2
# minutes) as its own recovery processor would: Blnk runs it every 30 minutes for transactions
# QUEUED for 2 hours, and rejects a transaction after 3 failed recovery attempts. So up to 4
# passes, each followed by a wait. The threshold is on Blnk's clock, and so are the times of
# asynq's retries (a commit whose first attempt met PostgreSQL down waits in the retry set): after
# a backward clock step both come later. One call waits at most 3,300 s for them (a command's
# timeout is at most an hour), so two calls cover a step of an hour back. It fails only when
# PostgreSQL cannot be read at the end.
. /hydo/workload/helper_lib.sh
wait_settled() {   # <seconds>: 0 when nothing is pending for two polls in a row
  i=0 quiet=0 n=?
  while [ $i -lt "$1" ]; do
    if dump_tx > /tmp/settle-tx.jsonl; then
      n=$(/bin/blnkc pending /tmp/settle-tx.jsonl)
      if [ "$n" = 0 ]; then quiet=$((quiet+1)); [ $quiet -ge 2 ] && return 0; else quiet=0; fi
    else
      echo "postgres unreadable"
    fi
    sleep 2; i=$((i+2))
  done
  echo "after $1 s: $n pending"
  return 1
}
retry_wait() {   # seconds until the last task in asynq's retry and scheduled sets is due, plus 5
  now=$(date +%s) last=0
  for z in $(timeout 10 hydo-oci redis redis-cli --scan --pattern 'asynq:{*}:retry' 2>/dev/null) \
           $(timeout 10 hydo-oci redis redis-cli --scan --pattern 'asynq:{*}:scheduled' 2>/dev/null); do
    t=$(timeout 10 hydo-oci redis redis-cli zrange "$z" -1 -1 withscores 2>/dev/null | tail -n 1)
    t=${t%%.*}
    case "$t" in ''|*[!0-9]*) continue ;; esac
    [ "$t" -gt "$last" ] && last=$t
  done
  w=$((last - now + 5))
  [ "$w" -lt 0 ] && w=0
  echo "$w"
}
settle() {
  wait_settled 120 && { echo "settled"; return 0; }
  # seconds until the youngest transaction that waits for its outcome is 2 minutes old by Blnk's clock
  s=$(hydo-sql read sql db "SELECT coalesce(ceil(extract(epoch FROM max(p.created_at) - now() AT TIME ZONE 'UTC')), 0)::int + 125 AS s FROM blnk.transactions p WHERE p.status = 'QUEUED' AND NOT EXISTS (SELECT 1 FROM blnk.transactions c WHERE c.reference = p.reference || '_q')" | sed -n 's/.*"s":"*\(-*[0-9]*\).*/\1/p')
  case "$s" in ''|*[!0-9-]*) s=130 ;; esac
  [ "$s" -lt 0 ] && s=0
  # and until the last task that asynq holds for a retry or a delay is due, by the same clock
  r=$(retry_wait)
  [ "$r" -gt "$s" ] && s=$r
  if [ "$s" -gt 3300 ]; then
    echo "the queue recovery takes these transactions after $s s of Blnk's clock (a backward clock step); waiting 3300 s"
    s=3300
  fi
  sleep "$s"
  for pass in 1 2 3 4; do
    BLNKC_TIMEOUT_MS=60000 /bin/blnkc recover
    wait_settled 30 && { echo "settled after $pass passes of the queue recovery"; return 0; }
  done
  dump_tx > /tmp/settle-tx.jsonl || return 1
  echo "not settled after 4 passes of the queue recovery: $(/bin/blnkc pending /tmp/settle-tx.jsonl) pending"
  return 0
}

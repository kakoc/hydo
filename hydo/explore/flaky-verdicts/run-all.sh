#!/bin/bash
# run-all.sh <work dir> <task list> [runs]: the whole experiment, one task at a time on the build
# machine, with the environments made ahead on this computer.
#
# On this computer (Docker that runs linux/amd64 containers, the swebench package in
# <work dir>/venv, <work dir>/lite.json = SWE-bench Lite's rows): a preparation loop writes each
# task's files (spec.py) and makes its environment (prepare.sh), one at a time and at most two
# ahead; the main loop copies each environment to the build machine, runs task-remote.sh there
# (native runs, the hydo image, hydo runs), copies the results back and grades them
# (collect.py). Tasks with a <work dir>/out/<id>/summary.json are skipped. Stops when this
# computer has less than 20 GB free or the build machine's script stopped for its disk.
# INFLIGHT=<id>: a task already running on the build machine (from an interrupted run): wait for
# it and collect it first. The build machine needs ~/hydo-flaky/pfx (a hydo release prefix).
# On the build machine every command goes through on_build_machine (machines.sh), whose local
# definition keeps it inside the machine's shared budget for hydo work; nothing here starts a
# scope of its own, which would leave that budget.
set -u
D=$(cd "$(dirname "$0")" && pwd)
. "$D/../../../machines.sh"
need_build_machine
WD=$1
LIST=$2
N=${3:-10}
PY=$WD/venv/bin/python
mkdir -p "$WD/t" "$WD/env" "$WD/out" "$WD/logs"
echo $$ > "$WD/run-all.pid"
ids=($(grep -v '^#' "$LIST"))
mac_free() { /bin/df -g / | awk 'NR==2{print $4}'; }
say() { echo "$(date -u +%FT%TZ) $*" | tee -a "$WD/logs/run-all.log"; }
todo() { for id in "${ids[@]}"; do [ -f "$WD/out/$id/summary.json" ] || echo "$id"; done; }

prep_one() {
  local id=$1
  "$PY" "$D/spec.py" "$WD/lite.json" "$id" "$WD/t/$id" &&
    timeout 3600 bash "$D/prepare.sh" "$WD/t/$id" "$WD/env/$id.tar.gz.part" > "$WD/logs/$id.prep" 2>&1 &&
    mv "$WD/env/$id.tar.gz.part" "$WD/env/$id.tar.gz" || { touch "$WD/env/$id.failed"; say "prep failed: $id"; }
  rm -f "$WD/env/$id.busy"
}
prep_loop() {   # one at a time (each writes about 8 GB to Docker's disk), at most two made ahead
  local id
  for id in $(todo); do
    [ "$id" = "${INFLIGHT:-}" ] && continue
    [ -f "$WD/env/$id.tar.gz" ] && continue
    rm -f "$WD/env/$id.failed" "$WD/env/$id.tar.gz.part"
    local t=0
    while [ "$(ls "$WD"/env/*.busy 2>/dev/null | wc -l)" -ge 1 ] ||
          [ "$(ls "$WD"/env/*.tar.gz 2>/dev/null | wc -l)" -ge 2 ]; do
      sleep 10; t=$((t + 10)); [ $t -lt 36000 ] || return 1
    done
    [ "$(mac_free)" -ge 20 ] || { say "stop: $(mac_free) GB free on this computer"; touch "$WD/env/STOP"; return 1; }
    [ -f "$WD/env/STOP" ] && return 1
    touch "$WD/env/$id.busy"
    prep_one "$id" &
    sleep 5
  done
  wait
}
remote_wait() {   # at most 4 hours
  local end=$(($(date +%s) + 14400))
  while [ "$(date +%s)" -lt "$end" ]; do
    # "running" or "ended"; anything else (a dropped connection) is asked again
    st=$(on_build_machine 'p=$(cat ~/hydo-flaky/run.pid 2>/dev/null); if [ -n "$p" ] && kill -0 $p 2>/dev/null; then echo running; else echo ended; fi')
    [ "$st" = ended ] && return 0
    sleep 30
  done
  return 1
}
collect() {
  local id=$1
  rsync -a "$BUILD_MACHINE:hydo-flaky/out/$id" "$WD/out/"
  "$PY" "$D/collect.py" "$WD/lite.json" "$WD/out" "$id" | tee -a "$WD/logs/results.txt"
  if grep -q '^stop:' "$WD/out/$id/info" 2>/dev/null; then
    say "stop: the build machine's disk ($(grep '^stop:' "$WD/out/$id/info"))"
    touch "$WD/env/STOP"
    return 1
  fi
}

rm -f "$WD"/env/*.busy "$WD"/env/*.part "$WD/env/STOP"
rsync -a "$D/task-remote.sh" "$BUILD_MACHINE:hydo-flaky/scripts/"
prep_loop > /dev/null 2>&1 &
PREP=$!
if [ -n "${INFLIGHT:-}" ]; then
  remote_wait || say "time limit: $INFLIGHT"
  collect "$INFLIGHT" || exit 1
fi
for id in $(todo); do
  [ -f "$WD/env/STOP" ] && break
  t=0
  until [ -f "$WD/env/$id.tar.gz" ] || [ -f "$WD/env/$id.failed" ] || [ -f "$WD/env/STOP" ]; do
    sleep 10; t=$((t + 10)); [ $t -lt 7200 ] || break
  done
  [ -f "$WD/env/$id.tar.gz" ] || { say "skip $id (no environment)"; continue; }
  say "start $id ($(ls -l "$WD/env/$id.tar.gz" | awk '{print $5}') bytes), this computer $(mac_free) GB free"
  ok=
  for try in 1 2 3; do   # the home network drops a connection now and then
    rsync -a "$WD/env/$id.tar.gz" "$BUILD_MACHINE:hydo-flaky/env/" &&
      on_build_machine "cd ~/hydo-flaky && mkdir -p logs && (setsid nohup bash scripts/task-remote.sh $id $N > logs/$id.out 2>&1 & echo \$! > run.pid)" &&
      { ok=1; break; }
    sleep 30
  done
  [ -n "$ok" ] || { say "could not start $id on the build machine"; continue; }
  rm -f "$WD/env/$id.tar.gz"
  remote_wait || say "time limit: $id"
  collect "$id" || break
done
kill $PREP 2>/dev/null
say "done"
rm -f "$WD/run-all.pid"

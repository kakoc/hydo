#!/bin/sh
# Branches of one workspace state (docs/design/agent-environments.md): holds the run's stop before
# the agent's session (the provisioned workspace) in a `hydo inspect` session, then runs an agent's
# actions on named branches of it, each call's answer kept as JSON.
#
#   HYDO=<hydo> RUN=<run dir of "disk=none | run"> [OUT=<dir>] [AT=run:<stop>] sh branch-demo.sh
#
# Each answer has the command's output, `state_fp` (the engine's fingerprint of the whole guest
# state after the call) and `cost` (wall time, engine stops). After each call the session
# process's memory (/proc/<pid>/smaps_rollup) is appended to OUT/memory.txt. Linux only.
set -u
H=${HYDO:-hydo}
RUN=${RUN:?RUN=<run dir>}
OUT=${OUT:-$PWD/branch-demo}
AT=${AT:-run:243}
mkdir -p "$OUT"
: > "$OUT/memory.txt"
"$H" inspect start "$RUN" --at "$AT" --idle 900 > "$OUT/00-start.json" || exit 1
id=$(sed -n 's/.*"id": *"\(i-[0-9a-f]*\)".*/\1/p' "$OUT/00-start.json" | head -1)
[ -n "$id" ] || { echo "no session id in $OUT/00-start.json"; exit 1; }
echo "$id" > "$OUT/session"
home=${HYDO_HOME:-$HOME/.hydo}
pid=$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$home/inspect/$id/session.json" | head -1)
mem() { # <label>
  printf '%s ' "$1" >> "$OUT/memory.txt"
  grep -E '^(Rss|Pss|Pss_Anon|Pss_File|Pss_Shmem):' "/proc/$pid/smaps_rollup" | tr -s ' ' |
    tr '\n' ' ' >> "$OUT/memory.txt"
  echo >> "$OUT/memory.txt"
}
mem start
n=0
x() { # <label> <inspect exec arguments...>
  label=$1
  shift
  n=$((n + 1))
  f=$OUT/$(printf %02d $n)-$label.json
  "$H" inspect exec "$id" --timeout 300000 --max-bytes 4000 "$@" > "$f"
  mem "$label"
}
W='cd /data/ws &&'
TESTS='python -m pytest -q -p no:cacheprovider 2>&1 | tail -2'
FIX="sed -i 's/lot.qty = take/lot.qty -= take/' stockbook/ledger.py"
BADFIX="sed -i 's/lot.qty = take/lot.qty = 0/' stockbook/ledger.py"
WHEEL='python -m pip wheel . --no-deps --no-build-isolation -q -w dist && sha256sum dist/*.whl'

# the per-call cost on the stop itself (no branch: nothing is kept)
x true-1 -- true
x true-2 -- true
x true-3 -- true
# a: the same action on two branches
x tests-a --branch tests-a --sh "$W $TESTS"
x tests-b --branch tests-b --sh "$W $TESTS"
# b: different actions, one per branch
x fix-edit --branch fix --sh "$W $FIX && git diff --stat"
x fix-tests --branch fix --sh "$W $TESTS"
x badfix-edit --branch badfix --sh "$W $BADFIX && git diff --stat"
x badfix-tests --branch badfix --sh "$W $TESTS"
x wheel --branch wheel --sh "$W $WHEEL"
x commit --branch commit --sh "$W git commit -q --allow-empty -m 'an empty commit' && git log --oneline"
x entropy --branch entropy --sh 'python -c "import os, random, time, uuid; print(random.random(), uuid.uuid4(), os.urandom(8).hex(), time.time())"; date; cat /proc/uptime'
x network --branch network --sh "$W python -m pip install -q requests 2>&1 | tail -2"
# a branch of a branch: the fixed workspace, then commit and build
x fix-commit --branch fix2 --from fix --sh "$W git commit -q -am 'ledger: keep the rest of a partly shipped lot' && git log --oneline"
x fix-wheel --branch fix2 --sh "$W $WHEEL"
# c: the fix branch's two actions again on a new branch: the same state as the fix branch
x again-edit --branch again --sh "$W $FIX && git diff --stat"
x again-tests --branch again --sh "$W $TESTS"
# the same entropy action and the same build on second branches: the same "random" values, and
# the same wheel bytes (natively, two builds of the wheel differ: the files' times are in it)
x entropy-b --branch entropy-b --sh 'python -c "import os, random, time, uuid; print(random.random(), uuid.uuid4(), os.urandom(8).hex(), time.time())"; date; cat /proc/uptime'
x wheel-b --branch wheel-b --sh "$W $WHEEL"
"$H" inspect stop "$id" > "$OUT/99-stop.json"
echo "done: $OUT"

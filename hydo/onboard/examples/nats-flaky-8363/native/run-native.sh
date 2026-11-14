#!/bin/sh
# Run TestRegisterAndUnregister as the rows of RESULTS.md: two processes of 100 iterations each,
# with a GOMAXPROCS, and count the failed iterations.
#   sh run-native.sh <program> [gomaxprocs] [count] [processes]
#   sh run-native.sh work/ats.test 1          # one P: every second iteration fails
# gomaxprocs: 1 by default; `default` leaves Go's default (the machine's CPUs). count: 100.
# processes: 2. The first iteration of a process cannot fail, and a failed iteration leaves no
# goroutine behind, so a process of 100 iterations fails at most 50.
set -u
cd "$(dirname "$0")"
PROG=${1:?usage: sh run-native.sh <program> [gomaxprocs] [count] [processes]}
PROCS=${2:-1}
COUNT=${3:-100}
N=${4:-2}
case $PROG in /*) ;; *) PROG=$(pwd)/$PROG ;; esac
mkdir -p work/runs
fails=0
i=1
while [ "$i" -le "$N" ]; do
  if [ "$PROCS" = default ]; then
    "$PROG" -test.run '^TestRegisterAndUnregister$' -test.count="$COUNT" > "work/runs/$i.out" 2>&1
  else
    GOMAXPROCS=$PROCS "$PROG" -test.run '^TestRegisterAndUnregister$' -test.count="$COUNT" \
      > "work/runs/$i.out" 2>&1
  fi
  f=$(grep -c '^--- FAIL: TestRegisterAndUnregister' "work/runs/$i.out")
  echo "process $i: $f of $COUNT iterations failed"
  fails=$((fails + f))
  i=$((i + 1))
done
echo "GOMAXPROCS=$PROCS: failed $fails of $((COUNT * N)) iterations"
grep -h -m1 'Expected same number of go routines' work/runs/*.out | sed 's/^ *//' | sort | uniq -c

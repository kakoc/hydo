#!/bin/bash
# Wait (at most <secs>) until every given file has a line "done"; print which finished.
#   waitdone.sh <secs> <file>...
secs=$1; shift
end=$((SECONDS + secs))
while [ $SECONDS -lt $end ]; do
  all=1
  for f in "$@"; do grep -qx done "$f" 2>/dev/null || all=0; done
  [ $all = 1 ] && { echo "all done: $*"; exit 0; }
  sleep 10
done
echo "timeout after $secs s:"; for f in "$@"; do grep -qx done "$f" 2>/dev/null && echo " done $f" || echo " running $f"; done
exit 1

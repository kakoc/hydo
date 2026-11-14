#!/bin/sh
# The probe that showed the race: a 60 ms sleep in the test before its Phase-2 `cache.Get` makes
# the cache's retry (50 ms after the compaction) load the second snapshot first. RESULTS.md: the
# test then failed 3 of 3 times with `expected cached revision 5 after compaction, got 20`.
#   sh sleep-probe.sh [count]       # count: 3 by default
# The sleep is a probe, not a fix. The script inserts it into a clone at the commit of
# ../build-test.sh (work/etcd-src, shared with build-program.sh), runs `go test` in cache/, and
# restores the file. Needs git, network once, and Go; GOTOOLCHAIN fetches go1.27.4.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COUNT=${1:-3}
COMMIT=1b969e26a2c65733f1ff696e3537373d56b3ae67
SRC=$HERE/work/etcd-src
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/etcd-io/etcd.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
F=$SRC/cache/cache_test.go
trap 'git -C "$SRC" checkout -q -f "$COMMIT" -- cache/cache_test.go' EXIT INT TERM
# The first `ctxGet, cancelGet :=` line is Phase 2's Get in TestCacheCompactionResync.
awk 'done != 1 && /ctxGet, cancelGet := context.WithTimeout/ {
       print "\ttime.Sleep(60 * time.Millisecond) // probe: let the retry run first"; done = 1 }
     { print }' "$F" > "$F.new"
mv "$F.new" "$F"
grep -n 'probe: let the retry run first' "$F"
cd "$SRC/cache"
GOTOOLCHAIN=go1.27.4 go test -run '^TestCacheCompactionResync$' -count="$COUNT" -cpu=1 -v . 2>&1 |
  grep -e '^--- ' -e 'expected cached revision' -e '^ok' -e '^FAIL' || true

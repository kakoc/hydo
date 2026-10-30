#!/usr/bin/env bash
# Byte-for-byte conformance of hydo_assert.history (Python) with hydohist (Go): build
# tests/history-go/main.go against the Go recorder's own sources (copied from
# hvx/stack/hydohist), run it and tests/history_conformance.py with every sink setting, and
# compare their records and stdouts.
#
# The time field (field 5 of a HYDO-HIST line) differs between runs and implementations: the
# records are compared with it replaced by "*", and each file is checked separately for time
# fields that are decimal integers and never decrease.
#
# Sink settings: "append" (HYDO_HIST_OUT names an existing file: records are appended after its
# content), "create" (a missing file: created, same permissions), "stderr", "fallback" (a path
# that cannot be opened: standard error) and "off" (nothing anywhere; the stdout of the helper
# functions is still compared).
#
# Usage: sdk/python/tests/history_conformance.sh [python...]   (needs go; default: python3;
#        give several interpreters to check each, e.g. "uv run -p 3.8 python").
#        GOTOOLCHAIN picks the Go version (default go1.26.5); KEEP=<dir> keeps the outputs.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(dirname "$HERE")
REPO=$(cd "$PKG/../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hydo-pyhist.XXXXXX")
trap 'rm -rf "$T"' EXIT
[ $# -gt 0 ] || set -- python3
export LC_ALL=C
sha() { if command -v sha256sum > /dev/null; then sha256sum; else shasum -a 256; fi; }

export GOTOOLCHAIN=${GOTOOLCHAIN:-go1.26.5}
GO=${GO:-go}
mkdir -p "$T/go/hydohist"
cp "$REPO"/hvx/stack/hydohist/hydohist.go "$REPO"/hvx/stack/hydohist/mono_linux.go \
   "$REPO"/hvx/stack/hydohist/mono_other.go "$T/go/hydohist/"
cp "$HERE/history-go/main.go" "$T/go/"
printf 'module conf\n\ngo 1.21\n' > "$T/go/go.mod"
(cd "$T/go" && GOFLAGS=-mod=mod "$GO" build -o "$T/hist-go" .)
echo "history conformance: go $("$GO" version | cut -d' ' -f3)"

# mask: field 5 of every HYDO-HIST line becomes "*"; other lines stay.
mask() { awk 'BEGIN { FS = "[ ]"; OFS = " " } /^HYDO-HIST / { $5 = "*" } { print }' "$1"; }
# times_ok: every HYDO-HIST time field is a decimal integer, none smaller than the one before
# (compared as decimal strings: by length, then lexically).
times_ok() {
  awk 'BEGIN { FS = "[ ]"; prev = ""; bad = 0 }
       /^HYDO-HIST / { t = $5
         if (t !~ /^[0-9]+$/) { bad = 1; exit }
         if (prev != "" && (length(t) < length(prev) || (length(t) == length(prev) && t < prev))) { bad = 1; exit }
         prev = t }
       END { exit bad }' "$1"
}
perm() { ls -l "$1" | cut -c1-10; }

# run <mode> <impl> <cmd...>: one program under one sink setting.
run() {
  local mode=$1 impl=$2; shift 2
  local rec="$T/$mode.$impl.records" so="$T/$mode.$impl.stdout" se="$T/$mode.$impl.stderr"
  case $mode in
    append)   printf 'PRE existing line\n' > "$rec"
              env HYDO_HIST_OUT="$rec" "$@" > "$so" 2> "$se" ;;
    create)   rm -f "$rec"; (umask 022; env HYDO_HIST_OUT="$rec" "$@" > "$so" 2> "$se") ;;
    stderr)   env HYDO_HIST_OUT=stderr "$@" > "$so" 2> "$rec" ;;
    fallback) env HYDO_HIST_OUT="$T/no-such-dir/x" "$@" > "$so" 2> "$rec" ;;
    off)      env HYDO_HIST_OUT=off "$@" > "$so" 2> "$rec" ;;
  esac
  if [ "$mode" = append ] || [ "$mode" = create ]; then
    if [ -s "$se" ]; then echo "history conformance: $mode $impl wrote to stderr:"; head -5 "$se"; fail=1; fi
  fi
  if ! times_ok "$rec"; then echo "history conformance: $mode $impl time fields NOT increasing decimals"; fail=1; fi
  mask "$rec" > "$rec.masked"
}

fail=0
MODES="append create stderr fallback off"
for mode in $MODES; do
  run "$mode" go "$T/hist-go"
  n=$(grep -c '^HYDO-HIST ' "$T/$mode.go.records" || true)
  if [ "$mode" = off ]; then
    [ "$n" = 0 ] || { echo "history conformance: off go wrote $n records"; fail=1; }
  elif [ "$n" -lt 100 ]; then
    echo "history conformance: $mode go wrote only $n records"; fail=1
  fi
done
for py in "$@"; do
  echo "history conformance: python = $py ($($py -c 'import sys; print(sys.version.split()[0])'))"
  for mode in $MODES; do
    # shellcheck disable=SC2086
    run "$mode" py $py "$HERE/history_conformance.py"
    n=$(grep -c '^HYDO-HIST ' "$T/$mode.go.records" || true)
    if [ "$mode" = create ]; then
      p=$(perm "$T/create.go.records"); q=$(perm "$T/create.py.records")
      if [ "$p" = "$q" ]; then echo "history conformance: create permissions identical ($p)"
      else echo "history conformance: create permissions DIFFER (go $p, python $q)"; fail=1; fi
    fi
    for k in records.masked stdout; do
      a="$T/$mode.go.$k"; b="$T/$mode.py.$k"
      if cmp -s "$a" "$b"; then
        echo "history conformance: $mode ${k%.masked} identical ($(wc -l < "$a" | tr -d ' ') lines, $n records, sha256 $(sha < "$a" | cut -c1-16))"
      else
        echo "history conformance: $mode ${k%.masked} DIFFERS"; { diff "$a" "$b" || true; } | head -20; fail=1
      fi
    done
  done
done
[ "${KEEP:-}" ] && cp -r "$T/." "$KEEP"
exit $fail

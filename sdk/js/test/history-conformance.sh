#!/usr/bin/env bash
# Byte-for-byte conformance of hydo-assert/history (JavaScript) with hydohist (Go): build
# test/history-go/main.go against the Go recorder's own sources (copied from
# hvx/stack/hydohist), run it and test/history-conformance.js (through the CommonJS and through
# the ESM entry) with every sink setting, and compare their records and stdouts.
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
# Usage: sdk/js/test/history-conformance.sh   (needs go and node; exit 0 = identical)
#        KEEP=<dir> keeps the outputs. GOTOOLCHAIN picks the Go version (default go1.26.5).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(dirname "$HERE")
REPO=$(cd "$PKG/../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hydo-jshist.XXXXXX")
trap 'rm -rf "$T"' EXIT
export LC_ALL=C

export GOTOOLCHAIN=${GOTOOLCHAIN:-go1.26.5}
mkdir -p "$T/go/hydohist"
cp "$REPO"/hvx/stack/hydohist/hydohist.go "$REPO"/hvx/stack/hydohist/mono_linux.go \
   "$REPO"/hvx/stack/hydohist/mono_other.go "$T/go/hydohist/"
cp "$HERE/history-go/main.go" "$T/go/"
printf 'module conf\n\ngo 1.21\n' > "$T/go/go.mod"
(cd "$T/go" && GOFLAGS=-mod=mod go build -o "$T/hist-go" .)
echo "history conformance: $(go version | cut -d' ' -f3), node $(node --version)"

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

fail=0
for mode in append create stderr fallback off; do
  for impl in go cjs esm; do
    case $impl in
      go) cmd=("$T/hist-go") ;;
      cjs) cmd=(env HYDO_JS_ENTRY=cjs node "$HERE/history-conformance.js") ;;
      esm) cmd=(env HYDO_JS_ENTRY=esm node "$HERE/history-conformance.js") ;;
    esac
    rec="$T/$mode.$impl.records"; so="$T/$mode.$impl.stdout"; se="$T/$mode.$impl.stderr"
    case $mode in
      append)   printf 'PRE existing line\n' > "$rec"
                env HYDO_HIST_OUT="$rec" "${cmd[@]}" > "$so" 2> "$se" ;;
      create)   rm -f "$rec"; (umask 022; env HYDO_HIST_OUT="$rec" "${cmd[@]}" > "$so" 2> "$se") ;;
      stderr)   env HYDO_HIST_OUT=stderr "${cmd[@]}" > "$so" 2> "$rec" ;;
      fallback) env HYDO_HIST_OUT="$T/no-such-dir/x" "${cmd[@]}" > "$so" 2> "$rec" ;;
      off)      env HYDO_HIST_OUT=off "${cmd[@]}" > "$so" 2> "$rec" ;;
    esac
    if [ "$mode" = append ] || [ "$mode" = create ]; then
      if [ -s "$se" ]; then echo "history conformance: $mode $impl wrote to stderr"; fail=1; fi
    fi
    if ! times_ok "$rec"; then echo "history conformance: $mode $impl time fields NOT increasing decimals"; fail=1; fi
    mask "$rec" > "$rec.masked"
  done
  n=$(grep -c '^HYDO-HIST ' "$T/$mode.go.records" || true)
  if [ "$mode" = off ]; then
    [ "$n" = 0 ] || { echo "history conformance: off go wrote $n records"; fail=1; }
  elif [ "$n" -lt 100 ]; then
    echo "history conformance: $mode go wrote only $n records"; fail=1
  fi
  if [ "$mode" = create ]; then
    p=$(perm "$T/create.go.records")
    for impl in cjs esm; do
      q=$(perm "$T/create.$impl.records")
      if [ "$p" = "$q" ]; then echo "history conformance: create $impl permissions identical ($p)"
      else echo "history conformance: create $impl permissions DIFFER (go $p, $impl $q)"; fail=1; fi
    done
  fi
  for impl in cjs esm; do
    for k in records.masked stdout; do
      a="$T/$mode.go.$k"; b="$T/$mode.$impl.$k"
      if cmp -s "$a" "$b"; then
        echo "history conformance: $mode $impl ${k%.masked} identical ($(wc -l < "$a" | tr -d ' ') lines, $n records, sha256 $(shasum -a 256 < "$a" | cut -c1-16))"
      else
        echo "history conformance: $mode $impl ${k%.masked} DIFFERS"; { diff "$a" "$b" || true; } | head -20; fail=1
      fi
    done
  done
done
[ "${KEEP:-}" ] && cp -r "$T/." "$KEEP"
exit $fail

#!/usr/bin/env bash
# Byte-for-byte conformance of hydo_assert (Python) with hydoassert (Go): build
# tests/go-conformance/main.go against the Go SDK's own sources (copied from
# hydo/stack/hydoassert), run it and tests/conformance.py under several BUGGIFY configurations
# (seed in the environment, seed in the file, none, and malformed ones that Go's strconv
# rejects), and diff their consoles and stdouts.
# Usage: sdk/python/tests/conformance.sh [python...]   (needs go; default python: python3;
#        give several interpreters to check each, e.g. "uv run -p 3.8 python").
#        GOTOOLCHAIN picks the Go version (default go1.26.5); KEEP=<dir> keeps the outputs.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(dirname "$HERE")
REPO=$(cd "$PKG/../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hydo-pyconf.XXXXXX")
trap 'rm -rf "$T"' EXIT
[ $# -gt 0 ] || set -- python3
sha() { if command -v sha256sum > /dev/null; then sha256sum; else shasum -a 256; fi; }

mkdir -p "$T/go/hydoassert"
cp "$REPO/hydo/stack/hydoassert/hydoassert.go" "$REPO/hydo/stack/hydoassert/eventually.go" "$T/go/hydoassert/"
cp "$HERE/go-conformance/main.go" "$T/go/"
printf 'module conf\n\ngo 1.21\n' > "$T/go/go.mod"
GO=${GO:-go}
# The guest images are built with Go 1.26.5 (docs/BUILD-PINS.md); pin it as sdk/js does. Set
# GOTOOLCHAIN=local to use whatever `go` is installed.
export GOTOOLCHAIN=${GOTOOLCHAIN:-go1.26.5}
(cd "$T/go" && GOFLAGS=-mod=mod "$GO" build -o "$T/conf-go" .)
gov=$("$GO" version | cut -d' ' -f3)
# Invalid UTF-8 in a string: Go <= 1.26 (the pinned guest Go is 1.26.5) writes \ufffd per bad
# byte, as hydo_assert does; Go 1.27 writes a raw U+FFFD. Compare that case only on <= 1.26.
case $gov in go1.2[7-9]*|go1.[3-9]*) unset HYDO_CONF_INVALID_UTF8; inv=no ;;
                *) export HYDO_CONF_INVALID_UTF8=1; inv=yes ;; esac
echo "conformance: go $gov (invalid UTF-8 case: $inv)"

printf '  beef,1000,300\n' > "$T/bgfile"
fail=0
for py in "$@"; do
  echo "conformance: python = $py ($($py -c 'import sys; print(sys.version.split()[0])'))"
  for mode in env file none odd prefix overflow sign all; do
    for impl in go py; do
      if [ $impl = go ]; then cmd=("$T/conf-go"); else cmd=($py "$HERE/conformance.py"); fi
      out="$T/$mode.$impl.console"; : > "$out"
      case $mode in
        env)      bg=1234abcd,500,500 ;;
        odd)      bg=$'  BEEF,07,+5\t' ;;     # enable 7; "+5" rejected (fire stays 250)
        prefix)   bg=0xbeef ;;                 # ParseUint(.., 16) rejects 0x: BUGGIFY off
        overflow) bg=1ffffffffffffffff ;;       # > 2^64-1: off
        sign)     bg='ab, 300,1_0' ;;          # " 300" and "1_0" rejected: defaults
        all)      bg=ffffffffffffffff,1000,1000 ;;
        *)        bg= ;;
      esac
      case $mode in
        file) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE="$T/bgfile" "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
        none) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE=/nonexistent "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
        *)    env HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY="$bg" HYDO_BUGGIFY_FILE=/nonexistent "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
      esac
    done
    for k in console stdout; do
      if cmp -s "$T/$mode.go.$k" "$T/$mode.py.$k"; then
        echo "conformance: $mode $k identical ($(wc -l < "$T/$mode.go.$k" | tr -d ' ') lines, sha256 $(sha < "$T/$mode.go.$k" | cut -c1-16))"
      else
        echo "conformance: $mode $k DIFFERS"; { diff "$T/$mode.go.$k" "$T/$mode.py.$k" || true; } | head -20; fail=1
      fi
    done
  done
done
[ "${KEEP:-}" ] && cp -r "$T" "$KEEP"
exit $fail

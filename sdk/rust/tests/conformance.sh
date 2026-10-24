#!/usr/bin/env bash
# Byte-for-byte conformance of hydo-assert (Rust) with hydoassert (Go): build
# tests/go-conformance/main.go against the Go SDK's own sources (copied from
# hvx/stack/hydoassert) and examples/conformance.rs, run both under three BUGGIFY
# configurations (env seed, config file, none), and diff their consoles and stdouts.
# Usage: sdk/rust/tests/conformance.sh   (needs go and cargo; exit 0 = identical)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
CRATE=$(dirname "$HERE")
REPO=$(cd "$CRATE/../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hydo-conf.XXXXXX")
trap 'rm -rf "$T"' EXIT

mkdir -p "$T/go/hydoassert"
cp "$REPO/hvx/stack/hydoassert/hydoassert.go" "$REPO/hvx/stack/hydoassert/eventually.go" "$T/go/hydoassert/"
cp "$HERE/go-conformance/main.go" "$T/go/"
printf 'module conf\n\ngo 1.21\n' > "$T/go/go.mod"
(cd "$T/go" && GOFLAGS=-mod=mod go build -o "$T/conf-go" .)
(cd "$CRATE" && cargo build -q --example conformance)
RS="$CRATE/target/debug/examples/conformance"

printf '  beef,1000,300\n' > "$T/bgfile"
fail=0
for mode in env file none; do
  for impl in go rs; do
    bin="$T/conf-go"; [ $impl = rs ] && bin="$RS"
    out="$T/$mode.$impl.console"; : > "$out"
    case $mode in
      env)  env HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY=1234abcd,500,500 HYDO_BUGGIFY_FILE=/nonexistent "$bin" > "$T/$mode.$impl.stdout" ;;
      file) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE="$T/bgfile" "$bin" > "$T/$mode.$impl.stdout" ;;
      none) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE=/nonexistent "$bin" > "$T/$mode.$impl.stdout" ;;
    esac
  done
  for k in console stdout; do
    if cmp -s "$T/$mode.go.$k" "$T/$mode.rs.$k"; then
      echo "conformance: $mode $k identical ($(wc -l < "$T/$mode.go.$k") lines, sha256 $(shasum -a 256 "$T/$mode.go.$k" | cut -c1-16))"
    else
      echo "conformance: $mode $k DIFFERS"; diff "$T/$mode.go.$k" "$T/$mode.rs.$k" | head -20; fail=1
    fi
  done
done
[ "${KEEP:-}" ] && cp -r "$T" "$KEEP"
exit $fail

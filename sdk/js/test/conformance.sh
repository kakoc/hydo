#!/usr/bin/env bash
# Byte-for-byte conformance of hydo-assert (JavaScript) with hydoassert (Go): build
# test/go-conformance/main.go against the Go SDK's own sources (copied from
# hydo/stack/hydoassert), run it and test/conformance.js (through the CommonJS and through the
# ESM entry) under five BUGGIFY configurations, and diff their consoles and stdouts.
# Usage: sdk/js/test/conformance.sh   (needs go and node; exit 0 = identical)
#        KEEP=<dir> keeps the outputs. GOTOOLCHAIN picks the Go version (default go1.26.5, the
#        version the repository's Go programs are built with; Go 1.27 changed one detail of
#        encoding/json -- invalid UTF-8 becomes a raw U+FFFD instead of the escape \ufffd -- so
#        with Go 1.27 the "bad" detail differs and nothing else).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(dirname "$HERE")
REPO=$(cd "$PKG/../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hydo-jsconf.XXXXXX")
trap 'rm -rf "$T"' EXIT

export GOTOOLCHAIN=${GOTOOLCHAIN:-go1.26.5}
mkdir -p "$T/go/hydoassert"
cp "$REPO/hydo/stack/hydoassert/hydoassert.go" "$REPO/hydo/stack/hydoassert/eventually.go" "$T/go/hydoassert/"
cp "$HERE/go-conformance/main.go" "$T/go/"
printf 'module conf\n\ngo 1.21\n' > "$T/go/go.mod"
(cd "$T/go" && GOFLAGS=-mod=mod go build -o "$T/conf-go" .)
echo "conformance: $(go version), node $(node --version)"

printf '  beef,1000,300\n' > "$T/bgfile"
fail=0
for mode in env file none odd bad; do
  for impl in go cjs esm; do
    case $impl in
      go) cmd=("$T/conf-go") ;;
      cjs) cmd=(env HYDO_JS_ENTRY=cjs node "$HERE/conformance.js") ;;
      esm) cmd=(env HYDO_JS_ENTRY=esm node "$HERE/conformance.js") ;;
    esac
    out="$T/$mode.$impl.console"; : > "$out"
    case $mode in
      env)  env HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY=1234abcd,500,500 HYDO_BUGGIFY_FILE=/nonexistent "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
      file) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE="$T/bgfile" "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
      none) env -u HYDO_BUGGIFY HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY_FILE=/nonexistent "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
      odd)  env HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY='FFFFFFFFFFFFFFFF,abc,1000' "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
      bad)  env HYDO_ASSERT_OUT="$out" HYDO_BUGGIFY='0x12' "${cmd[@]}" > "$T/$mode.$impl.stdout" ;;
    esac
  done
  for impl in cjs esm; do
    for k in console stdout; do
      if cmp -s "$T/$mode.go.$k" "$T/$mode.$impl.$k"; then
        echo "conformance: $mode $impl $k identical ($(wc -l < "$T/$mode.go.$k" | tr -d ' ') lines, sha256 $(shasum -a 256 "$T/$mode.go.$k" | cut -c1-16))"
      else
        echo "conformance: $mode $impl $k DIFFERS"; diff "$T/$mode.go.$k" "$T/$mode.$impl.$k" | head -20; fail=1
      fi
    done
  done
done
[ "${KEEP:-}" ] && cp -r "$T/." "$KEEP"
exit $fail

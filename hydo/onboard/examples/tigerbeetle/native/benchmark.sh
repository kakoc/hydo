#!/bin/sh
# Run TigerBeetle's own benchmark with pending transfers (holds) on this machine, with the 0.17.9
# release. This is one of the native load runs of RESULTS.md that never stopped the replica:
# ordinary load does not put two holds on one expiry nanosecond.
#   sh benchmark.sh [benchmark options]      # default: --transfer-pending
# Downloads the release for this machine into work/0.17.9/ and checks its sha256. The benchmark
# starts its own replica on a temporary data file and removes it at the end.
set -eu
cd "$(dirname "$0")"
case "$(uname -s)-$(uname -m)" in
  Darwin-*)        zip=tigerbeetle-universal-macos.zip; want=857269f799203221a935e44c8b25e690388c63430c13b98721196f8d4ad6a860 ;;
  Linux-x86_64)    zip=tigerbeetle-x86_64-linux.zip;    want=074aafd9287e03c07b068937b7b39dc9e563715b165e0a4c893e4ae2b8e63939 ;;
  Linux-aarch64)   zip=tigerbeetle-aarch64-linux.zip;   want=9802b8e3b89229e2019cfede6a467c5b1f1ad5d5405dc9d8363d55b83a866733 ;;
  *) echo "benchmark.sh: no release for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
D=work/0.17.9
mkdir -p "$D"
if [ ! -f "$D/tigerbeetle" ] || [ "$(sum "$D/tigerbeetle" | cut -d' ' -f1)" != "$want" ]; then
  curl -fsSL -o "$D/tb.zip" "https://github.com/tigerbeetle/tigerbeetle/releases/download/0.17.9/$zip"
  (cd "$D" && unzip -oq tb.zip tigerbeetle && rm tb.zip)
  got=$(sum "$D/tigerbeetle" | cut -d' ' -f1)
  [ "$got" = "$want" ] || { echo "benchmark.sh: $D/tigerbeetle sha256 $got, expected $want" >&2; exit 1; }
fi
[ "$#" -gt 0 ] || set -- --transfer-pending
"$D/tigerbeetle" benchmark "$@"

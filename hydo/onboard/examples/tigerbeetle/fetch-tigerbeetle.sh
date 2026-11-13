#!/bin/sh
# Download the TigerBeetle releases this example runs (the static Linux binary from each GitHub
# release) into bin/<name>/tigerbeetle, checked by sha256. Needs network once. A name is a
# release (the x86_64 binary) or <release>-aarch64 (the arm64 binary). The Mac engine stops the
# arm64 binary's boot as a loop without VM exits (RESULTS.md, "Holds and expiry"), so the example
# runs the x86_64 binaries on hvx.
#   sh fetch-tigerbeetle.sh                  # every x86_64 release below
#   sh fetch-tigerbeetle.sh 0.17.9           # one
set -e
cd "$(dirname "$0")"
pin() {   # name -> sha256 of the unpacked binary
  case $1 in
    0.17.9)          echo 074aafd9287e03c07b068937b7b39dc9e563715b165e0a4c893e4ae2b8e63939 ;;
    0.16.45)         echo b5cdc1a891a40d75fab7b843f63596ecb9fecf67c1e19d8e4c7a0643e0c0611b ;;
    0.16.29)         echo 57ded7ddad47e79e94b7abd9250baed9b8169bc44bb26ccdc66c8efca8837032 ;;
    0.16.28)         echo c3b5a97e20e268a7573eef9b6a39485ff4a64db4e50def8502eba33aab28fb4a ;;
    0.17.9-aarch64)  echo 9802b8e3b89229e2019cfede6a467c5b1f1ad5d5405dc9d8363d55b83a866733 ;;
    0.16.29-aarch64) echo fb47acf5ea7527f8e81242e07c4b220eb1046160cc50fcac6c5a6d5cbc62b51e ;;
    0.16.28-aarch64) echo 28d6caba2804bef8668b9bb2ecd6dfbb6fa72d828557362bd7372518c08a4424 ;;
    *) echo "unknown version $1" >&2; exit 1 ;;
  esac
}
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
for v in ${*:-0.17.9 0.16.45 0.16.29 0.16.28}; do
  want=$(pin "$v")
  case $v in *-aarch64) rel=${v%-aarch64} arch=aarch64 ;; *) rel=$v arch=x86_64 ;; esac
  mkdir -p "bin/$v"
  if [ -f "bin/$v/tigerbeetle" ] && [ "$(sum "bin/$v/tigerbeetle" | cut -d' ' -f1)" = "$want" ]; then
    echo "bin/$v/tigerbeetle ok"; continue
  fi
  curl -fsSL -o "bin/$v/tb.zip" "https://github.com/tigerbeetle/tigerbeetle/releases/download/$rel/tigerbeetle-$arch-linux.zip"
  (cd "bin/$v" && unzip -oq tb.zip tigerbeetle && rm tb.zip)
  got=$(sum "bin/$v/tigerbeetle" | cut -d' ' -f1)
  [ "$got" = "$want" ] || { echo "bin/$v/tigerbeetle: sha256 $got, expected $want" >&2; exit 1; }
  echo "bin/$v/tigerbeetle ok"
done

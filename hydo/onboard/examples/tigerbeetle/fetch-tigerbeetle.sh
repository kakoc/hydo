#!/bin/sh
# Download the TigerBeetle releases this example runs (the static x86_64 Linux binary from each
# GitHub release) into bin/<version>/tigerbeetle, checked by sha256. Needs network once.
#   sh fetch-tigerbeetle.sh            # every version below
#   sh fetch-tigerbeetle.sh 0.17.9     # one
set -e
cd "$(dirname "$0")"
pin() {   # version -> sha256 of the unpacked binary
  case $1 in
    0.17.9)  echo 074aafd9287e03c07b068937b7b39dc9e563715b165e0a4c893e4ae2b8e63939 ;;
    0.16.45) echo b5cdc1a891a40d75fab7b843f63596ecb9fecf67c1e19d8e4c7a0643e0c0611b ;;
    0.16.29) echo 57ded7ddad47e79e94b7abd9250baed9b8169bc44bb26ccdc66c8efca8837032 ;;
    *) echo "unknown version $1" >&2; exit 1 ;;
  esac
}
for v in ${*:-0.17.9 0.16.45 0.16.29}; do
  want=$(pin "$v")
  mkdir -p "bin/$v"
  if [ -f "bin/$v/tigerbeetle" ] && echo "$want  bin/$v/tigerbeetle" | sha256sum -c --status; then
    echo "bin/$v/tigerbeetle ok"; continue
  fi
  curl -fsSL -o "bin/$v/tb.zip" "https://github.com/tigerbeetle/tigerbeetle/releases/download/$v/tigerbeetle-x86_64-linux.zip"
  (cd "bin/$v" && unzip -oq tb.zip tigerbeetle && rm tb.zip)
  echo "$want  bin/$v/tigerbeetle" | sha256sum -c
done

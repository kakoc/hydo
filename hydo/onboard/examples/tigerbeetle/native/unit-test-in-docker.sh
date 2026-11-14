#!/bin/sh
# Run unit-test.sh inside a Linux container, for a machine where the Zig that TigerBeetle pins
# cannot build. On macOS 26.4.1 the Zig 0.14.1 of the release 0.17.9 fails to link its build
# program (`error: undefined symbol: _clock_gettime`); the Zig 0.16.0 of main works there.
#   sh unit-test-in-docker.sh            # TB_COMMIT=<commit> as for unit-test.sh
# The container (Debian 12.12, named hydo-native-tb-unit) mounts this folder, installs git, curl
# and unzip, runs unit-test.sh, and is removed when it ends. Needs Docker.
set -eu
cd "$(dirname "$0")"
# A Zig cache built by another system is not reusable here.
rm -rf work/tigerbeetle-src/.zig-cache
docker run --rm --name hydo-native-tb-unit -e TB_COMMIT="${TB_COMMIT:-}" -v "$(pwd)":/w -w /w \
  debian:12.12-slim sh -c '
  apt-get update -qq >/dev/null &&
  apt-get install -y -qq git curl ca-certificates unzip xz-utils >/dev/null &&
  git config --global --add safe.directory "*" &&
  sh unit-test.sh'

#!/bin/sh
# Run the unit test of issue 3960 on TigerBeetle's main of the day the issue was opened:
#   sh unit-test.sh                 # TB_COMMIT=<commit> runs it on another commit
# On the release 0.17.9 (cc1c06a9) the same test passes; the issue's numbers fit main's pulse.
# Clones TigerBeetle into work/tigerbeetle-src, checks out the commit, appends issue-3960-test.zig to
# src/state_machine_tests.zig, downloads TigerBeetle's pinned Zig with its own zig/download.sh,
# and runs only that test. The issue reports that it ends with `panic: reached unreachable code`
# at `assert(expires_at <= timestamp_event)` in src/state_machine.zig. The script restores the
# test file at the end. Needs git, curl and network once; the build takes a few minutes.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COMMIT=${TB_COMMIT:-8cbd6065f8b02c4eb9a21938d6a826d6e3ac71a4}   # main, 2026-11-10 11:29 UTC
SRC=$HERE/work/tigerbeetle-src
# A clone with the whole history but without old file contents: on Linux, TigerBeetle's build
# lists the earlier release tags of HEAD^ (build.zig, release_history).
[ -d "$SRC/.git" ] || git clone -q --filter=blob:none --no-checkout \
  https://github.com/tigerbeetle/tigerbeetle.git "$SRC"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
trap 'git -C "$SRC" checkout -q -f "$COMMIT" -- src/state_machine_tests.zig' EXIT INT TERM
cat "$HERE/issue-3960-test.zig" >> "$SRC/src/state_machine_tests.zig"
cd "$SRC"
./zig/download.sh   # skips the download when the right Zig is there
./zig/zig build test:unit --summary all -- "expire_pending_transfers: expiry not before expires_at"

#!/bin/sh
# Checks the external links (http and https) of the documents that ci/vale.sh checks, with
# lychee 0.24.2. Relative links and anchors are checked offline by the test
# `cargo test -p hydo --test it docs::`; this script needs the network, so CI runs it only in the
# nightly and manual job of .github/workflows/docs.yml, and no push depends on other sites.
#
#   sh ci/links-external.sh
#
# Linux x86_64 only (the CI runner). The download is checked by sha256.
set -eu

LYCHEE=lychee-x86_64-unknown-linux-gnu
LYCHEE_URL=https://github.com/lycheeverse/lychee/releases/download/lychee-v0.24.2/$LYCHEE.tar.gz
LYCHEE_SHA256=1f4e0ef7f6554a6ed33dd7ac144fb2e1bbed98598e7af973042fc5cd43951c9a

root=$(cd "$(dirname "$0")/.." && pwd)
cache=${XDG_CACHE_HOME:-$HOME/.cache}/hydo-lychee
mkdir -p "$cache"
if [ ! -x "$cache/$LYCHEE/lychee" ]; then
  curl -fsSL --retry 3 -o "$cache/lychee.tar.gz" "$LYCHEE_URL"
  got=$(sha256sum "$cache/lychee.tar.gz" | cut -d' ' -f1)
  if [ "$got" != "$LYCHEE_SHA256" ]; then
    echo "ci/links-external.sh: lychee has sha256 $got, expected $LYCHEE_SHA256" >&2
    rm -f "$cache/lychee.tar.gz"
    exit 2
  fi
  tar xzf "$cache/lychee.tar.gz" -C "$cache"
fi

cd "$root"
# shellcheck disable=SC2046
"$cache/$LYCHEE/lychee" --no-progress --scheme https --scheme http --max-retries 3 \
  $(sh ci/vale.sh --list)

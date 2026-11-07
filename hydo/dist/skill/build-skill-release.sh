#!/bin/sh
# Build the onboarding skill's release assets (Milestone 170):
#     sh hydo/dist/skill/build-skill-release.sh [OUT_DIR]
# writes OUT_DIR/hydo-skill-<version>.tar.gz (the folder skills/hydo-onboard/ as hydo-onboard/)
# and OUT_DIR/SHA256SUMS (default OUT_DIR: hydo/dist/out/skill, git-ignored). The archive is made
# by hydo-skill-pack (hydo_agents::release::pack): sorted entries, every time 0, owner 0:0, modes
# 0644 or 0755, gzip without a name or time, so the same folder gives the same bytes on a Mac and
# on Linux. The script packs twice and compares, checks that SKILL.md's metadata.version equals
# the version in .claude-plugin/marketplace.json, and prints the tag to publish, skill-v<version>.
# It tags, uploads and publishes nothing: docs/RELEASING.md "Releasing the onboarding skill".
set -eu

TOP=$(cd "$(dirname "$0")/../../.." && pwd)
OUT=${1:-$TOP/hydo/dist/out/skill}
SKILL=$TOP/skills/hydo-onboard

cd "$TOP"
if [ -n "$(git status --porcelain -- skills/hydo-onboard .claude-plugin 2>/dev/null)" ]; then
  echo "build-skill-release: skills/hydo-onboard or .claude-plugin has uncommitted changes; commit them first" >&2
  exit 1
fi
cargo build --release -q -p hydo-tools --no-default-features --bin hydo-skill-pack
PACK=$TOP/target/release/hydo-skill-pack

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
LINE=$("$PACK" "$SKILL" "$OUT")
"$PACK" "$SKILL" "$TMP" >/dev/null
VERSION=${LINE%% *}
NAME=hydo-skill-$VERSION.tar.gz
cmp "$OUT/$NAME" "$TMP/$NAME"
cmp "$OUT/SHA256SUMS" "$TMP/SHA256SUMS"

PLUGIN=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' .claude-plugin/marketplace.json | head -1)
if [ "$PLUGIN" != "$VERSION" ]; then
  echo "build-skill-release: SKILL.md says $VERSION, .claude-plugin/marketplace.json says $PLUGIN; make them equal" >&2
  exit 1
fi

echo "skill $VERSION, packed twice with the same bytes:"
cat "$OUT/SHA256SUMS"
echo "assets: $OUT/$NAME $OUT/SHA256SUMS"
echo "tag:    skill-v$VERSION (on the commit $(git rev-parse --short HEAD); docs/RELEASING.md)"

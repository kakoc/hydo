#!/bin/sh
# Build the onboarding skill's archive, an asset of hydo's own GitHub release v<version>
# (Milestone 332; the skill has hydo's version since then):
#     sh hydo/dist/skill/build-skill-release.sh [OUT_DIR]
# writes OUT_DIR/hydo-skill-<version>.tar.gz (the plugin folder plugin/ as hydo/, with the skill in
# hydo/skills/onboard/; default OUT_DIR: hydo/dist/out/skill, git-ignored) and prints its line for
# the release's SHA256SUMS. hydo/dist/linux/build-release.sh and hydo/dist/mac/build-release.sh run
# it into their output folder, so each platform's build makes the same archive. The archive is made
# by hydo-skill-pack (hydo_agents::release::pack): sorted entries, every time 0, owner 0:0, modes
# 0644 or 0755, gzip without a name or time, so the same folder gives the same bytes on a Mac and
# on Linux. The script packs twice and compares, and checks that SKILL.md's metadata.version and
# hydo-min, plugin/.claude-plugin/plugin.json and the entry of .claude-plugin/marketplace.json
# all say hydo/VERSION (and HYDO_VERSION, when it is set). It tags, uploads and publishes nothing:
# docs/RELEASING.md, steps 6 and 7, and the section "The onboarding skill".
set -eu

TOP=$(cd "$(dirname "$0")/../../.." && pwd)
OUT=${1:-$TOP/hydo/dist/out/skill}
SKILL=$TOP/plugin
WANT=$(tr -d ' \n' < "$TOP/hydo/VERSION")

cd "$TOP"
if [ -n "${HYDO_VERSION:-}" ] && [ "$HYDO_VERSION" != "$WANT" ]; then
  echo "build-skill-release: HYDO_VERSION is $HYDO_VERSION, hydo/VERSION is $WANT; make them equal" >&2
  exit 1
fi
if git rev-parse --git-dir >/dev/null 2>&1 &&
  [ -n "$(git status --porcelain -- plugin .claude-plugin 2>/dev/null)" ]; then
  echo "build-skill-release: plugin or .claude-plugin has uncommitted changes; commit them first" >&2
  exit 1
fi
for f in "plugin/skills/onboard/SKILL.md:^  version: \"\(.*\)\"$" \
         "plugin/skills/onboard/SKILL.md:^  hydo-min: \"\(.*\)\"$" \
         "plugin/.claude-plugin/plugin.json:^  \"version\": \"\(.*\)\",$" \
         ".claude-plugin/marketplace.json:^ *\"version\": \"\(.*\)\",$"; do
  file=${f%%:*}
  got=$(sed -n "s/${f#*:}/\1/p" "$file" | head -1)
  if [ "$got" != "$WANT" ]; then
    echo "build-skill-release: $file says ${got:-no version} where hydo/VERSION says $WANT; the skill has hydo's version (docs/RELEASING.md)" >&2
    exit 1
  fi
done

cargo build --release --locked -q -p hydo-tools --no-default-features --bin hydo-skill-pack
PACK=${CARGO_TARGET_DIR:-$TOP/target}/release/hydo-skill-pack

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
"$PACK" "$SKILL" "$TMP/a" >/dev/null
"$PACK" "$SKILL" "$TMP/b" >/dev/null
NAME=hydo-skill-$WANT.tar.gz
cmp "$TMP/a/$NAME" "$TMP/b/$NAME"
mkdir -p "$OUT"
cp "$TMP/a/$NAME" "$OUT/$NAME"

echo "the onboarding skill $WANT, packed twice with the same bytes: $OUT/$NAME"
echo "its line for the SHA256SUMS of the release v$WANT:"
cat "$TMP/a/SHA256SUMS"

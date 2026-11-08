#!/bin/sh
# Checks that shortening AGENTS.md lost no recorded value.
#
# Usage, in the repository root:
#   git show <commit before the change>:AGENTS.md > /tmp/AGENTS.old.md
#   sh docs/tools/check-moved-values.sh /tmp/AGENTS.old.md
#
# It lists these values of the old file:
#   - every hex value of 7 to 64 digits with at least one digit and one
#     letter (hashes, fingerprints, commits);
#   - every module srcversion (23 or 24 upper-case hex digits);
#   - every number written with thousands separators (sizes, counts).
# It prints each value that no tracked file except AGENTS.md contains.
# The last line counts them. Exit status 0 means every value is still in
# the repository.
set -eu
old=$1
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
{
  grep -oE '\b[0-9a-f]{7,64}\b|\b[0-9A-F]{23,24}\b' "$old" |
    grep -E '[0-9]' | grep -E '[a-fA-F]' || true
  grep -oE '\b[0-9]{1,3}(,[0-9]{3})+\b' "$old" || true
} | sort -u > "$tmp/values"
git ls-files -z -- . ':!AGENTS.md' | xargs -0 cat 2>/dev/null > "$tmp/all" || true
total=$(wc -l < "$tmp/values" | tr -d ' ')
missing=0
while read -r v; do
  if ! grep -qF -- "$v" "$tmp/all"; then
    echo "missing: $v"
    missing=$((missing + 1))
  fi
done < "$tmp/values"
echo "values: $total, missing outside AGENTS.md: $missing"
[ "$missing" -eq 0 ]

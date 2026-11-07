#!/bin/sh
# ci/tier-filter.sh fast|full [PACKAGE...] -- the cargo-nextest filter expression of a test tier
# (ci/test-tiers.txt; docs/CI.md "Test tiers").
#   fast  every test binary except those whose tier is `full`, but with the `full` binaries of
#         the PACKAGEs named (the packages a change touches directly: their own corpora run);
#   full  every test binary (the full tier itself runs `cargo test --release --workspace`, which
#         also runs the documentation examples, which cargo-nextest does not).
#   cargo nextest run --cargo-profile ci -E "$(sh ci/tier-filter.sh fast hydo-build)"
set -eu
tier=${1:?usage: ci/tier-filter.sh fast|full [PACKAGE...]}
shift
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case $tier in
  full) echo 'all()' ;;
  fast)
    ids=$(awk -v keep=" $* " '
      !/^[[:space:]]*(#|$)/ && $2 == "full" && $1 !~ /::doctest$/ {
        pkg = $1; sub(/::.*/, "", pkg)
        if (index(keep, " " pkg " ") == 0) print $1
      }' "$here/test-tiers.txt")
    if [ -z "$ids" ]; then
      echo 'all()'
    else
      printf 'not (%s)\n' "$(printf '%s\n' $ids | sed 's/.*/binary_id(&)/' | paste -sd'|' - | sed 's/|/ | /g')"
    fi
    ;;
  *)
    echo "ci/tier-filter.sh: the tier is fast or full, not $tier" >&2
    exit 2
    ;;
esac

#!/bin/sh
# ci/tier-filter.sh fast|full [PACKAGE...] -- the cargo-nextest filter expression of a test tier
# (ci/test-tiers.txt; docs/CI.md "Test tiers").
#   fast  every test except those whose tier is `full`, but with the `full` tests of the PACKAGEs
#         named (the packages a change touches directly: their own corpora run);
#   full  every test (the full tier itself runs `cargo test --release --workspace`, which also
#         runs the documentation examples, which cargo-nextest does not).
#   cargo nextest run --cargo-profile ci -E "$(sh ci/tier-filter.sh fast hydo-build)"
# A line `<package>::<name>` names the test program tests/<name>.rs or, in a crate whose
# integration tests are one program (ci/consolidate-tests.sh), the module <name> of its program
# `it`, whose tests are `<name>::<test>`: the filter is then
# `(binary_id(<package>::it) & test(/^<name>::/))`.
set -eu
tier=${1:?usage: ci/tier-filter.sh fast|full [PACKAGE...]}
shift
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname "$here")

# whether <package>'s tests/main.rs declares the module <name>
folded() {
  for c in "$root"/hydo/crates/*/Cargo.toml; do
    d=$(dirname "$c")
    [ -f "$d/tests/main.rs" ] || continue
    p=$(sed -n 's/^name[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "$c" | head -n 1)
    [ "$p" = "$1" ] || continue
    grep -qx "mod $2;" "$d/tests/main.rs" && return 0
  done
  return 1
}

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
      exprs=""
      for id in $ids; do
        pkg=${id%%::*}
        name=${id#*::}
        if folded "$pkg" "$name"; then
          e="(binary_id($pkg::it) & test(/^$name::/))"
        else
          e="binary_id($id)"
        fi
        exprs="${exprs:+$exprs | }$e"
      done
      printf 'not (%s)\n' "$exprs"
    fi
    ;;
  *)
    echo "ci/tier-filter.sh: the tier is fast or full, not $tier" >&2
    exit 2
    ;;
esac

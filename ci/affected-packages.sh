#!/bin/sh
# ci/affected-packages.sh [--direct] BASE [HEAD] -- the workspace packages a change from BASE to
# HEAD (default HEAD) can affect (docs/CI.md, "Test tiers").
#
# Without --direct: one package name per line, or the single line `ALL`. The per-push tier runs
# the `fast` tests (ci/test-tiers.txt) of these packages; the full tier runs every test of every
# package, so whatever this selection misses is caught there within a day. The rules,
# conservative on purpose:
# - a file under a workspace member's directory (hydo/crates/<crate>/) selects that package and
#   every workspace package that depends on it, directly or not (normal, dev and build
#   dependencies, through `cargo metadata`); a file under its tests/ also selects every package
#   whose sources or tests name that crate's directory (tests that read another crate's fixtures);
# - any other file (the lock file, Cargo.toml, .cargo/, docs, examples, the skill, scripts, the
#   workflows) can be read by some test (public_content reads every tracked file), so it selects
#   ALL; so does a BASE that is not a commit of this repository (a new branch, a force push), and
#   a change with no file (a re-run tests everything).
#
# With --direct: only the packages that own a changed file, one per line (none for files outside
# the workspace members): their `full` tests run on the push too (ci/tier-filter.sh).
set -eu
direct=false
if [ "${1:-}" = --direct ]; then
  direct=true
  shift
fi
base=${1:?usage: ci/affected-packages.sh [--direct] BASE [HEAD]}
head=${2:-HEAD}
root=$(git rev-parse --show-toplevel)
cd "$root"

all() {
  $direct || echo ALL
  exit 0
}

git cat-file -e "$base^{commit}" 2>/dev/null || all
changed=$(git diff --name-only "$base" "$head")
[ -n "$changed" ] || all

meta=$(cargo metadata --format-version 1 --locked --offline 2>/dev/null ||
  cargo metadata --format-version 1 --locked)
# "<package name> <directory relative to the root>" of every workspace member
members=$(printf '%s' "$meta" | jq -r --arg root "$root/" '
  .workspace_members as $m | .packages[] | select(.id as $i | $m | any(. == $i))
  | "\(.name) \(.manifest_path | ltrimstr($root) | rtrimstr("/Cargo.toml"))"')

# the package that owns a path, and its directory (`name dir`), or nothing
owner() {
  printf '%s\n' "$members" | while read -r name d; do
    case $1 in "$d"/*) echo "$name $d" ;; esac
  done
}

selected=""
for f in $changed; do
  o=$(owner "$f")
  if [ -z "$o" ]; then
    $direct && continue
    all
  fi
  pkg=${o%% *}
  dir=${o#* }
  selected="$selected $pkg"
  $direct && continue
  case $f in
    "$dir"/tests/*)
      # packages whose sources or tests name this crate's directory
      users=$(git grep -l -F -e "$dir/" -e "../$(basename "$dir")/" -- \
        'hydo/crates/*/src' 'hydo/crates/*/tests' 2>/dev/null || true)
      for u in $users; do
        uo=$(owner "$u")
        [ -z "$uo" ] || selected="$selected ${uo%% *}"
      done
      ;;
  esac
done

if $direct; then
  printf '%s\n' $selected | sort -u
  exit 0
fi

# the selected packages and every workspace package that depends on one of them
printf '%s' "$meta" | jq -r --arg sel "$selected" '
  ($sel | split(" ") | map(select(. != ""))) as $start
  | .workspace_members as $m
  | [.packages[] | select(.id as $i | $m | any(. == $i))] as $ws
  | ($ws | map({key: .id, value: .name}) | from_entries) as $name
  | [.resolve.nodes[] | select(.id as $i | $m | any(. == $i))
     | {name: $name[.id], deps: [.deps[].pkg | $name[.] // empty]}] as $nodes
  | {set: ($start | unique), more: true}
  | until(.more | not;
      .set as $s
      | ([$nodes[] | select(any(.deps[]; . as $d | $s | any(. == $d))) | .name] + $s | unique) as $n
      | {set: $n, more: (($n | length) > ($s | length))})
  | .set[]'

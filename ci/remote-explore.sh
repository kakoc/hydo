#!/bin/sh
# Explore an example on GitHub's KVM runners (Milestone 304): start the workflow explore.yml,
# wait for it, download the merged exploration and print its problems.
#
#   sh ci/remote-explore.sh --example hydo/onboard/examples/redis [--manifest hydo.yaml]
#       [--set "KEY=VALUE ..."] [--seeds 1-40] [--shards 8] [--explore "--strategy swarm ..."]
#       [--ref REF] [--vcpus N] [--prepare "sh build.sh"] [--out DIR] [--wait MINUTES]
#       [--repo OWNER/NAME] [--workflow-ref BRANCH]
#
# It needs `gh`, logged in with the right to start workflows in the repository. The workflow
# must be on the default branch (or name a branch that has it with --workflow-ref). --ref is
# the commit or branch the runners build; it must be on GitHub (pushed). The result goes to
# --out (default hydo-out/remote-<id>): exploration.json, events.jsonl, report.md, report.html,
# summary.txt (the problems in words) and the consoles. `hydo report DIR` and
# `hydo report --html DIR` read it like a local exploration.
#
# Every wait has a time limit: --wait (default 360 minutes) for the run, 10 minutes for the run
# to appear. Exit status: 0 when the run succeeded and its result was downloaded, 1 otherwise.
set -eu

example='' manifest=hydo.yaml set='' seeds=1-40 shards=8 explore='' ref='' vcpus='' prepare=''
out='' wait=360 repo='' wref=''
while [ $# -gt 0 ]; do
  case "$1" in
    --example) example=$2; shift 2;;
    --manifest) manifest=$2; shift 2;;
    --set) set=$2; shift 2;;
    --seeds) seeds=$2; shift 2;;
    --shards) shards=$2; shift 2;;
    --explore) explore=$2; shift 2;;
    --ref) ref=$2; shift 2;;
    --vcpus) vcpus=$2; shift 2;;
    --prepare) prepare=$2; shift 2;;
    --out) out=$2; shift 2;;
    --wait) wait=$2; shift 2;;
    --repo) repo=$2; shift 2;;
    --workflow-ref) wref=$2; shift 2;;
    -h|--help) sed -n '2,18p' "$0"; exit 0;;
    *) echo "remote-explore.sh: unknown argument $1 (--help)" >&2; exit 2;;
  esac
done
[ -n "$example" ] || { echo "remote-explore.sh: --example DIR is required (--help)" >&2; exit 2; }
command -v gh > /dev/null || { echo "remote-explore.sh: gh is not installed" >&2; exit 2; }
[ -n "$repo" ] || repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
id=r$(date -u +%Y%m%d%H%M%S)-$$
[ -n "$out" ] || out=hydo-out/remote-$id

set -- -R "$repo" -f example="$example" -f manifest="$manifest" -f set="$set" -f seeds="$seeds" \
  -f shards="$shards" -f explore="$explore" -f ref="$ref" -f vcpus="$vcpus" \
  -f prepare="$prepare" -f id="$id"
[ -z "$wref" ] || set -- "$@" --ref "$wref"
gh workflow run explore.yml "$@"
echo "remote-explore.sh: started explore.yml in $repo (id $id); waiting for the run to appear"

run=
t=0
while [ -z "$run" ]; do
  [ "$t" -lt 600 ] || { echo "remote-explore.sh: the run of id $id did not appear in 10 minutes" >&2; exit 1; }
  sleep 10; t=$((t + 10))
  run=$(gh run list -R "$repo" --workflow explore.yml --event workflow_dispatch -L 30 \
    --json databaseId,displayTitle -q ".[] | select(.displayTitle | endswith(\" $id\")) | .databaseId" | head -1)
done
url=$(gh run view "$run" -R "$repo" --json url -q .url)
echo "remote-explore.sh: run $run ($url)"

limit=$((wait * 60))
t=0
while :; do
  st=$(gh run view "$run" -R "$repo" --json status,conclusion -q '.status + " " + .conclusion')
  case "$st" in completed*) break;; esac
  if [ "$t" -ge "$limit" ]; then
    echo "remote-explore.sh: run $run is still $st after $wait minutes; it goes on ($url)" >&2
    exit 1
  fi
  sleep 30; t=$((t + 30))
done
times=$(gh run view "$run" -R "$repo" --json createdAt,updatedAt -q '.createdAt + " " + .updatedAt')
echo "remote-explore.sh: run $run $st (created and last updated: $times)"

mkdir -p "$out"
if ! gh run download "$run" -R "$repo" -n "explore-$id-merged" -D "$out"; then
  echo "remote-explore.sh: run $run left no merged exploration ($url)" >&2
  exit 1
fi
echo "remote-explore.sh: the merged exploration is in $out"
cat "$out/inputs.txt" 2>/dev/null || true
echo
cat "$out/summary.txt" 2>/dev/null || true
case "$st" in "completed success") exit 0;; *) exit 1;; esac

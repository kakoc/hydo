#!/usr/bin/env bash
# action/comment.sh REPO NUMBER MARKER FILE: post FILE as the one comment of hydo on pull request
# NUMBER of REPO, or edit that comment in place when it exists (hydo guide ci, "GitHub Actions";
# docs/design/ci.md section 5.1). The comment is found again by its first line, MARKER, so a pull
# request keeps one comment per project however often it is pushed. It uses `gh api` with
# GH_TOKEN. A token that cannot write (a pull request from a fork) gives a warning and exit
# status 0: the report is still in the job summary, and the workflow_run recorder can post it.
#
# The marker is compared in the shell, never put into a jq program, so a project name cannot
# change the query. The body goes to gh from a file (-F body=@FILE), so its size and its text
# never pass through a command line.
set -euo pipefail

if [ $# -ne 4 ]; then
  echo "usage: comment.sh REPO NUMBER MARKER FILE" >&2
  exit 2
fi
repo=$1
number=$2
marker=$3
file=$4

case "$number" in
  '' | *[!0-9]*)
    echo "::error::comment.sh: the pull request number $number is not a number"
    exit 2
    ;;
esac
case "$repo" in
  */*) ;;
  *)
    echo "::error::comment.sh: $repo is not OWNER/NAME"
    exit 2
    ;;
esac
if [ ! -f "$file" ]; then
  echo "comment.sh: no $file; nothing to post"
  exit 0
fi

body="$(mktemp)"
trap 'rm -f "$body"' EXIT
{
  printf '%s\n' "$marker"
  cat "$file"
} > "$body"

# every comment's id and first line, one per line; the first whose first line is the marker
id=""
if listed="$(gh api --paginate "repos/$repo/issues/$number/comments" \
  --jq '.[] | "\(.id)\t\(.body | split("\n") | .[0] | rtrimstr("\r"))"')"; then
  while IFS=$'\t' read -r cid first; do
    if [ "$first" = "$marker" ]; then
      id="$cid"
      break
    fi
  done <<< "$listed"
else
  echo "::warning::could not list the pull request's comments; a new hydo comment is posted"
fi

if [ -n "$id" ]; then
  if gh api -X PATCH "repos/$repo/issues/comments/$id" -F "body=@$body" > /dev/null; then
    echo "comment.sh: updated comment $id on #$number"
  else
    echo "::warning::could not update the hydo comment (a pull request from a fork gets a read-only token); the report is in the job summary"
  fi
else
  if gh api -X POST "repos/$repo/issues/$number/comments" -F "body=@$body" > /dev/null; then
    echo "comment.sh: posted a comment on #$number"
  else
    echo "::warning::could not post the hydo comment (a pull request from a fork gets a read-only token); the report is in the job summary"
  fi
fi

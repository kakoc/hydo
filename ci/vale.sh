#!/bin/sh
# Checks hydo's documentation with Vale (docs/STYLE.md "How the standard is checked").
#
#   sh ci/vale.sh              check the documents listed below
#   sh ci/vale.sh FILE...      check these files instead
#   sh ci/vale.sh --list       print the documents and exit (ci/links-external.sh uses it)
#
# It downloads Vale 3.25.1 and the Google and proselint style packages once into
# ${XDG_CACHE_HOME:-~/.cache}/hydo-vale, checks every download's sha256, copies the two
# packages into .vale/styles (git-ignored), and runs Vale with .vale.ini. Every warning and
# every error fails the check (exit status 1). Suggestions are not shown; run
# `vale --minAlertLevel=suggestion FILE` with the cached program to see them.
# Runs on Linux x86_64 and on macOS (Apple Silicon); needs curl, tar, unzip and shasum or
# sha256sum.
set -eu

VALE_VERSION=3.25.1
# Vale's own release files and their sha256 (from vale_3.25.1_checksums.txt of the release).
VALE_LINUX=vale_${VALE_VERSION}_Linux_64-bit.tar.gz
VALE_LINUX_SHA256=4bf5c5176b682b1da0830f471584d3494f1c2e27ee3029e1ad6f6661d4962182
VALE_MAC=vale_${VALE_VERSION}_macOS_arm64.tar.gz
VALE_MAC_SHA256=fbcaa68d9b9e229b9c8dc918f209e93572ce943039700b7e04a423ff12f684cd
# The style packages (https://github.com/errata-ai/Google, MIT; https://github.com/errata-ai/proselint,
# BSD-3-Clause), pinned by release and sha256.
GOOGLE_URL=https://github.com/errata-ai/Google/releases/download/v0.7.1/Google.zip
GOOGLE_SHA256=4b67fca1f2a88595b2a578ab98bbf3017a23af677b04f93e862843f3ea1a9a0b
PROSELINT_URL=https://github.com/errata-ai/proselint/releases/download/v0.3.4/proselint.zip
PROSELINT_SHA256=48b1fb481ba44f6505d44346b283f6cdc74efd234f913bbdb09bf34420fab81c

# The documents the standard covers (shell patterns, expanded in the repository root). Add a
# document here once it passes.
DOCS="README.md CONTRIBUTING.md AGENTS.md docs/STYLE.md docs/ARCHITECTURE.md
  skills/hydo-docs/SKILL.md
  hydo/dist/linux/QUICKSTART-linux.md hydo/dist/mac/QUICKSTART-mac.md
  hydo/crates/*/README.md sdk/README.md sdk/*/README.md
  hydo/guide/*.md"
# Files of those patterns that Vale does not check, each with its reason:
# - hydo/guide/overview.md: the text `hydo guide` prints with no topic, laid out for a terminal
#   (aligned columns, not prose); the CLI golden pins its bytes.
EXCEPT="hydo/guide/overview.md"

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
list=
if [ "${1:-}" = "--list" ]; then list=1; shift; fi
if [ "$#" -eq 0 ]; then
  for f in $DOCS; do
    case " $EXCEPT " in *" $f "*) continue ;; esac
    set -- "$@" "$f"
  done
fi
if [ -n "$list" ]; then printf '%s\n' "$@"; exit 0; fi
cache=${XDG_CACHE_HOME:-$HOME/.cache}/hydo-vale
mkdir -p "$cache"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# fetch URL SHA256 FILE: download URL to FILE in the cache once, then check its sha256.
fetch() {
  if [ ! -f "$cache/$3" ]; then
    curl -fsSL --retry 3 -o "$cache/$3.part" "$1"
    mv "$cache/$3.part" "$cache/$3"
  fi
  got=$(sha256 "$cache/$3")
  if [ "$got" != "$2" ]; then
    echo "ci/vale.sh: $3 has sha256 $got, expected $2; deleted it" >&2
    rm -f "$cache/$3"
    exit 2
  fi
}

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) archive=$VALE_LINUX; want=$VALE_LINUX_SHA256 ;;
  Darwin-arm64) archive=$VALE_MAC; want=$VALE_MAC_SHA256 ;;
  *) echo "ci/vale.sh: no pinned Vale for $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac
fetch "https://github.com/vale-cli/vale/releases/download/v$VALE_VERSION/$archive" "$want" "$archive"
if [ ! -x "$cache/$VALE_VERSION/vale" ]; then
  mkdir -p "$cache/$VALE_VERSION"
  tar xzf "$cache/$archive" -C "$cache/$VALE_VERSION" vale
fi

fetch "$GOOGLE_URL" "$GOOGLE_SHA256" Google-0.7.1.zip
fetch "$PROSELINT_URL" "$PROSELINT_SHA256" proselint-0.3.4.zip
rm -rf "$root/.vale/styles/Google" "$root/.vale/styles/proselint"
unzip -qo "$cache/Google-0.7.1.zip" -d "$root/.vale/styles"
unzip -qo "$cache/proselint-0.3.4.zip" -d "$root/.vale/styles"

# .vale.ini sets MinAlertLevel = warning, so every line Vale prints is a warning or an error.
"$cache/$VALE_VERSION/vale" --no-exit --output=line "$@" > "$cache/out.txt"
if [ -s "$cache/out.txt" ]; then
  cat "$cache/out.txt"
  echo "ci/vale.sh: $(wc -l < "$cache/out.txt" | tr -d ' ') findings (docs/STYLE.md says how to fix or except one)" >&2
  exit 1
fi
echo "ci/vale.sh: no findings in $*"

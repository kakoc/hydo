#!/bin/sh
# Point the Homebrew formula at a published Mac release (Milestone 168):
#     sh hydo/dist/mac/update-homebrew.sh <version> <tap checkout> [--sums FILE] [--no-download]
# <tap checkout> is a clone of the tap repository https://github.com/kakoc/homebrew-tap. The
# script rewrites the `url` and `sha256` lines of its Formula/hydo.rb for hydo-mac-<version>.tar.gz
# of the release v<version>, taking the sha256 from the release's SHA256SUMS (downloaded from
# the release, or --sums FILE), and checks the published tarball against it (--no-download skips
# that). Homebrew reads the version from the url, so the formula has no `version` line. It
# commits and pushes nothing: it prints the diff and the steps that follow (docs/RELEASING.md
# step 8). HYDO_REPO_URL names another repository for the release (default the one below).
set -eu

REPO_URL=${HYDO_REPO_URL:-https://github.com/kakoc/hydo}
VERSION=
TAP=
SUMS=
DOWNLOAD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --sums) SUMS=$2; shift 2 ;;
    --no-download) DOWNLOAD=; shift ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    -*) echo "update-homebrew.sh: unknown option $1" >&2; exit 2 ;;
    *)
      if [ -z "$VERSION" ]; then VERSION=$1
      elif [ -z "$TAP" ]; then TAP=$1
      else echo "update-homebrew.sh: one version and one tap checkout, not '$1'" >&2; exit 2
      fi
      shift ;;
  esac
done
[ -n "$VERSION" ] && [ -n "$TAP" ] || { sed -n '2,3p' "$0" >&2; exit 2; }
case "$VERSION" in
  v*) echo "update-homebrew.sh: give the version without the v ($VERSION)" >&2; exit 2 ;;
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "update-homebrew.sh: '$VERSION' is not a version such as 0.1.0" >&2; exit 2 ;;
esac
FORMULA=$TAP/Formula/hydo.rb
[ -f "$FORMULA" ] || { echo "update-homebrew.sh: $FORMULA not found (give a clone of the tap repository)" >&2; exit 1; }

TARBALL=hydo-mac-$VERSION.tar.gz
BASE=$REPO_URL/releases/download/v$VERSION
URL=$BASE/$TARBALL
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ -z "$SUMS" ]; then
  SUMS=$TMP/SHA256SUMS
  curl -fsSL -o "$SUMS" "$BASE/SHA256SUMS" ||
    { echo "update-homebrew.sh: cannot download $BASE/SHA256SUMS (is v$VERSION published?)" >&2; exit 1; }
fi
SHA=$(awk -v f="$TARBALL" '$2 == f || $2 == "*" f { print $1 }' "$SUMS")
n=$(printf '%s' "$SHA" | grep -c . || true)
[ "$n" -eq 1 ] || { echo "update-homebrew.sh: $SUMS lists $TARBALL $n times, expected once" >&2; exit 1; }
case "$SHA" in
  *[!0-9a-f]*) echo "update-homebrew.sh: $SUMS lists a bad sha256 for $TARBALL: $SHA" >&2; exit 1 ;;
esac
[ ${#SHA} -eq 64 ] || { echo "update-homebrew.sh: $SUMS lists a bad sha256 for $TARBALL: $SHA" >&2; exit 1; }

if [ -n "$DOWNLOAD" ]; then
  curl -fsSL -o "$TMP/$TARBALL" "$URL" ||
    { echo "update-homebrew.sh: cannot download $URL" >&2; exit 1; }
  GOT=$(shasum -a 256 "$TMP/$TARBALL" | awk '{ print $1 }')
  [ "$GOT" = "$SHA" ] ||
    { echo "update-homebrew.sh: $URL has sha256 $GOT, but SHA256SUMS lists $SHA" >&2; exit 1; }
  echo "update-homebrew.sh: $URL matches SHA256SUMS ($SHA)"
fi

# the formula's own url and sha256 lines (two-space indent: the class body, not a resource)
for key in url sha256; do
  n=$(grep -c "^  $key \"" "$FORMULA" || true)
  [ "$n" -eq 1 ] || { echo "update-homebrew.sh: $FORMULA has $n '  $key \"…\"' lines, expected one" >&2; exit 1; }
done
if grep -q '^  version "' "$FORMULA"; then
  echo "update-homebrew.sh: $FORMULA has a version line; Homebrew reads the version from the url (brew audit --strict refuses it)" >&2
  exit 1
fi
awk -v url="$URL" -v sha="$SHA" '
  /^  url "/    { print "  url \"" url "\""; next }
  /^  sha256 "/ { print "  sha256 \"" sha "\""; next }
  { print }
' "$FORMULA" > "$TMP/hydo.rb"
if cmp -s "$TMP/hydo.rb" "$FORMULA"; then
  echo "update-homebrew.sh: $FORMULA already names hydo $VERSION ($SHA); nothing to change"
  exit 0
fi
cp "$FORMULA" "$TMP/before.rb"
cat "$TMP/hydo.rb" > "$FORMULA"
diff -u "$TMP/before.rb" "$FORMULA" || true
cat <<EOF

update-homebrew.sh: $FORMULA now installs hydo $VERSION. Next (docs/RELEASING.md step 8):
  cd $TAP && brew style Formula/hydo.rb
  git commit -am "hydo $VERSION" && git push
  brew update && brew upgrade kakoc/tap/hydo (or brew install kakoc/tap/hydo), hydo doctor,
  brew test hydo, brew audit --strict --online kakoc/tap/hydo
EOF

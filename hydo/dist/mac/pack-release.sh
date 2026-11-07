#!/bin/sh
# Pack a Mac release tree into its tarball, the same bytes from the same files:
#     hydo/dist/mac/pack-release.sh <OUTDIR>/hydo-mac-<version>   -> <OUTDIR>/hydo-mac-<version>.tar.gz
#
# It writes the tree's SHA256SUMS (every other file, sorted), removes extended attributes (a
# downloaded or copied file carries com.apple.* ones), sets every file's time to 0 and writes the
# files in a sorted order with no owner, extended attributes or ACLs, with gzip writing no name or
# time. build-release.sh packs the unsigned build with it, and attach-signatures.sh packs the same
# tree with the Developer ID signatures attached (docs/design/signing.md), so the published signed
# tarball is reproducible from the unsigned build and the signatures asset.
set -e
umask 022
REL=$(cd "$1" && pwd)
OUTDIR=$(dirname "$REL")
NAME=$(basename "$REL")
case "$NAME" in hydo-mac-*) ;; *) echo "pack-release.sh: $REL is not a hydo-mac-<version> tree" >&2; exit 2 ;; esac
( cd "$REL" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | xargs shasum -a 256 > SHA256SUMS )
xattr -cr "$REL" 2>/dev/null || true
find "$REL" -exec env TZ=UTC touch -h -t 197001010000 {} +
( cd "$OUTDIR" && find "$NAME" | LC_ALL=C sort > ".$NAME.list" \
  && COPYFILE_DISABLE=1 tar --uid 0 --gid 0 --uname root --gname wheel --no-xattrs --no-acls \
       --no-fflags --no-mac-metadata --format gnutar -n -T ".$NAME.list" -cf - | gzip -n -9 > "$NAME.tar.gz" \
  && rm ".$NAME.list" )
ls -la "$OUTDIR/$NAME.tar.gz"
shasum -a 256 "$OUTDIR/$NAME.tar.gz"

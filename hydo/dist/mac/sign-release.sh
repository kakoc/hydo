#!/bin/sh
# Sign and notarize the programs of an unsigned Mac release build, and write the signatures asset
# (docs/design/signing.md, docs/RELEASING.md; Milestone 172). Only the holder of the Developer ID
# key runs this, once per release:
#
#     hydo/dist/mac/sign-release.sh <unsigned hydo-mac-<v>.tar.gz> <OUTDIR>
#         -> OUTDIR/hydo-mac-<v>-signatures.tar.gz   the signatures asset (published with the release)
#            OUTDIR/signed/hydo-mac-<v>.tar.gz       the signed tarball (published as hydo-mac-<v>.tar.gz)
#            OUTDIR/notary-submit.json, notary-log.json   Apple's answer and log for the submission
#
# Every Mach-O program in bin/ (hydo, hydo-build, hydo-fetch-image, hydo-hvm) is signed with
# codesign: the Developer ID identity $HYDO_SIGN_IDENTITY (the keychain must hold its key), the
# hardened runtime (--options runtime), a secure timestamp (--timestamp, from Apple's time-stamp
# service), and for bin/hydo-hvm the hypervisor entitlement of hv/hvm/hv.entitlements. The four
# signed programs are submitted together in one zip to Apple's notary service (notarytool, the
# keychain profile $HYDO_NOTARY_PROFILE, waiting at most $HYDO_NOTARY_TIMEOUT); the submission must
# be Accepted. A notarization ticket cannot be stapled to a plain command-line program (stapler
# staples apps, disk images and installer packages only): Gatekeeper looks the ticket up online by
# the program's code-directory hash, which the signature carries. Then each signature is taken
# out of its program (hydo-release signatures extract, which checks that attaching it to the
# unsigned program gives the signed program byte for byte) into the signatures asset, and the
# signed tarball is made from the unsigned one with attach-signatures.sh, as anyone can repeat.
#
# HYDO_NOTARIZE=0 skips the notarization (a trial; such signatures must not be published).
set -e
umask 022
[ $# -eq 2 ] || { echo "usage: $0 <unsigned hydo-mac-<v>.tar.gz> <OUTDIR>" >&2; exit 2; }
ID=${HYDO_SIGN_IDENTITY:-"Developer ID Application: Konstantin Matsiushonak (5C3JA2N5X6)"}
PROFILE=${HYDO_NOTARY_PROFILE:-hydo-notary}
TIMEOUT=${HYDO_NOTARY_TIMEOUT:-3h}
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
U=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
NAME=$(basename "$U" .tar.gz)
case "$NAME" in hydo-mac-*) ;; *) echo "$U is not a hydo-mac-<version>.tar.gz" >&2; exit 2 ;; esac
( cd "$TOP" && cargo build --release --locked -q -p hydo-release ) >&2
HR=$TOP/target/release/hydo-release
W=$OUT/work
[ -d "$W" ] && chmod -R u+w "$W" && rm -rf "$W"
mkdir -p "$W/unsigned" "$W/signed"
tar -xzf "$U" -C "$W/unsigned"
tar -xzf "$U" -C "$W/signed"
T=$W/signed/$NAME

# 1. sign
for p in "$T"/bin/*; do
  file "$p" | grep -q 'Mach-O 64-bit executable' || continue
  if [ "$(basename "$p")" = hydo-hvm ]; then
    codesign --force --options runtime --timestamp --entitlements "$T/hv/hvm/hv.entitlements" -s "$ID" "$p"
  else
    codesign --force --options runtime --timestamp -s "$ID" "$p"
  fi
  codesign --verify --strict --verbose=2 "$p"
done
codesign -d --entitlements - "$T/bin/hydo-hvm" 2>/dev/null | grep -q com.apple.security.hypervisor \
  || { echo "bin/hydo-hvm lacks the hypervisor entitlement after signing" >&2; exit 1; }

# 2. notarize
if [ "${HYDO_NOTARIZE:-1}" != 0 ]; then
  Z=$W/$NAME-programs.zip
  ( cd "$T" && ditto -c -k --keepParent bin "$Z" )
  echo "submitting $(basename "$Z") to the notary service (waiting at most $TIMEOUT)"
  xcrun notarytool submit "$Z" --keychain-profile "$PROFILE" --wait --timeout "$TIMEOUT" \
    --output-format json > "$OUT/notary-submit.json" || true
  cat "$OUT/notary-submit.json"; echo
  sid=$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$OUT/notary-submit.json")
  [ -n "$sid" ] && xcrun notarytool log "$sid" --keychain-profile "$PROFILE" "$OUT/notary-log.json" || true
  grep -q '"status" *: *"Accepted"' "$OUT/notary-submit.json" \
    || { echo "the notarization was not accepted (see $OUT/notary-submit.json and notary-log.json)" >&2; exit 1; }
fi

# 3. the signatures asset, and the signed tarball made from it as anyone would
"$HR" signatures extract "$W/unsigned/$NAME" "$T" "$OUT/$NAME-signatures.tar.gz"
HYDO_RELEASE_BIN=$HR sh "$D/attach-signatures.sh" "$U" "$OUT/$NAME-signatures.tar.gz" "$OUT/signed"
for p in "$T"/bin/*; do
  cmp "$p" "$OUT/signed/$NAME/bin/$(basename "$p")"
done
echo "signed: $OUT/signed/$NAME.tar.gz; signatures: $OUT/$NAME-signatures.tar.gz"

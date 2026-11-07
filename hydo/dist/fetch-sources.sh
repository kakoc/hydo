#!/bin/sh
# Fetch the source archives a release's sources archive includes (Milestone 158; docs/RELEASING.md):
#     sh hydo/dist/fetch-sources.sh <list> <outdir>
# <list> is what `hydo-release fetch-list <linux|mac>` prints: one line per file,
#     <file> TAB <sha256> TAB <from> TAB <url>
# and every file lands in <outdir>/<file> with exactly that sha256 (a file already there with it is
# kept). Runs on a Linux machine with Nix (the bare-metal machine has it since M140; `nix` and
# `nix-store` on PATH, cache.nixos.org as substituter) and curl, GNU tar, gzip and jq. <from>:
#   url                          download <url>
#   nix:<system>:<attr>:<name>   a fixed-output input named <name> of the derivation of <attr> in
#                                the pinned nixpkgs (the nix-pack line's store path), realised by
#                                Nix (from cache.nixos.org or upstream); a directory is packed
#   nix-cache:<store path>       a file output read from cache.nixos.org
#   nix-pack:<store path>        a store directory (the nixpkgs tree), realised and packed
# Directories are packed as <file> (a .tar.gz whose top directory is <file> without .tar.gz):
# GNU tar, sorted, time 1, owner 0:0, gzip -n -9, so the bytes depend only on the contents.
set -e
LIST=$1; OUT=$2
[ -f "$LIST" ] && [ -n "$OUT" ] || { echo "usage: fetch-sources.sh <list> <outdir>" >&2; exit 2; }
mkdir -p "$OUT"
NIXPKGS=$(awk -F'\t' '$3 ~ /^nix-pack:/ {sub(/^nix-pack:/, "", $3); print $3; exit}' "$LIST")
pack() {   # pack <store dir> <file>
  stem=${2%.tar.gz}
  tar --sort=name --format=gnu --mtime=@1 --owner=0 --group=0 --numeric-owner \
      -C "$(dirname "$1")" --transform "s#^$(basename "$1")#$stem#" -cf - "$(basename "$1")" | gzip -n -9 > "$OUT/$2.part"
}
ok() { [ -f "$OUT/$1" ] && [ "$(sha256sum "$OUT/$1" | cut -d' ' -f1)" = "$2" ]; }
take() {   # take <file> <sha256>: keep $OUT/<file>.part when its sha256 is right
  if [ -f "$OUT/$1.part" ] && [ "$(sha256sum "$OUT/$1.part" | cut -d' ' -f1)" = "$2" ]; then
    mv "$OUT/$1.part" "$OUT/$1"; chmod 644 "$OUT/$1"; return 0
  fi
  rm -f "$OUT/$1.part"; return 1
}
failed=0
while IFS="$(printf '\t')" read -r file sha from url; do
  [ -n "$file" ] || continue
  ok "$file" "$sha" && continue
  case "$from" in
    url)
      curl -sSLf -o "$OUT/$file.part" "$url" || true
      take "$file" "$sha" || { echo "fetch-sources: $file from $url: not the pinned sha256" >&2; failed=1; } ;;
    nix-cache:*)
      nix store cat --store https://cache.nixos.org "${from#nix-cache:}" > "$OUT/$file.part" || true
      take "$file" "$sha" || { echo "fetch-sources: $file from ${from#nix-cache:}: not the pinned sha256" >&2; failed=1; } ;;
    nix-pack:*)
      p=${from#nix-pack:}; nix-store -r "$p" > /dev/null
      pack "$p" "$file"
      take "$file" "$sha" || { echo "fetch-sources: $file packed from $p: not the pinned sha256" >&2; failed=1; } ;;
    nix:*)
      rest=${from#nix:}; sys=${rest%%:*}; rest=${rest#*:}; attr=${rest%%:*}; name=${rest#*:}
      drv=$(nix-instantiate -E "(import $NIXPKGS { system = \"$sys\"; }).$attr" 2>/dev/null | head -1 | sed 's/!.*//')
      got=1
      for i in $(nix derivation show "$drv^*" 2>/dev/null | jq -r '.[].inputDrvs | keys[]'); do
        nix derivation show "$i^*" 2>/dev/null | jq -e --arg n "$name" '.[] | select(.name == $n and .outputs.out.hash != null)' > /dev/null || continue
        o=$(nix-store -r "$i" 2>/dev/null | tail -1)
        if [ -d "$o" ]; then pack "$o" "$file"; else cp "$o" "$OUT/$file.part"; fi
        if take "$file" "$sha"; then got=0; break; fi
      done
      [ $got = 0 ] || { echo "fetch-sources: $file: no input $name of $attr ($sys) has the pinned sha256" >&2; failed=1; } ;;
    *) echo "fetch-sources: $file: unknown origin $from" >&2; failed=1 ;;
  esac
done < "$LIST"
[ $failed = 0 ] || exit 1
echo "fetch-sources: $(wc -l < "$LIST" | tr -d ' ') files in $OUT"

#!/bin/sh
# Fetch the source archives a release's sources archive includes (Milestone 158; docs/RELEASING.md):
#     sh hydo/dist/fetch-sources.sh <list> <outdir>
# <list> is what `hydo-release fetch-list <linux|mac>` prints: one line per file,
#     <file> TAB <sha256> TAB <from> TAB <url>
# and every file lands in <outdir>/<file> with exactly that sha256 (a file already there with it is
# kept). Runs on a Linux machine with Nix (the bare-metal machine has it since M140; `nix` and
# `nix-store` on PATH, cache.nixos.org as substituter) and curl, GNU tar, gzip and jq. <from>:
#   url                          download <url>
#   nix-in:<tree>:<system>:<attr>:<name>
#                                a fixed-output input named <name> of the derivation of <attr> in
#                                the nixpkgs tree <tree> (a store path), realised by Nix (from
#                                cache.nixos.org or upstream); a directory is packed
#   nix:<system>:<attr>:<name>   the same in the list's pinned nixpkgs (fetch-list writes these
#                                as nix-in: lines; an older list's first nix-pack line names it)
#   nix-cache:<store path>       a file output read from cache.nixos.org
#   nix-pack:<store path>        a store directory (a nixpkgs tree), realised and packed; a tree this
#                                store lacks is fetched from <url> with builtins.fetchTarball, which
#                                must give exactly that store path
#   nix-recipe:<tree>:<system>:<attr>
#                                the derivation closure of <attr> in the nixpkgs tree <tree>
#                                (Milestone 158 addendum): `nix derivation show -r` as
#                                derivations.json, every .drv file under drv/ and every
#                                non-derivation input (builder scripts, setup hooks, patches and
#                                files of the nixpkgs tree) under inputs/, packed
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
ok() { [ "$2" != - ] && [ -f "$OUT/$1" ] && [ "$(sha256sum "$OUT/$1" | cut -d' ' -f1)" = "$2" ]; }
take() {   # take <file> <sha256>: keep $OUT/<file>.part when its sha256 is right ("-": any, printed)
  if [ "$2" = - ] && [ -f "$OUT/$1.part" ]; then
    mv "$OUT/$1.part" "$OUT/$1"; chmod 644 "$OUT/$1"
    echo "fetch-sources: $1 has sha256 $(sha256sum "$OUT/$1" | cut -d' ' -f1) (not pinned yet: pin it in third-party.json)" >&2; return 0
  fi
  if [ -f "$OUT/$1.part" ] && [ "$(sha256sum "$OUT/$1.part" | cut -d' ' -f1)" = "$2" ]; then
    mv "$OUT/$1.part" "$OUT/$1"; chmod 644 "$OUT/$1"; return 0
  fi
  rm -f "$OUT/$1.part"; return 1
}
tree() {   # tree <store path> [<url>]: realise a source tree, or fetch it with fetchTarball
  nix-store -r "$1" > /dev/null 2>&1 && return 0
  case "$2" in
    http*.tar.gz) [ "$(nix-instantiate --eval -E "builtins.fetchTarball \"$2\"" 2>/dev/null | tr -d '"')" = "$1" ] ;;
    *) return 1 ;;
  esac
}
instantiate() {   # instantiate <tree> <system> <attr>: the derivation's path
  nix-instantiate -E "(import $1 { system = \"$2\"; }).$3" 2>/dev/null | head -1 | sed 's/!.*//'
}
fod() {   # fod <tree> <system> <attr> <name> <file> <sha256>
  drv=$(instantiate "$1" "$2" "$3")
  for i in $(nix derivation show "$drv^*" 2>/dev/null | jq -r '.[].inputDrvs | keys[]'); do
    nix derivation show "$i^*" 2>/dev/null | jq -e --arg n "$4" '.[] | select(.name == $n and .outputs.out.hash != null)' > /dev/null || continue
    o=$(nix-store -r "$i" 2>/dev/null | tail -1)
    if [ -d "$o" ]; then pack "$o" "$5"; else cp "$o" "$OUT/$5.part"; fi
    take "$5" "$6" && return 0
  done
  echo "fetch-sources: $5: no input $4 of $3 ($2) in $1 has the pinned sha256" >&2; return 1
}
recipe() {   # recipe <tree> <system> <attr> <file> <sha256>
  drv=$(instantiate "$1" "$2" "$3")
  [ -n "$drv" ] || { echo "fetch-sources: $4: $3 ($2) does not evaluate in $1" >&2; return 1; }
  w=$(mktemp -d); d=$w/${4%.tar.gz}
  mkdir -p "$d/drv" "$d/inputs"; chmod 755 "$d" "$d/drv" "$d/inputs"
  nix derivation show -r "$drv^*" > "$d/derivations.json"; chmod 644 "$d/derivations.json"
  for p in $(nix-store -qR "$drv"); do
    case "$p" in
      *.drv) cp --preserve=mode "$p" "$d/drv/" ;;
      *) cp -RP --preserve=mode "$p" "$d/inputs/" ;;
    esac
  done
  pack "$d" "$4"; chmod -R u+w "$w"; rm -rf "$w"
  take "$4" "$5" || { echo "fetch-sources: $4: the derivation closure of $3 ($2, $drv) is not the pinned sha256" >&2; return 1; }
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
      p=${from#nix-pack:}
      if tree "$p" "$url"; then
        pack "$p" "$file"
        take "$file" "$sha" || { echo "fetch-sources: $file packed from $p: not the pinned sha256" >&2; failed=1; }
      else
        echo "fetch-sources: $file: $p is not in this store and $url does not give it" >&2; failed=1
      fi ;;
    nix-in:*)
      rest=${from#nix-in:}; t=${rest%%:*}; rest=${rest#*:}; sys=${rest%%:*}; rest=${rest#*:}; attr=${rest%%:*}; name=${rest#*:}
      if tree "$t"; then fod "$t" "$sys" "$attr" "$name" "$file" "$sha" || failed=1
      else echo "fetch-sources: $file: the nixpkgs tree $t is not in this store (fetch its archive first)" >&2; failed=1; fi ;;
    nix:*)
      rest=${from#nix:}; sys=${rest%%:*}; rest=${rest#*:}; attr=${rest%%:*}; name=${rest#*:}
      fod "$NIXPKGS" "$sys" "$attr" "$name" "$file" "$sha" || failed=1 ;;
    nix-recipe:*)
      rest=${from#nix-recipe:}; t=${rest%%:*}; rest=${rest#*:}; sys=${rest%%:*}; attr=${rest#*:}
      if tree "$t"; then recipe "$t" "$sys" "$attr" "$file" "$sha" || failed=1
      else echo "fetch-sources: $file: the nixpkgs tree $t is not in this store (fetch its archive first)" >&2; failed=1; fi ;;
    *) echo "fetch-sources: $file: unknown origin $from" >&2; failed=1 ;;
  esac
done < "$LIST"
[ $failed = 0 ] || exit 1
echo "fetch-sources: $(wc -l < "$LIST" | tr -d ' ') files in $OUT"

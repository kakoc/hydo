#!/bin/sh
# pconf.sh <base file> <override>...: print the properties file <base> with every key of the
# overrides set. An override is a file of key=value lines (comments and blank lines are left out)
# or one key=value argument. A key that the base has is replaced in place; a new key is added at
# the end. Pulsar and BookKeeper read the first of two equal keys, so a key is never written twice.
# The image's own configuration files stay as they are; the result goes to standard output.
set -eu
base=$1
shift
tmp=$(mktemp)
cp "$base" "$tmp"
for o in "$@"; do
  if [ -f "$o" ]; then
    grep -v '^[[:space:]]*#' "$o" | grep '=' || true
  else
    printf '%s\n' "$o"
  fi
done > "$tmp.over"
while IFS= read -r kv; do
  [ -n "$kv" ] || continue
  k=${kv%%=*}
  awk -v k="$k" -v kv="$kv" 'BEGIN { done = 0 }
    index($0, k "=") == 1 { if (!done) print kv; done = 1; next }
    { print }
    END { if (!done) print kv }' "$tmp" > "$tmp.n"
  mv "$tmp.n" "$tmp"
done < "$tmp.over"
cat "$tmp"
rm -f "$tmp" "$tmp.over"

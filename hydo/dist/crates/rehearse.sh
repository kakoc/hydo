#!/usr/bin/env bash
# A dry run of publishing the hydo command to crates.io and of a user's `cargo install hydo`
# (docs/PUBLISHING.md "Publishing the hydo command"; Milestone 169). Nothing is uploaded.
#
#   hydo/dist/crates/rehearse.sh [WORK]          (default WORK: target/crates-rehearsal)
#
# 1. `cargo package` of every crate of docs/PUBLISHING.md's order in one call: cargo packs them,
#    puts the packed crates into a temporary registry and builds each one from its .crate file
#    against the packed others (what `cargo publish` checks, one crate after the other).
# 2. A stand-in for crates.io: a directory source with the packed crates (unpacked, with the
#    sha256 of their .crate file, as crates.io would serve them) and every third-party crate of
#    the workspace's Cargo.lock (`cargo vendor`).
# 3. `cargo install hydo --locked --offline` in WORK, with a fresh Cargo home whose only registry
#    is that directory: no repository file is read, and the Cargo.lock inside the packed `hydo`
#    decides every version, as for a user.
# 4. The installed `hydo`: --version, guide, init and doctor (doctor fails until `hydo setup
#    --get` has fetched the version's files; its exit status is printed, not checked).
# 5. With HYDO_REHEARSAL_LINUX=1 also `cargo check --target x86_64-unknown-linux-musl` of the
#    packed `hydo` (the Linux-only code, which a Mac build does not compile).
#
# HYDO_REHEARSAL_DIRTY=1 packs an uncommitted tree (`--allow-dirty`; the packages then record
# no commit). HYDO_REHEARSAL_TOOLCHAIN names the toolchain of the install (default: the one
# rust-toolchain.toml pins, which is also every crate's rust-version).
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="${1:-$TOP/target/crates-rehearsal}"
mkdir -p "$WORK"
WORK="$(cd "$WORK" && pwd)"
CRATES=$(sed -n 's/^ *cargo publish --registry crates-io -p \([a-z0-9-]*\)$/\1/p' "$TOP/docs/PUBLISHING.md")
[ -n "$CRATES" ] || { echo "error: no 'cargo publish --registry crates-io -p <crate>' lines in docs/PUBLISHING.md" >&2; exit 1; }
TOOLCHAIN="${HYDO_REHEARSAL_TOOLCHAIN:-$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$TOP/rust-toolchain.toml")}"
DIRTY=()
[ "${HYDO_REHEARSAL_DIRTY:-0}" = 1 ] && DIRTY=(--allow-dirty)
PKGS=()
for c in $CRATES; do PKGS+=(-p "$c"); done

echo "== 1. cargo package (${#PKGS[@]} arguments: $(echo $CRATES | wc -w | tr -d ' ') crates)"
rm -rf "$TOP/target/package"
( cd "$TOP" && cargo package --locked "${DIRTY[@]}" "${PKGS[@]}" )

echo "== 2. the stand-in registry: $WORK/registry"
rm -rf "$WORK/registry" "$WORK/home" "$WORK/install" "$WORK/target" "$WORK/proj"
( cd "$TOP" && cargo vendor --locked --versioned-dirs --quiet "$WORK/registry" >/dev/null )
for c in $CRATES; do
  f=$(ls "$TOP/target/package/$c"-[0-9]*.crate)
  base=$(basename "$f" .crate)
  sum=$(shasum -a 256 "$f" | cut -d' ' -f1)
  tar xzf "$f" -C "$WORK/registry"
  printf '{"files":{},"package":"%s"}' "$sum" > "$WORK/registry/$base/.cargo-checksum.json"
  printf '%s  %s\n' "$sum" "$base.crate"
done
mkdir -p "$WORK/home"
cat > "$WORK/home/config.toml" <<EOF
[source.crates-io]
replace-with = "rehearsal"
[source.rehearsal]
directory = "$WORK/registry"
EOF

echo "== 3. cargo install hydo --locked --offline (toolchain $TOOLCHAIN, Cargo home $WORK/home)"
# the toolchain's own cargo and rustc by path: rustup finds rustc through the Cargo home's bin/,
# which the fresh Cargo home does not have
if command -v rustup > /dev/null; then
  CARGO_BIN="$(cd "$WORK" && rustup which --toolchain "$TOOLCHAIN" cargo)"
  RUSTC_BIN="$(cd "$WORK" && rustup which --toolchain "$TOOLCHAIN" rustc)"
else
  CARGO_BIN="$(command -v cargo)"
  RUSTC_BIN="$(command -v rustc)"
fi
"$RUSTC_BIN" --version
( cd "$WORK" && CARGO_HOME="$WORK/home" RUSTC="$RUSTC_BIN" \
    "$CARGO_BIN" install hydo --locked --offline --root "$WORK/install" --target-dir "$WORK/target" )
H="$WORK/install/bin/hydo"
ls -l "$WORK/install/bin"

echo "== 4. the installed hydo"
"$H" --version
"$H" guide | head -3
mkdir -p "$WORK/proj"
"$H" init --dir "$WORK/proj" --name demo
ls "$WORK/proj/hydo"
rc=0
"$H" doctor || rc=$?
echo "hydo doctor: exit status $rc"

if [ "${HYDO_REHEARSAL_LINUX:-0}" = 1 ]; then
  echo "== 5. cargo check --target x86_64-unknown-linux-musl of the packed hydo"
  ( cd "$WORK" && CARGO_HOME="$WORK/home" RUSTC="$RUSTC_BIN" \
      "$CARGO_BIN" check --locked --offline --target x86_64-unknown-linux-musl \
      --manifest-path "$WORK/registry/hydo-$("$H" --version | cut -d' ' -f2)/Cargo.toml" \
      --target-dir "$WORK/target" )
fi
echo "rehearsal done: $WORK"

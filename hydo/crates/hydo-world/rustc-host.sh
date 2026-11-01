#!/bin/sh
# The rustc that hydo/crates/hydo-world/build.sh gives Cargo. Cargo mixes the `host:` line of
# `rustc -vV` into the identity (-C metadata) of every crate built for the host -- the build
# scripts -- and so into every crate that depends on one; rustc derives symbol hashes and type ids
# from those identities, so a Mac and a Linux machine would compile the same static musl binary
# into different bytes. This answers `-vV` with the host line fixed, and runs the real rustc
# ($HYDO_REAL_RUSTC) for everything else; the build scripts still run on the real host.
if [ "$#" = 1 ] && [ "$1" = -vV ]; then
  "$HYDO_REAL_RUSTC" -vV | sed 's/^host: .*/host: x86_64-unknown-linux-gnu/'
  exit
fi
exec "$HYDO_REAL_RUSTC" "$@"

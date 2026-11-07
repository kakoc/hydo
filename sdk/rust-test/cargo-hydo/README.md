# cargo-hydo

`cargo hydo` builds a [`hydo-test`](https://crates.io/crates/hydo-test) program twice (for the
host, to write hydo's plan; for the guest, as a static musl binary, to run the drivers and
checks) and calls the installed `hydo` command of hydo, a deterministic hypervisor
(https://github.com/kakoc/hydo).

```sh
cargo install cargo-hydo
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo hydo plan  [--bin NAME] [--arch x86_64|aarch64]   # .hydo/plan.json and .hydo/guest/<bin>
cargo hydo build                                        # plan, then `hydo build .hydo/plan.json --out .hydo`
cargo hydo run "disk=none | run"                        # plan + build, then `hydo run ...` (also explore, repro, save, report)
```

The guest binary is linked with Rust's self-contained musl and rust-lld (no C toolchain is
needed) and host paths are remapped, so a pinned toolchain gives the same bytes on every build.
`--arch` defaults to aarch64 on macOS (the Mac backend) and x86_64 elsewhere; `HYDO` names the
engine command (default `hydo` on PATH). The `hydo-test` README, section "cargo hydo", has the
details.

License: Apache-2.0.

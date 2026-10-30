//! `hydo-build <hydo.yaml> [--set KEY=VALUE ...] [--out NAME] [--build-dir DIR]
//! [--arch x86_64|arm64] [--check] [--print-shell] [--print-init] [--print-world]`: the same
//! command line as hydo/onboard/hydo-build, with the repository's (or a release's) onboard/
//! directory and the host's tools.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let ctx = hydo_build::Ctx::standalone();
    std::process::exit(hydo_build::main(&ctx, &argv));
}

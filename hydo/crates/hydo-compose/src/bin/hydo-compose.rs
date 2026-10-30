//! `hydo-compose <docker-compose.yaml> [--out hydo.yaml] ...`: the stand-alone form of
//! `hydo compose` (src/lib.rs; MANIFEST.md, "From Docker Compose").

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(hydo_compose::main(&argv));
}

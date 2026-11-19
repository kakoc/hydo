//! `hydo-compose <docker-compose.yaml> [--out hydo.yaml] ...`: the stand-alone form of
//! `hydo compose` (src/lib.rs; MANIFEST.md, "From Docker Compose").

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cfg = hydo_config::Config::process();
    // a private registry's credential, where Docker keeps it (Milestone 320)
    hydo_compose::registry::set_docker_config(cfg.tools.docker_config.clone());
    std::process::exit(hydo_compose::main_clock(&argv, cfg.cli.init_clock_start));
}

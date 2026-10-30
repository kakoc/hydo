//! `hydo-hist-check`: the history checks of one run from its saved consoles (the command line of
//! hydo/history/hydo-check; also `hydo check-history`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(hydo_history::cli::main(&argv));
}

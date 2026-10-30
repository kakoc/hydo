//! `hydo` -- deterministic fault testing: one command over both engines (hvx on x86_64 Linux,
//! hvm on Apple Silicon). It replaces the two Python release CLIs (hydo/dist/linux/hydo and
//! hydo/dist/mac/hydo before M101) with the same commands, flags, output and exit codes; where
//! the two differed, each platform keeps its own behaviour (docs/HISTORY.md, M101).
//!
//! How each command is implemented today:
//!   Rust:                 run, repro (incl. --regressions), save, report, doctor, module (Linux),
//!                         compose (hydo-compose), guide/skill/init (hydo-agents)
//!   Rust + Python seams:  run/repro call hydo-history (Rust) and hydo_errors.for_run_dir,
//!                         --regressions and save call hydo_composer (src/bridge.rs)
//!   delegated (Python):   build (hydo-build), explore (explore.py), errors (hydo_errors.py)

mod bridge;
mod ctx;
mod delegated;
mod doctor;
mod hvxenv;
mod module;
mod pyjson;
mod runs;

use ctx::{Ctx, Platform};

const HELP_LINUX: &str = r#"hydo -- deterministic fault testing on an x86_64 Linux machine with VT-x (the hvx backend).

    hydo init    [--dir DIR] [--name NAME]         start a repository's hydo/ folder and its AGENTS.md section
    hydo guide   [TOPIC | --list]                  print the onboarding guide for any AI tool (the skill's SKILL.md)
    hydo skill   install --agent claude|codex|cursor|copilot|gemini|all [--user] | --to DIR
                                                   copy the onboarding skill where an AI tool reads skills
    hydo doctor                                    check this machine and the installation
    hydo module build|load|unload|status [--name NAME] [--dry-run]
                                                   build and (un)load the hvx kernel module
    hydo module udev install|remove|status [--name NAME] [--group GROUP] [--dry-run]
                                                   optional: give /dev/hvx to a group, so runs need no sudo
    hydo compose <docker-compose.yaml> [--out hydo.yaml] [hydo-compose options]
                                                   start a hydo.yaml from a Compose file (MANIFEST.md)
    hydo build   <hydo.yaml> [--out DIR] [--set KEY=VALUE ...]
                                                   compile the manifest into a guest disk image
    hydo run     [IMG] "<schedule>" [--image IMG]  run one schedule from a fresh boot
    hydo repro   <run dir | explore dir | "<schedule>"> [--image IMG]
                                                   run again and compare the fingerprints
    hydo repro   --regressions [IMG]               replay the plan's saved regressions (hydo/regressions/)
    hydo save    <explore dir> [--to hydo/regressions]
                                                   save the minimal schedules of an exploration as regressions
    hydo explore [IMG] [--seeds 1-20] [--strategy random|swarm|process|...] [--workers N]
                 [--out DIR] [any other explore.py option]
                                                   generate, run, re-run and shrink schedules
    hydo report  [DIR]                             print the report of the last (or a given) exploration
    hydo errors  list [DIR] | accept SIG... --reason TEXT [--with F] [--during F] | check
                                                   error logs as findings: list new error kinds, accept
                                                   expected ones into hydo/known-errors.yaml (MANIFEST.md)

IMG defaults to the one *-disk.img in ./hydo-out, or in ./.hydo for a project whose manifest is
hydo/hydo.yaml (the agreed repository layout; `hydo build` with no manifest uses ./hydo/hydo.yaml).
Runs open /dev/hvx ($HVX_DEV) while holding the shared module lock; `hydo module load/unload` take it
exclusively, so a reload never pulls the module from under a running VM. The lock file is
$HYDO_LOCK (default ~/.hvx-module.lock). A run uses sudo only when you cannot open the device
yourself (see `hydo module udev`; HYDO_SUDO=1 forces sudo). Plain Python 3.9+, no packages.

"#;

const HELP_MAC: &str = r#"hydo -- deterministic fault testing on an Apple Silicon Mac (the hvm backend).

    hydo init    [--dir DIR] [--name NAME]        start a repository's hydo/ folder and its AGENTS.md section
    hydo guide   [TOPIC | --list]                 print the onboarding guide for any AI tool (the skill's SKILL.md)
    hydo skill   install --agent claude|codex|cursor|copilot|gemini|all [--user] | --to DIR
                                                  copy the onboarding skill where an AI tool reads skills
    hydo compose <docker-compose.yaml> [--out hydo.yaml]
                                                  start a hydo.yaml from a Compose file (MANIFEST.md)
    hydo build   <hydo.yaml> [--out DIR]          compile the manifest into an arm64 guest image
    hydo run     "<schedule>" [--image IMG]       run one schedule from a fresh boot
    hydo repro   "<schedule>" [--image IMG]       the same as run (reproduce a reported failure)
    hydo repro   --regressions [--image IMG]      replay the plan's saved regressions (hydo/regressions/)
    hydo save    <explore dir> [--to hydo/regressions]   save an exploration's minimal schedules as regressions
    hydo explore [--seeds 1-50] [--image IMG] [--workers N] [--strategy swarm] [--out DIR]
                                                  generate, run, re-run and shrink schedules
    hydo report  [DIR]                            print the report of the last (or a given) exploration
    hydo errors  list [DIR] | accept SIG... --reason TEXT [--with F] [--during F] | check
                                                  error logs as findings: list new error kinds, accept
                                                  expected ones into hydo/known-errors.yaml (MANIFEST.md)
    hydo doctor                                   check this Mac and the installation

IMG defaults to the one image in ./hydo-out (what `hydo build` wrote there). Everything runs on
this Mac: `hydo build` formats the data disk inside a small helper VM (mke2fs in the guest),
so no Linux machine is needed. Plain Python 3.9+ (macOS's /usr/bin/python3 works).

"#;

fn main() {
    // `hydo report | head`: end quietly when the reader goes away
    // SAFETY: restoring the default action of SIGPIPE before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let argv: Vec<String> = std::env::args().collect();
    let ctx = Ctx::new();
    std::process::exit(dispatch(&ctx, &argv));
}

fn dispatch(ctx: &Ctx, argv: &[String]) -> i32 {
    let cmd = argv.get(1).map(String::as_str).unwrap_or("");
    let rest = if argv.len() > 2 { &argv[2..] } else { &[] };
    if matches!(cmd, "guide" | "skill" | "init") {
        return delegated::cmd_agents(ctx, cmd, rest);
    }
    let linux = ctx.platform == Platform::Linux;
    match cmd {
        "compose" => delegated::cmd_compose(ctx, rest),
        "build" => delegated::cmd_build(ctx, rest),
        "run" => runs::cmd_run(ctx, rest),
        "repro" => runs::cmd_repro(ctx, rest),
        "explore" => delegated::cmd_explore(ctx, rest),
        "report" => delegated::cmd_report(ctx, rest),
        "save" => runs::cmd_save(ctx, rest),
        "errors" => delegated::cmd_errors(ctx, rest),
        "check-history" => hydo_history::cli::main(rest),
        "doctor" if linux => doctor::cmd_doctor_linux(ctx),
        "doctor" => doctor::cmd_doctor_mac(ctx),
        "module" if linux => module::cmd_module(ctx, rest),
        _ => {
            print!("{}", if linux { HELP_LINUX } else { HELP_MAC });
            if matches!(cmd, "-h" | "--help" | "help") && argv.len() >= 2 {
                0
            } else {
                2
            }
        }
    }
}

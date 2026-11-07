//! `hydo` -- deterministic fault testing: one command over both engines (hvx on x86_64 Linux,
//! hvm on Apple Silicon). It replaced the two Python release CLIs (hydo/dist/linux/hydo and
//! hydo/dist/mac/hydo before M101, deleted in M107). Every command behaves the same on both
//! platforms since M107 (where the old CLIs differed, the more complete behaviour won); only what
//! a platform cannot do differs: `hydo module` exists on Linux only, and each builds images for
//! its own engine.
//!
//! Commands: run, repro (incl. --regressions: hydo-composer), save (hydo-composer), report,
//! setup (src/setup.rs, M154), doctor, module (Linux), errors and run/repro's error kinds
//! (hydo-errors), history checks (hydo-history), compose (hydo-compose), guide/skill/init
//! (hydo-agents), migrate (hydo-manifest, src/migrate.rs), build (hydo-build, src/build.rs),
//! explore (hydo-explore), diff and explain (src/diff.rs, src/explain.rs, M165: where two runs
//! first differ). No command runs Python.

mod build;
mod ctx;
mod delegated;
mod diff;
mod doctor;
mod explain;
mod hvxenv;
mod migrate;
mod module;
mod pyjson;
mod runs;
mod setup;

use ctx::{Ctx, Platform};

const HELP_LINUX: &str = r#"hydo -- deterministic fault testing on an x86_64 Linux machine with VT-x (the hvx backend).

    hydo init    [--dir DIR] [--name NAME]         start a repository's hydo/ folder and its AGENTS.md section
    hydo guide   [TOPIC | --list]                  print the onboarding guide for any AI tool (the skill's SKILL.md)
    hydo skill   install --agent claude|codex|cursor|copilot|gemini|all [--user] | --to DIR
                                                   copy the onboarding skill where an AI tool reads skills
    hydo setup   [--check | --verify] [--add-to-path]
                                                   make this unpacked release usable (install.sh runs it)
    hydo doctor                                    check this machine and the installation
    hydo module install|uninstall|build|load|unload|status [--name NAME] [--dry-run]
                                                   build for this kernel and load (install), or unload
                                                   and delete (uninstall) the hvx kernel module
    hydo module udev install|remove|status [--name NAME] [--group GROUP] [--dry-run]
                                                   optional: give /dev/hvx to a group, so runs need no sudo
    hydo compose <docker-compose.yaml> [--out hydo.yaml] [hydo-compose options]
                                                   start a hydo.yaml from a Compose file (MANIFEST.md)
    hydo build   [hydo.yaml] [--out DIR] [--set KEY=VALUE ...]
                                                   compile the manifest into a guest disk image
    hydo migrate [hydo.yaml] [--check] [--out FILE | --stdout]
                                                   rewrite a project file in the current format (format: 1)
    hydo run     [IMG] "<schedule>" [--image IMG] [--out DIR] [--known-errors FILE]
                                                   run one schedule from a fresh boot
    hydo repro   <run dir | explore dir | "<schedule>"> [--image IMG]
                                                   run again and compare the fingerprints
    hydo repro   --regressions [IMG]               replay the plan's saved regressions (hydo/regressions/)
    hydo repro   --transcript <control.jsonl> [--image IMG]
                                                   type an agent image's recorded run again
    hydo save    <explore dir> [--to hydo/regressions]
                                                   save the minimal schedules of an exploration as regressions
    hydo explore [IMG] [--seeds 1-20] [--strategy random|swarm|process|...] [--workers N]
                 [--out DIR] [any other explorer option]
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
yourself (see `hydo module udev`; HYDO_SUDO=1 forces sudo). One static program; no Python needed.

"#;

const HELP_MAC: &str = r#"hydo -- deterministic fault testing on an Apple Silicon Mac (the hvm backend).

    hydo init    [--dir DIR] [--name NAME]         start a repository's hydo/ folder and its AGENTS.md section
    hydo guide   [TOPIC | --list]                  print the onboarding guide for any AI tool (the skill's SKILL.md)
    hydo skill   install --agent claude|codex|cursor|copilot|gemini|all [--user] | --to DIR
                                                   copy the onboarding skill where an AI tool reads skills
    hydo setup   [--check | --verify] [--add-to-path]
                                                   make this unpacked release usable (install.sh runs it)
    hydo doctor                                    check this Mac and the installation
    hydo compose <docker-compose.yaml> [--out hydo.yaml] [hydo-compose options]
                                                   start a hydo.yaml from a Compose file (MANIFEST.md)
    hydo build   [hydo.yaml] [--out DIR] [--set KEY=VALUE ...]
                                                   compile the manifest into an arm64 guest image
    hydo migrate [hydo.yaml] [--check] [--out FILE | --stdout]
                                                   rewrite a project file in the current format (format: 1)
    hydo run     [IMG] "<schedule>" [--image IMG] [--out DIR] [--known-errors FILE]
                                                   run one schedule from a fresh boot
    hydo repro   <run dir | explore dir | "<schedule>"> [--image IMG]
                                                   run again and compare the fingerprints
    hydo repro   --regressions [IMG]               replay the plan's saved regressions (hydo/regressions/)
    hydo repro   --transcript <control.jsonl> [--image IMG]
                                                   type an agent image's recorded run again
    hydo save    <explore dir> [--to hydo/regressions]
                                                   save the minimal schedules of an exploration as regressions
    hydo explore [IMG] [--seeds 1-20] [--strategy random|swarm|process|...] [--workers N]
                 [--out DIR] [any other explorer option]
                                                   generate, run, re-run and shrink schedules
    hydo report  [DIR]                             print the report of the last (or a given) exploration
    hydo errors  list [DIR] | accept SIG... --reason TEXT [--with F] [--during F] | check
                                                   error logs as findings: list new error kinds, accept
                                                   expected ones into hydo/known-errors.yaml (MANIFEST.md)

IMG defaults to the one *-arm64.cpio.gz in ./hydo-out, or in ./.hydo for a project whose manifest
is hydo/hydo.yaml (the agreed repository layout; `hydo build` with no manifest uses ./hydo/hydo.yaml).
Everything runs on this Mac: `hydo build` formats the data disk inside a small helper VM (mke2fs
in the guest), so no Linux machine is needed. One program; no Python needed.

"#;

fn main() {
    // `hydo report | head`: end quietly when the reader goes away
    // SAFETY: restoring the default action of SIGPIPE before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let argv: Vec<String> = std::env::args().collect();
    let ctx = Ctx::new(hydo_config::Config::process());
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
        "build" => build::cmd_build(ctx, rest),
        "run" => runs::cmd_run(ctx, rest),
        "repro" => runs::cmd_repro(ctx, rest),
        "explore" => delegated::cmd_explore(ctx, rest),
        "report" => delegated::cmd_report(ctx, rest),
        // M165: `hydo diff --help`, `hydo explain --help` (the main help is a recorded answer)
        "diff" => diff::cmd_diff(ctx, rest),
        "explain" => explain::cmd_explain(ctx, rest),
        "save" => runs::cmd_save(ctx, rest),
        "errors" => delegated::cmd_errors(ctx, rest),
        "check-history" => hydo_history::cli::main(rest),
        "migrate" => migrate::cmd_migrate(rest),
        "setup" => setup::cmd_setup(ctx, rest),
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

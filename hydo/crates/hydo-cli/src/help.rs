//! `hydo <command> --help` (and `-h`) for the commands whose own argument handling has no help:
//! the answer comes before anything else the command does, so it needs no image, no project, no
//! recorded run and no engine. Before M210 `hydo explore --help` looked for an image and
//! stopped without one, `hydo doctor --help` checked the machine, `hydo report --help` read
//! `--help` as a directory, `hydo repro --help` printed `hydo run`'s usage, and `hydo build
//! --help` in a project built it. The commands with a help of their own (run, explain, diff,
//! inspect, ci, sql-gen, setup, migrate, errors, check-history, guide, skill, init) keep it.

use crate::runs::RUN_OPTS_HELP;

/// Whether `args` (the words after the command) ask for its help. The words after `--` belong
/// to a program the command starts (`hydo inspect exec DIR -- ls --help`).
pub fn asked(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "-h" || a == "--help")
}

/// The commands with sub-commands whose own help covers them all: `hydo errors list --help`,
/// `hydo inspect logs --help`, `hydo ci compare --help`, `hydo skill install --help` and
/// `hydo check-history DIR --help` print it. (`hydo skill update --help` and `hydo ci run
/// --help` are answered by their own code.)
pub const WITH_SUBCOMMANDS: &[&str] = &["errors", "inspect", "ci", "skill", "check-history"];

/// The help of `cmd`, for the commands this module answers; None for the others.
pub fn of(cmd: &str, linux: bool) -> Option<String> {
    let img = if linux {
        "the one *-disk.img"
    } else {
        "the one *-arm64.cpio.gz"
    };
    let backend = if linux { " [--backend hvx|kvm]" } else { "" };
    let text = match cmd {
        "explore" => format!(
            "usage: hydo explore [IMG] [--image IMG] [--seeds 1-20] [--strategy random|swarm|swarm-pos|process]
                    [--workers N] [--out DIR]{backend} [any other explorer option]
  generates schedules from seeds, runs each from a fresh boot, runs every failure again to check
  that it reproduces, and shrinks it to the fewest faults that still cause it.
  IMG defaults to {img} that `hydo build` wrote in ./hydo-out (or ./.hydo).
  Without --out the results go to hydo-out/explore; without --seeds, --schedule or --sweep the
  seeds are 1-20; without --workers the pool is sized from the machine and the image.
  `hydo report` prints the report, `hydo repro hydo-out/explore` runs every failure again, and
  `hydo save hydo-out/explore` keeps the minimal schedules as regressions. `hydo guide faults`
  lists the fault kinds.

The explorer's options (hydo explore passes them on; --image and --out have the defaults above):
{}",
            explorer_options()
        ),
        "repro" => format!(
            "usage: hydo repro <run dir | explore dir>{backend}
       hydo repro [IMG] \"<schedule>\" [--image IMG] [--out DIR] [--vcpus N] [--turn-margin N]
       hydo repro --regressions [IMG]
       hydo repro --transcript <control.jsonl> [--image IMG] [--out DIR]
  runs a recorded run, or every failure of an exploration, again from a fresh boot and compares
  the fingerprints (across engines or fingerprint epochs, the verdicts). It uses the engine that
  recorded the run unless --backend names one. The exit status is 1 when a run did not come out
  the same.
  With a schedule it is `hydo run` (`hydo run --help`). --regressions replays the saved
  regressions the image carries (hydo/regressions/). --transcript types an agent image's recorded
  run (its control.jsonl) again.
{RUN_OPTS_HELP}"
        ),
        "save" => "usage: hydo save <explore dir> [--to hydo/regressions]
  saves the minimal schedules of an exploration as regressions, one file each in the --to
  directory (default hydo/regressions). `hydo build` puts them into the image, and
  `hydo repro --regressions` replays them."
            .to_string(),
        "report" => "usage: hydo report [DIR]
  prints the report of an exploration (DIR's exploration.json) or of a run (DIR's result.json);
  while an exploration still runs, how far it has got. DIR defaults to hydo-out/explore."
            .to_string(),
        "doctor" if linux => "usage: hydo doctor [--backend kvm]
  checks this machine and the installation for the hvx engine: the CPU, the kernel module and
  its device, and the release's files. With --backend kvm it checks them for the KVM engine,
  among them whether this user can open /dev/kvm. It prints what it found, runs no guest and
  fetches nothing. The exit status is 1 when a check failed."
            .to_string(),
        "doctor" => "usage: hydo doctor
  checks this Mac and the installation, and prints what it found. It runs no guest and fetches
  nothing. The exit status is 1 when a check failed."
            .to_string(),
        "build" => format!(
            "usage: hydo build [hydo.yaml] [--out DIR] [--set KEY=VALUE ...] [--check]
  compiles the project file into a guest image ({}). Without a file it uses ./hydo/hydo.yaml.
  The image goes to --out DIR, by default ./.hydo for a file in a hydo/ folder and ./hydo-out
  otherwise.
  --set KEY=VALUE  sets one key of the project file for this build (repeat it for more)
  --check          checks the project file and its commands, and builds nothing
  `hydo guide workflow` describes the hydo/ folder.",
            if linux {
                "an x86_64 *-disk.img"
            } else {
                "an arm64 *-arm64.cpio.gz and its data disk"
            }
        ),
        "compose" => "usage: hydo compose <docker-compose.yaml> [--out hydo.yaml] [--images-dir DIR] [--env-file F]
                    [--profile P] [--use-env] [--no-resolve] [--lock FILE]
  writes a hydo.yaml from a Docker Compose file: its services, their images and how they start.
  `hydo guide services` describes the result."
            .to_string(),
        "module" if linux => "usage: hydo module build|install|uninstall|load|unload|status [--name NAME] [--dry-run] [--param KEY=VALUE ...]
       hydo module udev install|remove|status [--name NAME] [--group GROUP] [--mode 0660] [--no-adduser] [--dry-run]
  builds the hvx kernel module for the running kernel and loads it (install), or unloads and
  deletes it (uninstall). `udev install` gives /dev/hvx to a group, so that runs need no sudo.
  --dry-run prints what it would do and does nothing."
            .to_string(),
        _ => return None,
    };
    Some(text)
}

/// The explorer's usage text without its first words, indented under the heading.
fn explorer_options() -> String {
    let usage = hydo_explore::args::USAGE;
    let body = usage.strip_prefix("usage: hydo explore ").unwrap_or(usage);
    body.lines()
        .map(|l| format!("  {}", l.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn help_is_asked_before_a_double_dash_only() {
        assert!(asked(&words("--help")));
        assert!(asked(&words("hydo-out/explore -h")));
        assert!(asked(&words("logs DIR --help -- ls")));
        assert!(!asked(&words("exec DIR -- ls --help")));
        assert!(!asked(&words("disk=none")));
    }

    #[test]
    fn the_explore_help_lists_the_explorer_options() {
        for linux in [true, false] {
            let h = of("explore", linux).unwrap();
            assert!(h.starts_with("usage: hydo explore [IMG]"), "{h}");
            for opt in [
                "--seeds",
                "--strategy",
                "--workers",
                "--timing",
                "--steer-match",
            ] {
                assert!(h.contains(opt), "{opt}: {h}");
            }
            assert_eq!(h.contains("--backend hvx|kvm"), linux, "{h}");
        }
        assert!(of("module", false).is_none());
        assert!(of("run", true).is_none());
    }
}

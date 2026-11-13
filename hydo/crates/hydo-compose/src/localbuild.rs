//! Services built locally (a Compose `build:`), M214. `hydo compose` builds such an image itself
//! when a Docker CLI answers and no image file or pin is there yet, so that one import gives the
//! whole manifest (the image's HEALTHCHECK, EXPOSE and VOLUME are read from the file) and records
//! the pin in the lock file. The generated fetch-images.sh builds the image again where its file
//! is missing (a fresh checkout, the other architecture) with `hydo-fetch-image build`, which
//! checks the new file against the pin: a build that gives other bytes stops, and is kept beside
//! the file, until it is accepted with `HYDO_ACCEPT_REBUILD=1`.
//!
//! A pin is the digest that hydo reads from the file: the image config's digest of a
//! `docker save` archive, or the manifest's digest of an OCI layout ([`ocilocal`]). The lock
//! entry of a local build has `local_build: true` and the same fields as a registry image's.

use crate::ocilocal;
use crate::pyval::{dumps_indent1_sorted, from_json, pstr, Y};
use crate::shlex;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};

/// The guest architecture this machine builds, as fetch-images.sh decides it: arm64 on a Mac,
/// x86_64 elsewhere.
pub fn host_arch() -> &'static str {
    if cfg!(target_os = "macos") {
        "arm64"
    } else {
        "x86_64"
    }
}

/// The image platform's architecture name of a guest architecture (x86_64 -> amd64).
pub fn plat_key(arch: &str) -> &'static str {
    if arch == "x86_64" {
        "amd64"
    } else {
        "arm64"
    }
}

/// What looking for a Docker CLI found.
#[derive(Clone, Debug)]
pub enum Docker {
    /// A CLI whose `version` answered (so its engine runs).
    Found(String),
    /// A CLI that is installed but whose `version` failed: the engine does not run.
    NotRunning(String),
    /// No CLI at all.
    Missing(Vec<String>),
}

/// The Docker CLI to build with: `given` (a name or a path), else `docker`, else `podman`. A
/// candidate counts when `<cli> version` exits 0, which needs a running engine.
pub fn find_docker(given: Option<&str>) -> Docker {
    let cands: Vec<String> = match given {
        Some(g) if !g.is_empty() => vec![g.to_string()],
        _ => vec!["docker".into(), "podman".into()],
    };
    let mut not_running = None;
    for c in &cands {
        match Command::new(c)
            .arg("version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(s) if s.success() => return Docker::Found(c.clone()),
            Ok(_) => {
                not_running.get_or_insert_with(|| c.clone());
            }
            Err(_) => {}
        }
    }
    match not_running {
        Some(c) => Docker::NotRunning(c),
        None => Docker::Missing(cands),
    }
}

/// What to do when no Docker CLI can build: what to install or start.
pub fn docker_hint(d: &Docker) -> String {
    match d {
        Docker::Found(c) => format!("{c} is there"),
        Docker::NotRunning(c) => format!(
            "`{c} version` found no running engine: start it (Docker Desktop, `colima start`, \
             `podman machine start`, or `sudo systemctl start docker` on Linux)"
        ),
        Docker::Missing(c) => format!(
            "no Docker CLI was found (tried {}): install Docker Engine on Linux \
             (https://docs.docker.com/engine/install/), Docker Desktop or Colima on a Mac \
             (`brew install colima docker && colima start`), or Podman; name another compatible \
             CLI with HYDO_DOCKER=<cli> for fetch-images.sh or --docker <cli> for hydo compose",
            c.join(" and ")
        ),
    }
}

/// The command lines that make the file by hand, for messages.
pub fn by_hand(arch: &str, tag: &str, args: &[String], out: &str) -> String {
    format!(
        "docker build --platform linux/{} -t {} {} && docker save -o {} {}",
        plat_key(arch),
        shlex::quote(tag),
        quoted(args),
        shlex::quote(out),
        shlex::quote(tag)
    )
}

/// The words, quoted for sh and joined by spaces.
pub fn quoted(args: &[String]) -> String {
    args.iter()
        .map(|a| shlex::quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    // the CLI's own output goes to stderr, so that hydo compose's stdout stays as it was
    if let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() {
        cmd.stdout(Stdio::from(fd));
    }
    match cmd.stdin(Stdio::null()).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{what} failed ({s})")),
        Err(e) => Err(format!("{what}: {e}")),
    }
}

/// `<docker> build --platform linux/<arch> -t <tag> <args>` and `<docker> save` into
/// `<out>.part`, in `dir` (where the relative paths of `args` and `out` start). Returns the
/// part file's path and its platform entry (digest, variant, config). `who` starts the line
/// that says what runs.
pub fn build(
    who: &str,
    docker: &str,
    dir: &str,
    arch: &str,
    tag: &str,
    args: &[String],
    out: &str,
) -> Result<(String, Y), String> {
    let plat = format!("linux/{}", plat_key(arch));
    eprintln!(
        "{who}: building {tag} for {plat}: {docker} build --platform {plat} -t {} {}",
        shlex::quote(tag),
        quoted(args)
    );
    run(
        Command::new(docker)
            .current_dir(dir)
            .args(["build", "--platform", &plat, "-t", tag])
            .args(args),
        &format!("{docker} build"),
    )?;
    let abs = Path::new(dir).join(out);
    if let Some(p) = abs.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let part = format!("{}.part", abs.display());
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_dir_all(&part);
    run(
        Command::new(docker)
            .current_dir(dir)
            .args(["save", "-o", &part, tag]),
        &format!("{docker} save"),
    )?;
    match ocilocal::config_from_file(&part, plat_key(arch)) {
        Ok(info) => Ok((part, info)),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e.0)
        }
    }
}

/// Move the part file into place (replacing what is there).
pub fn place(part: &str, dst: &str) -> Result<(), String> {
    if Path::new(dst).is_dir() {
        std::fs::remove_dir_all(dst).map_err(|e| format!("{dst}: {e}"))?;
    }
    std::fs::rename(part, dst).map_err(|e| format!("{dst}: {e}"))
}

const BUILD_USAGE: &str =
    "hydo-fetch-image build -- build one image of a Compose `build:` service and check it
against its pin (fetch-images.sh calls it).

    hydo-fetch-image build [--docker CLI] [--accept 0|1] <x86_64|arm64> <out> <tag> <lock> <ref>
                     -- <docker build arguments>

It runs `<cli> build --platform linux/<arch> -t <tag> <arguments>` and `<cli> save`, reads the
digest of the saved image, and compares it with the lock file's pin for <ref>. The same digest,
or no pin yet (then it is recorded), puts the file at <out>. Another digest keeps the new file at
<out>.rebuilt and exits 1, unless --accept 1 (fetch-images.sh: HYDO_ACCEPT_REBUILD=1) takes it
and re-pins. Exit status 3: no Docker CLI could build.
";

/// `hydo-fetch-image build ...`: the exit status.
pub fn build_main(a: &[String]) -> i32 {
    let mut docker: Option<String> = None;
    let mut accept = false;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--docker" | "--accept" if i + 1 < a.len() => {
                if a[i] == "--docker" {
                    docker = Some(a[i + 1].clone());
                } else {
                    accept = a[i + 1] == "1";
                }
                i += 2;
            }
            "--" => {
                i += 1;
                break;
            }
            x => {
                pos.push(x.to_string());
                i += 1;
            }
        }
    }
    let args: Vec<String> = a[i..].to_vec();
    if pos.len() != 5 || !matches!(pos[0].as_str(), "x86_64" | "arm64") || args.is_empty() {
        eprint!("{BUILD_USAGE}");
        return 2;
    }
    let (arch, out, tag, lock, rf) = (&pos[0], &pos[1], &pos[2], &pos[3], &pos[4]);
    let p = "fetch-images.sh";
    let d = match find_docker(docker.as_deref()) {
        Docker::Found(d) => d,
        other => {
            eprintln!(
                "{p}: {out} is built locally (build:), but {}. Or make the file yourself: {}",
                docker_hint(&other),
                by_hand(arch, tag, &args, out)
            );
            return 3;
        }
    };
    let (part, info) = match build(p, &d, ".", arch, tag, &args, out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{p}: {tag} for {arch}: {e}");
            return 1;
        }
    };
    let pk = plat_key(arch);
    let got = pstr(info.getn("digest"));
    let mut doc = std::fs::read(lock)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| from_json(&v))
        .filter(Y::is_map)
        .unwrap_or_else(|| {
            Y::Map(vec![(
                Y::s("generated_by"),
                Y::s("hvx/onboard/hydo-compose"),
            )])
        });
    let mut images = doc
        .get("images")
        .filter(|v| v.is_map())
        .cloned()
        .unwrap_or(Y::Map(Vec::new()));
    let entry = images.get(rf).cloned();
    if let Some(e) = &entry {
        if !e.getn("local_build").truthy() {
            eprintln!("{p}: {lock} pins {rf} as a registry image, not a local build; run hydo compose again");
            let _ = std::fs::remove_file(&part);
            return 1;
        }
    }
    let old = entry
        .as_ref()
        .and_then(|e| e.getn("platforms").get(pk))
        .cloned();
    let settings = |x: &Y| x.getn("config").clone();
    let mut repin = false;
    match &old {
        Some(o) if pstr(o.getn("digest")) == got => {
            eprintln!("{p}: {out}: built {tag} for linux/{pk}, {got}, as pinned in {lock}");
        }
        Some(o) => {
            let want = pstr(o.getn("digest"));
            let same = settings(o) == settings(&info);
            let what = if same {
                "its settings (entrypoint, command, environment, ports, healthcheck, volumes, user) are the same"
            } else {
                "its settings (entrypoint, command, environment, ports, healthcheck, volumes, user) changed too"
            };
            if !accept {
                let kept = format!("{out}.rebuilt");
                let _ = std::fs::remove_file(&kept);
                let _ = std::fs::remove_dir_all(&kept);
                if let Err(e) = place(&part, &kept) {
                    eprintln!("{p}: {e}");
                    return 1;
                }
                eprintln!(
                    "{p}: {tag} for linux/{pk} built to {got}, but {lock} pins {want}: the image's \
                     bytes changed (its sources, its base image, or a build that is not \
                     reproducible), and {what}. The new build is {kept}; {out} is not made. To use \
                     it: HYDO_ACCEPT_REBUILD=1 sh fetch-images.sh (it re-pins {rf} in {lock})"
                );
                return 1;
            }
            eprintln!(
                "{p}: {tag} for linux/{pk} built to {got}, not the pinned {want}; accepted \
                 (HYDO_ACCEPT_REBUILD=1) and re-pinned in {lock}; {what}"
            );
            if !same {
                eprintln!("{p}: run hydo compose again, so that hydo.yaml follows the image's new settings");
            }
            repin = true;
        }
        None => {
            eprintln!("{p}: {out}: built {tag} for linux/{pk}, {got}; pinned in {lock}");
            if entry.is_none() {
                eprintln!("{p}: run hydo compose again, so that hydo.yaml takes the image's settings (its HEALTHCHECK, EXPOSE and VOLUME)");
            }
            repin = true;
        }
    }
    if let Err(e) = place(&part, out) {
        eprintln!("{p}: {e}");
        return 1;
    }
    // a build an earlier run kept aside is answered by this one
    let kept = format!("{out}.rebuilt");
    let _ = std::fs::remove_file(&kept);
    let _ = std::fs::remove_dir_all(&kept);
    if repin {
        let mut e = match entry {
            Some(e) => e,
            None => match crate::local_entry(rf) {
                Ok(e) => e,
                Err(x) => {
                    eprintln!("{p}: {x}");
                    return 1;
                }
            },
        };
        let mut plats = e
            .get("platforms")
            .filter(|v| v.is_map())
            .cloned()
            .unwrap_or(Y::Map(Vec::new()));
        plats.set(Y::s(pk), info);
        e.set(Y::s("platforms"), plats);
        images.set(Y::s(rf), e);
        doc.set(Y::s("images"), images);
        if let Err(x) = std::fs::write(lock, dumps_indent1_sorted(&doc) + "\n") {
            eprintln!("{p}: {lock}: {x}");
            return 1;
        }
    }
    0
}

//! The saved regressions stay out of the image from the fingerprint epoch 4 (Milestone 269): an
//! edit of a regression, a new one or a removed one changes nothing that `hydo build` puts into
//! the image. Here that is checked without the build tools, on everything the image is made from:
//! the manifest the build stages (`Prepared::data`), the test composer's files in the root
//! (`stage::compose_stage`) and the compiled manifest.sh and prelude (`--print-shell`). The
//! regressions go to `<image>.regressions.json` (`stage::write_regressions`). The whole image's
//! sha256 was compared with real builds on both architectures (docs/HISTORY.md Milestone 269).

use crate::tmpdir::TmpDir;
use hydo_build::{prepare, print_shell_tools, shell, stage, Ctx, Io, Prepared};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

/// The test's folder, removed when the test ends (Milestone 271).
fn scratch(name: &str) -> TmpDir {
    TmpDir::new(&format!(
        "hydo-build-regressions-{name}-{}",
        std::process::id()
    ))
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(src)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    for n in names {
        let (s, d) = (src.join(&n), dst.join(&n));
        if s.is_dir() {
            copy_dir(&s, &d);
        } else {
            std::fs::copy(&s, &d).unwrap();
        }
    }
}

/// Every file of a staged root: its path, mode and sha256.
fn tree(root: &Path) -> Vec<String> {
    fn walk(top: &Path, d: &Path, out: &mut Vec<String>) {
        let mut names: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        for p in names {
            let md = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(top).unwrap().display().to_string();
            let mode = md.permissions().mode();
            if md.is_dir() {
                out.push(format!("{rel}/ {mode:o}"));
                walk(top, &p, out);
            } else {
                let sha = Sha256::digest(std::fs::read(&p).unwrap());
                out.push(format!("{rel} {mode:o} {sha:x}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// What the image is made from, for the manifest at `path`: the staged manifest (as JSON), the
/// composer's staged root, manifest.sh and the prelude; and the regressions the build keeps out.
fn image_inputs(path: &Path, root: &Path) -> (Vec<String>, Vec<serde_json::Value>) {
    let ctx = Ctx::standalone(Default::default());
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = Io {
        out: &mut o,
        err: &mut e,
    };
    let p: Prepared = match prepare(&ctx, &path.display().to_string(), "x86_64", &[], &mut io) {
        Ok(p) => p,
        Err(f) => panic!("{}: {}", path.display(), f.message()),
    };
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    stage::compose_stage(&p.data, root).unwrap();
    let (ms, pre) = shell::compile_shell(
        &ctx,
        &p.data,
        &p.loaded.service_order,
        &print_shell_tools(&p.data),
    )
    .unwrap_or_else(|f| panic!("{}", f.message()));
    let mut v = vec![serde_json::to_string(&p.data).unwrap(), ms, pre];
    v.extend(tree(root));
    (v, p.regressions)
}

#[test]
fn an_edited_regression_leaves_the_image_alone() {
    let d = scratch("edit");
    copy_dir(
        &repo().join("hydo/guide/examples/example-redis-composed/hydo"),
        &d.join("hydo"),
    );
    let m = d.join("hydo/hydo.yaml");
    let reg = d.join("hydo/regressions/lost-update.schedule");
    let root = d.join("root");
    let (first, regs) = image_inputs(&m, &root);
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0]["name"], "lost-update");
    // nothing of the regression is in the root (before M269: /hydo/regressions/ and compose.json)
    assert!(!first.iter().any(|l| l.starts_with("hydo/regressions")));
    let compose = std::fs::read_to_string(root.join("etc/hydo/compose.json")).unwrap();
    assert!(compose.contains("\"regressions\": []"), "{compose}");
    assert!(!compose.contains("lost-update"));

    // an edited comment, another expectation, another schedule
    let text = std::fs::read_to_string(&reg).unwrap();
    let edited = text
        .replace(
            "# expect: ok",
            "# a new comment\n# expect: fp=0123456789abcdef",
        )
        .replace("W=0000002a", "W=0000002b");
    assert_ne!(edited, text);
    std::fs::write(&reg, &edited).unwrap();
    let (second, regs2) = image_inputs(&m, &root);
    assert_eq!(first, second, "an edited regression changed the image");
    assert_eq!(regs2[0]["expect"], "fp=0123456789abcdef");

    // a second regression, saved as `hydo save` writes one with a resolved composition
    std::fs::write(
        d.join("hydo/regressions/another.schedule"),
        "# found by `hydo explore` (seed 6): violation\n# expect: violation\n\
         disk=none dseed=1 | run W=:p2,r200,cparallel_driver_transfer,7.4.0.721476632\n",
    )
    .unwrap();
    let (third, regs3) = image_inputs(&m, &root);
    assert_eq!(first, third, "a new regression changed the image");
    assert_eq!(regs3.len(), 2);

    // no regressions/ folder at all: the same image
    std::fs::remove_dir_all(d.join("hydo/regressions")).unwrap();
    let (none, regs4) = image_inputs(&m, &root);
    assert_eq!(first, none, "the image depends on the folder regressions/");
    assert!(regs4.is_empty());

    // the file beside the image: written with the regressions, removed without them
    let img = d.join("explore-ob-x-disk.img");
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = Io {
        out: &mut o,
        err: &mut e,
    };
    stage::write_regressions(&img, &regs3, &mut io).unwrap();
    let side = hydo_image::regressions_path(&img);
    let back = hydo_image::read_regressions(&img).unwrap().unwrap();
    assert_eq!(back, regs3);
    stage::write_regressions(&img, &[], &mut io).unwrap();
    assert!(!side.exists());
    let said = String::from_utf8(o).unwrap();
    assert!(
        said.contains("(2 saved regressions: the host replays them"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A plan of only regressions (a `regressions/` folder beside a manifest without test commands
/// and checks) is no plan for the image: no compose.json, no composer lines in manifest.sh.
#[test]
fn a_plan_of_only_regressions_is_no_plan_in_the_image() {
    let d = scratch("only");
    std::fs::create_dir_all(d.join("hydo")).unwrap();
    let m = d.join("hydo/hydo.yaml");
    std::fs::write(
        &m,
        "name: only-regressions\nformat: 1\nservices:\n  - name: s\n    cmd: /bin/sleep 1000\n    ready: /bin/true\n\
         workload:\n  run: /bin/true\n  steps: 2\ninvariants:\n  - {id: up, type: always, cmd: /bin/true, msg: up}\n",
    )
    .unwrap();
    let root = d.join("root");
    let (without, none) = image_inputs(&m, &root);
    assert!(none.is_empty());
    std::fs::create_dir_all(d.join("hydo/regressions")).unwrap();
    std::fs::write(
        d.join("hydo/regressions/crash.schedule"),
        "# expect: violation\ndisk=none dseed=1 | run K=s@1+1\n",
    )
    .unwrap();
    let (with, regs) = image_inputs(&m, &root);
    assert_eq!(regs.len(), 1);
    assert_eq!(
        without, with,
        "a plan of only regressions changed the image"
    );
    assert!(!root.join("etc/hydo/compose.json").exists());
    assert!(!with[1].contains("HYDO_COMPOSE=1"));
    let _ = std::fs::remove_dir_all(&d);
}

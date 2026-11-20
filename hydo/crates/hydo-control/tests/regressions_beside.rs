//! Where the host finds an image's saved regressions (Milestone 269): inside an image built before
//! the fingerprint epoch 4 (`/etc/hydo/compose.json` and `compose.saved/`), beside an image built
//! at epoch 4 or later (`<image>.regressions.json`). The replay (`hydo repro --regressions`,
//! `hydo ci run`) reads them with [`regressions_of_image`], and a run of a `W=@<key>` takes the
//! composition the key names from the same place ([`plan_of_image`]). The images here are small
//! arm64 initramfs files with only the files these readers open.

use crate::support::top;
use crate::tmpdir::TmpDir;
use hydo_control::agent::{plan_of_image, regressions_of_image};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A resolved composition and the key that names it (`W=@<key>`).
const COMPOSE: &str = ":p2,r200,cparallel_driver_transfer,7.4.0.721476632";

/// The test's folder, removed when the test ends (Milestone 271).
fn scratch(name: &str) -> TmpDir {
    TmpDir::new(&format!(
        "hydo-control-regressions-{name}-{}",
        std::process::id()
    ))
}

/// The composed Redis example in `d/hydo`, with a saved regression in the saved form.
fn project(d: &Path) -> (PathBuf, String) {
    let src = top().join("hydo/guide/examples/example-redis-composed/hydo");
    let dst = d.join("hydo");
    for sub in ["", "workload", "checks", "regressions"] {
        std::fs::create_dir_all(dst.join(sub)).unwrap();
        for e in std::fs::read_dir(src.join(sub)).unwrap() {
            let p = e.unwrap().path();
            if p.is_file() {
                std::fs::copy(&p, dst.join(sub).join(p.file_name().unwrap())).unwrap();
            }
        }
    }
    let key = hydo_composer::key_of(COMPOSE);
    std::fs::write(
        dst.join("regressions/long.schedule"),
        format!("# compose: {COMPOSE}\n# expect: violation\ndisk=none dseed=1 | run W=@{key}\n"),
    )
    .unwrap();
    (dst.join("hydo.yaml"), key)
}

/// An arm64 image of the manifest at `m` built at `epoch`: the manifest's text, the image record
/// and the plan meta, with its regressions only before epoch 4 (as hydo-build stages them).
fn image(d: &Path, m: &Path, epoch: u64) -> PathBuf {
    let (_, meta) = hydo_manifest::for_explorer(m).unwrap();
    let mut meta = meta.unwrap();
    let regs = meta["regressions"].as_array().cloned().unwrap();
    let in_image = hydo_image::defaults(epoch, "arm64").regressions_in_image;
    if !in_image {
        meta["regressions"] = Value::Array(Vec::new());
    }
    let root = d.join(format!("root{epoch}"));
    let etc = root.join("etc/hydo");
    std::fs::create_dir_all(&etc).unwrap();
    std::fs::copy(m, etc.join("hydo.yaml")).unwrap();
    std::fs::write(
        etc.join("compose.json"),
        hydo_build::pyjson::dumps_indent1_sorted(&meta) + "\n",
    )
    .unwrap();
    let mut rec = hydo_image::kernels::record_with("arm64", false, None);
    rec.epoch = epoch;
    std::fs::write(
        root.join(hydo_image::RECORD_PATH),
        hydo_image::canonical(&rec),
    )
    .unwrap();
    let img = d.join(format!("explore-ob-e{epoch}-arm64.cpio.gz"));
    hydo_build::cpio::write_cpio_gz(&root, &img, 0, None, false, false).unwrap();
    if !in_image {
        std::fs::write(
            hydo_image::regressions_path(&img),
            hydo_image::regressions_text(&regs),
        )
        .unwrap();
    }
    img
}

#[test]
fn an_epoch_4_image_has_its_regressions_beside_it() {
    let d = scratch("e4");
    let (m, key) = project(&d);
    let img = image(&d, &m, 4);
    let side = hydo_image::regressions_path(&img);
    // the image itself has none
    let inside = hydo_engine::image::embedded_compose(&img).unwrap();
    assert!(!inside.contains("lost-update") && !inside.contains(&key));
    let got = regressions_of_image(&img).unwrap();
    let names: Vec<&str> = got
        .regressions
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["long", "lost-update"]);
    assert_eq!(got.file.as_deref(), Some(side.as_path()));
    assert_eq!(got.meta["regressions"].as_array().unwrap().len(), 2);
    assert!(got.meta["commands"]
        .as_array()
        .is_some_and(|c| !c.is_empty()));
    // a run of W=@<key> takes the composition from the file beside the image
    let plan = plan_of_image(&img).unwrap();
    let saved = &plan.compose.as_ref().unwrap().saved;
    assert_eq!(saved.get(&key).map(String::as_str), Some(COMPOSE));
    // without the file: no regressions, no saved composition, and the image still runs
    std::fs::remove_file(&side).unwrap();
    let got = regressions_of_image(&img).unwrap();
    assert!(got.regressions.is_empty() && got.file.is_some());
    assert!(plan_of_image(&img)
        .unwrap()
        .compose
        .unwrap()
        .saved
        .is_empty());
    // a file that is not a regressions file is named
    std::fs::write(&side, "{\"v\": 1}").unwrap();
    let e = regressions_of_image(&img).unwrap_err();
    assert!(
        e.contains(".regressions.json") && e.contains("version 1"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_epoch_3_image_keeps_its_own() {
    let d = scratch("e3");
    let (m, key) = project(&d);
    let img = image(&d, &m, 3);
    // a stray file beside an epoch-3 image is not read: the image carries its regressions
    std::fs::write(
        hydo_image::regressions_path(&img),
        hydo_image::regressions_text(&[]),
    )
    .unwrap();
    let got = regressions_of_image(&img).unwrap();
    assert_eq!(got.regressions.len(), 2);
    assert_eq!(got.file, None);
    let plan = plan_of_image(&img).unwrap();
    let saved = &plan.compose.as_ref().unwrap().saved;
    assert_eq!(saved.get(&key).map(String::as_str), Some(COMPOSE));
    let _ = std::fs::remove_dir_all(&d);
}

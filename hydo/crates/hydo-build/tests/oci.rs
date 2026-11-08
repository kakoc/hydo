//! Services from Docker/OCI images: the Rust `oci::prepare` + `oci::stage` against the recorded
//! answers of hydo_oci.py (the Python reference, frozen) on synthetic images: docker save archives,
//! plain and gzip, and an OCI layout with a multi-arch index; GNU and PAX layers with whiteouts, an
//! opaque directory, hard links, a symlinked directory in a path, a long name, a setuid file,
//! owners, a device and a FIFO; manifests with env, user, workdir, entrypoint and every kind of
//! mount, and two refusals; for both architectures: the same service data, the same owners and
//! modes (the meta that goes into debugfs or the cpio headers), the same printed lines, and the
//! same staged tree (names, types, modes, link targets, hard links, bytes).
//!
//! `tests/golden/oci/images.tar.gz` holds the images and manifests that `tests/py_oci.py make`
//! wrote with Python's tarfile (extracted with the system `tar` for each run), `busybox` is the
//! stand-in file staged as the busybox binary, and `answers.json` is what `py_oci.py stage`
//! (hydo_oci.prepare + stage, Python 3.15.0 on the Mac) answered for each case and the tree it
//! staged, recorded at commit 2a58c4b (`git show 2a58c4b:hydo/crates/hydo-build/tests/py_oci.py`
//! and `git show 2a58c4b:hydo/onboard/hydo_oci.py` restore them). Paths under the scratch
//! directory are written `<tmp>`. From now on Rust is the reference: `HYDO_BLESS=1 cargo test -p
//! hydo-build --test it oci::` rewrites `answers.json` from the Rust answers; review the diff.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden_dir() -> PathBuf {
    crate_dir().join("tests/golden/oci")
}

/// Every entry under root: (type, mode, link target or content, hard-link group).
fn tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut inodes: HashMap<u64, String> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut ents: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let ft = m.file_type();
            let v = if ft.is_symlink() {
                format!("link -> {}", std::fs::read_link(&p).unwrap().display())
            } else if ft.is_dir() {
                stack.push(p.clone());
                format!("dir {:o}", m.mode() & 0o7777)
            } else {
                let first = inodes.entry(m.ino()).or_insert_with(|| rel.clone()).clone();
                let b = std::fs::read(&p).unwrap();
                format!(
                    "file {:o} {} bytes {:?} same-inode-as {}",
                    m.mode() & 0o7777,
                    b.len(),
                    String::from_utf8_lossy(&b[..b.len().min(64)]),
                    first
                )
            };
            out.insert(rel, v);
        }
    }
    out
}

/// The scratch directory written `<tmp>` in every string (both spellings: as given and resolved).
fn norm(v: &Value, tmp: &Path) -> Value {
    let a = tmp.to_string_lossy().into_owned();
    let b = tmp
        .canonicalize()
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|_| a.clone());
    match v {
        Value::String(s) => Value::String(s.replace(&b, "<tmp>").replace(&a, "<tmp>")),
        Value::Array(x) => Value::Array(x.iter().map(|y| norm(y, tmp)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, y)| (k.replace(&b, "<tmp>").replace(&a, "<tmp>"), norm(y, tmp)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What Rust answers for one case: {error | meta + services, out, err} and the staged tree.
fn rust_case(mpath: &Path, arch: &str, rrs: &Path, busybox: &Path) -> Value {
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(mpath).unwrap()).unwrap();
    std::fs::create_dir_all(rrs.join("bin")).unwrap();
    let (mut o, mut e): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let res = {
        let mut io = hydo_build::Io {
            out: &mut o,
            err: &mut e,
        };
        hydo_build::oci::prepare(&mut m, &mpath.to_string_lossy(), arch, &mut io).and_then(
            |mut svcs| {
                let tmpd = PathBuf::from(format!("{}.tmp", rrs.display()));
                hydo_build::oci::stage(
                    &mut svcs,
                    rrs,
                    busybox,
                    &tmpd,
                    arch == "arm64",
                    false,
                    &mut io,
                )
            },
        )
    };
    let mut rs = json!({});
    match res {
        Ok(meta) => {
            let mm: serde_json::Map<String, Value> = meta
                .iter()
                .map(|(k, (a, b, c, d))| (k.clone(), json!([a, b, c, d])))
                .collect();
            rs["meta"] = Value::Object(mm);
            rs["services"] = m["services"].clone();
        }
        Err(f) => rs["error"] = Value::String(f.message()),
    }
    rs["out"] = Value::String(String::from_utf8_lossy(&o).into_owned());
    rs["err"] = Value::String(String::from_utf8_lossy(&e).into_owned());
    rs
}

#[test]
fn oci_staging_matches_hydo_oci() {
    // The recorded modes are those of directories made under umask 022; a umask of 002
    // (common for users on Linux) gave other modes (M167).
    // SAFETY: umask only sets the process's file creation mask.
    unsafe {
        libc::umask(0o022);
    }
    let tmp = std::env::temp_dir().join(format!("hydo-build-oci-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let busybox = golden_dir().join("busybox");
    let bless = std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1");
    let st = Command::new("tar")
        .arg("-C")
        .arg(&tmp)
        .arg("-xzf")
        .arg(golden_dir().join("images.tar.gz"))
        .status()
        .expect("tar");
    assert!(st.success(), "tar -xzf images.tar.gz failed");
    let recorded: Value = if bless {
        json!({})
    } else {
        serde_json::from_str(
            &std::fs::read_to_string(golden_dir().join("answers.json"))
                .expect("tests/golden/oci/answers.json (HYDO_BLESS=1 writes it)"),
        )
        .unwrap()
    };
    let mut answers = serde_json::Map::new();
    let mut compared = 0;
    let mut failures = Vec::new();
    for name in ["docker", "dockergz", "oci", "badref", "badmount"] {
        for arch in ["x86_64", "arm64"] {
            let label = format!("{name} --arch {arch}");
            let mpath = tmp.join(format!("{name}.json"));
            let rrs = tmp.join(format!("rs-{name}-{arch}"));
            let mut rs = norm(&rust_case(&mpath, arch, &rrs, &busybox), &tmp);
            let staged = rs.get("error").is_none();
            if staged {
                rs["tree"] = json!(tree(&rrs));
            }
            let want = if bless {
                rs.clone()
            } else {
                recorded
                    .get(&label)
                    .cloned()
                    .unwrap_or_else(|| panic!("{label}: not in answers.json"))
            };
            for k in ["error", "meta", "services", "out", "err", "tree"] {
                if want.get(k) != rs.get(k) {
                    failures.push(format!(
                        "{label}: {k} differs\n  recorded: {}\n  rs: {}",
                        want.get(k).cloned().unwrap_or(Value::Null),
                        rs.get(k).cloned().unwrap_or(Value::Null)
                    ));
                }
            }
            if staged {
                assert!(
                    rs["tree"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .any(|k| k.ends_with("etc/hydo/oci/a/image.json") || k.contains("/oci/")),
                    "{label}: nothing staged"
                );
            }
            answers.insert(label, want);
            compared += 1;
        }
    }
    if bless {
        let mut text = serde_json::to_string_pretty(&Value::Object(answers)).unwrap();
        text.push('\n');
        std::fs::write(golden_dir().join("answers.json"), text).unwrap();
    }
    let _ = Command::new("chmod")
        .arg("-R")
        .arg("u+w")
        .arg(&tmp)
        .status();
    let _ = std::fs::remove_dir_all(&tmp);
    for f in failures.iter().take(30) {
        eprintln!("{f}\n");
    }
    assert!(
        failures.is_empty(),
        "{} differences in {compared} cases",
        failures.len()
    );
    assert_eq!(compared, 10);
}

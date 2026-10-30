//! Services from Docker/OCI images: the Rust `oci::prepare` + `oci::stage` against hydo_oci.py's
//! on synthetic images (tests/py_oci.py writes them: docker save archives, plain and gzip, and an
//! OCI layout with a multi-arch index; GNU and PAX layers with whiteouts, an opaque directory, hard
//! links, a symlinked directory in a path, a long name, a setuid file, owners, a device and a FIFO;
//! manifests with env, user, workdir, entrypoint and every kind of mount, and two refusals), for
//! both architectures: the same service data, the same owners and modes (the meta that goes into
//! debugfs or the cpio headers), the same printed lines, and the same staged tree (names, types,
//! modes, link targets, hard links, bytes). Needs python3 (HYDO_PYTHON).

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
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

#[test]
fn oci_staging_matches_hydo_oci() {
    let tmp = std::env::temp_dir().join(format!("hydo-build-oci-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let st = Command::new(python())
        .arg(crate_dir().join("tests/py_oci.py"))
        .arg("make")
        .arg(&tmp)
        .status()
        .expect("python3 is needed (set HYDO_PYTHON)");
    assert!(st.success());
    let busybox = crate_dir().join("tests/py_oci.py");
    let mut compared = 0;
    let mut failures = Vec::new();
    for name in ["docker", "dockergz", "oci", "badref", "badmount"] {
        for arch in ["x86_64", "arm64"] {
            let mpath = tmp.join(format!("{name}.json"));
            let rpy = tmp.join(format!("py-{name}-{arch}"));
            let rrs = tmp.join(format!("rs-{name}-{arch}"));
            let out = Command::new(python())
                .arg(crate_dir().join("tests/py_oci.py"))
                .arg("stage")
                .arg(&mpath)
                .arg(arch)
                .arg(&rpy)
                .arg(&busybox)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let py: Value = serde_json::from_slice(&out.stdout).unwrap();
            // the Rust side
            let mut m: Value =
                serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
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
                            &rrs,
                            &busybox,
                            &tmpd,
                            arch == "arm64",
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
            let label = format!("{name} --arch {arch}");
            for k in ["error", "meta", "services", "out", "err"] {
                if py.get(k) != rs.get(k) {
                    failures.push(format!(
                        "{label}: {k} differs\n  py: {}\n  rs: {}",
                        py.get(k).cloned().unwrap_or(Value::Null),
                        rs.get(k).cloned().unwrap_or(Value::Null)
                    ));
                }
            }
            if py.get("error").is_none() {
                let (tp, tr) = (tree(&rpy), tree(&rrs));
                if tp != tr {
                    for (k, v) in &tp {
                        if tr.get(k) != Some(v) {
                            failures.push(format!("{label}: {k}: py {v} / rs {:?}", tr.get(k)));
                        }
                    }
                    for k in tr.keys().filter(|k| !tp.contains_key(*k)) {
                        failures.push(format!("{label}: {k}: only in the Rust tree"));
                    }
                }
                assert!(
                    tp.keys()
                        .any(|k| k.ends_with("etc/hydo/oci/a/image.json") || k.contains("/oci/")),
                    "{label}: nothing staged"
                );
            }
            compared += 1;
        }
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

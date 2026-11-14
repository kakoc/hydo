//! The project format (M114): the committed schema is the one the model generates, it changed
//! only by additions since the last released format version, and every permanent example
//! document still reads into exactly its recorded model.
//!
//! Files (repository root):
//! - `hydo/schema/project-v1.json`: the schema, generated from `src/model.rs`.
//! - `hydo/schema/released/project-v1.json`: the schema of format 1 as the last hydo release
//!   shipped it. It is replaced only when a release ships (by the current schema, which this test
//!   proves to be an addition to it, with `hydo-schema-compat`'s check), never edited by hand.
//! - `hydo/schema/examples/project/v1/<case>/`: one document each (`hydo/hydo.yaml` with its
//!   folder, or one `*.yaml` / `*.json` file) and `model.json`, the model it reads into. `INDEX`
//!   lists every case; a case is never removed or changed, a later format adds its own.
//!
//! `HYDO_BLESS=1 cargo test -p hydo-manifest --test it format::` rewrites the schema and writes the
//! `model.json` of a NEW case (one listed in INDEX without a model.json); a model.json that exists
//! is never rewritten.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hydo_manifest::{migrate, model, LoadOptions};
use hydo_schema_compat::{check, Readers, Rules};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1")
}

fn read_json(p: &Path) -> Value {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&t).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap() + "\n"
}

#[test]
fn schema_file_is_generated_from_the_model() {
    let p = root().join(model::SCHEMA_PATH);
    let now = model::schema_text();
    if bless() {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &now).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&p).unwrap_or_default();
    assert!(
        committed == now,
        "{} is not the schema of the model in src/model.rs; HYDO_BLESS=1 cargo test -p hydo-manifest --test it format:: rewrites it (and the additions-only test then checks the change)",
        p.display()
    );
}

#[test]
fn schema_changed_only_by_additions_since_the_release() {
    let released = read_json(&root().join("hydo/schema/released/project-v1.json"));
    let now = model::schema();
    // Readers::Current: only hydo reads a project, and an older hydo refuses a key or value it
    // does not know on purpose (`hydo_version:` names the release a project needs), so a format-1
    // schema may widen what it accepts; it may not refuse or reread a document it accepted.
    let problems: Vec<String> = check(&released, &now, &Rules::new(Readers::Current))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        problems.is_empty(),
        "format {} may only gain optional keys and values; a breaking change needs format {} (MANIFEST.md, \"The project format\"):\n  {}",
        model::FORMAT,
        model::FORMAT + 1,
        problems.join("\n  ")
    );
}

/// The model's keys and the validator's keys are the same, place by place (the validator refuses
/// a top-level image: with its own message, so the model does not have it).
#[test]
fn model_and_validator_know_the_same_keys() {
    let s = model::schema();
    let props = |name: &str| -> BTreeSet<String> {
        let d = if name == "Project" {
            &s
        } else {
            &s["$defs"][name]
        };
        d["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("no definition {name} in the schema"))
            .keys()
            .cloned()
            .collect()
    };
    for (name, keys) in hydo_manifest::validator_keys() {
        let mut want: BTreeSet<String> = keys.iter().map(|k| k.to_string()).collect();
        if name == "Project" {
            want.remove("image");
        }
        assert_eq!(
            props(name),
            want,
            "{name}: the model and the validator differ"
        );
    }
}

struct Case {
    name: String,
    dir: PathBuf,
    doc: PathBuf,
}

fn examples_dir() -> PathBuf {
    root().join("hydo/schema/examples/project/v1")
}

fn cases() -> Vec<Case> {
    let e = examples_dir();
    let index: Vec<String> = std::fs::read_to_string(e.join("INDEX"))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let dirs: BTreeSet<String> = std::fs::read_dir(&e)
        .unwrap()
        .flatten()
        .filter(|x| x.path().is_dir())
        .map(|x| x.file_name().to_string_lossy().into_owned())
        .collect();
    let listed: BTreeSet<String> = index.iter().cloned().collect();
    assert_eq!(
        dirs, listed,
        "INDEX and the case directories differ (a case is never removed; a new one is listed)"
    );
    index
        .into_iter()
        .map(|name| {
            let dir = e.join(&name);
            let folder = dir.join("hydo/hydo.yaml");
            let doc = if folder.is_file() {
                folder
            } else {
                let docs: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .unwrap()
                    .flatten()
                    .map(|x| x.path())
                    .filter(|p| {
                        p.is_file()
                            && p.file_name().is_some_and(|n| n != "model.json")
                            && p.extension()
                                .is_some_and(|x| x == "yaml" || x == "yml" || x == "json")
                    })
                    .collect();
                assert_eq!(
                    docs.len(),
                    1,
                    "{name}: one document per case, found {docs:?}"
                );
                docs[0].clone()
            };
            Case { name, dir, doc }
        })
        .collect()
}

#[test]
fn every_example_reads_into_its_recorded_model() {
    let mut fails = Vec::new();
    for c in cases() {
        // valid today: the validator accepts it on at least one architecture
        let valid = ["x86_64", "arm64"].iter().any(|arch| {
            // the examples are permanent and some pin the hydo they were written for
            // (`hydo_version: "0.1"`); that pin is a build-time check against this hydo's
            // version, not part of the format, so it only warns here
            let opts = LoadOptions {
                arch: arch.to_string(),
                version_check_warn: true,
                ..LoadOptions::default()
            };
            matches!(hydo_manifest::load_until_images(&c.doc, &opts).0, Ok(l) if hydo_manifest::check_document(&l).is_ok())
        });
        if !valid {
            fails.push(format!("{}: not a valid document", c.name));
            continue;
        }
        let m = match migrate::read_project(&c.doc) {
            Ok(p) => model::to_json(&p),
            Err(e) => {
                fails.push(format!("{}: {}", c.name, e.message));
                continue;
            }
        };
        let mp = c.dir.join("model.json");
        if !mp.exists() && bless() {
            std::fs::write(&mp, pretty(&m)).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&mp).unwrap_or_default();
        if want != pretty(&m) {
            fails.push(format!(
                "{}: reads into another model than {} now:\n{}",
                c.name,
                mp.display(),
                pretty(&m)
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// A retired spelling reads into the same model as its canonical spelling, and `hydo migrate`
/// rewrites the one into the other, byte for byte.
#[test]
fn retired_spellings_read_as_their_canonical_spelling() {
    let e = examples_dir();
    for (old, new) in [
        (
            "retired-history-check-in-test",
            "retired-history-check-in-test-migrated",
        ),
        ("plan-json-sdk", "plan-json-sdk-migrated"),
    ] {
        let doc = |n: &str| {
            cases()
                .into_iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("no case {n}"))
                .doc
        };
        let (a, b) = (doc(old), doc(new));
        assert_eq!(
            read_json(&e.join(old).join("model.json")),
            read_json(&e.join(new).join("model.json")),
            "{old} and {new} read into different models"
        );
        let text = std::fs::read_to_string(&a).unwrap();
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        let m = migrate::migrate_text(&text, &name, a.parent()).unwrap();
        assert!(m.changed);
        assert_eq!(
            m.text,
            std::fs::read_to_string(&b).unwrap(),
            "hydo migrate {old}"
        );
        let again = migrate::migrate_text(&m.text, &name, a.parent()).unwrap();
        assert!(!again.changed, "{new} is canonical");
    }
}

/// `hydo migrate` on every example: it means the same afterwards (migrate checks that itself),
/// and a second run changes nothing. The YAML writer reads back to the same data on each one.
#[test]
fn migrate_is_safe_on_every_example() {
    for c in cases() {
        let text = std::fs::read_to_string(&c.doc).unwrap();
        let name = c.doc.file_name().unwrap().to_string_lossy().into_owned();
        let m = migrate::migrate_text(&text, &name, c.doc.parent())
            .unwrap_or_else(|e| panic!("{}: {}", c.name, e.message));
        let again = migrate::migrate_text(&m.text, &name, c.doc.parent()).unwrap();
        assert!(!again.changed, "{}: a second migrate changed it", c.name);
        let doc = hydo_manifest::parse_text(&text, &name, true).unwrap();
        let back = hydo_manifest::yaml::parse_yaml(&migrate::emit_yaml(&doc), &name).unwrap();
        assert_eq!(back, doc, "{}: the YAML writer", c.name);
    }
}

#[test]
fn a_newer_format_is_refused_with_the_format_it_needs() {
    let d = std::env::temp_dir().join(format!("hydo-format-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("hydo.yaml");
    std::fs::write(
        &p,
        "format: 2\nhydo_version: \">=0.3\"\nname: x\nsomething_new: {a: 1}\n",
    )
    .unwrap();
    let e = hydo_manifest::load(&p, &LoadOptions::default()).unwrap_err();
    assert!(
        e.message.starts_with("format: this file is written in project format 2, and this hydo (")
            && e.message.contains("reads project format 1 only; install a newer hydo, one that reads format 2; the file asks for hydo_version >=0.3"),
        "{}",
        e.message
    );
    std::fs::write(
        &p,
        "name: x\nworkload: {steps: 1, run: x}\ninvariants: [{id: a, cmd: x}]\n",
    )
    .unwrap();
    hydo_manifest::load(&p, &LoadOptions::default()).expect("no format: is format 1");
    let _ = std::fs::remove_dir_all(&d);
}

/// `hydo build --check` of the folder `hydo init` writes passes the format check and names both
/// placeholders, which fail every run (0.3.2: a reader of the quickstart took the OK for a
/// finished project). A project with its own files gets no such warning.
#[test]
fn the_check_names_the_placeholders_of_hydo_init() {
    use hydo_manifest::{check_summary, load_until_images};
    let top = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // the template with its name filled in, as `hydo init` writes it
    let d = std::env::temp_dir().join(format!("hydo-init-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let src = top.join("hydo/guide/init/hydo");
    for sub in ["", "workload", "checks", "regressions"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
        for e in std::fs::read_dir(src.join(sub)).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_file() {
                let t = std::fs::read_to_string(e.path())
                    .unwrap()
                    .replace("@NAME@", "demo")
                    .replace("@CLOCK_START@", "1767225600")
                    .replace("@CLOCK_DATE@", "2026-01-01");
                let to = d.join(sub).join(e.file_name());
                std::fs::write(&to, t).unwrap();
                let mode = std::fs::metadata(e.path()).unwrap().permissions();
                std::fs::set_permissions(&to, mode).unwrap();
            }
        }
    }
    let init = d.join("hydo.yaml");
    let l = load_until_images(&init, &LoadOptions::default()).0.unwrap();
    let s = check_summary("hydo/hydo.yaml", &l);
    for p in [
        "hydo/workload/parallel_driver_todo",
        "hydo/checks/final_todo",
    ] {
        assert!(
            s.iter()
                .any(|x| x.starts_with("hydo-build: warning: ") && x.contains(p)),
            "{p}: {s:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
    let real = top.join("hydo/onboard/examples/lago/hydo/hydo.yaml");
    let l = load_until_images(&real, &LoadOptions::default()).0.unwrap();
    let s = check_summary("hydo/hydo.yaml", &l);
    assert!(!s.iter().any(|x| x.contains("warning")), "{s:?}");
}

/// The same for the placeholders of a fresh `hydo compose` import: its `workload.run` and its
/// TODO invariant (0.3.3: a reader of docs/ONBOARD.md got an OK for an import with both).
#[test]
fn the_check_names_the_placeholders_of_hydo_compose() {
    use hydo_manifest::{check_summary, load_until_images};
    let top = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let gen = top.join("hydo/onboard/examples/compose-voting/hydo.generated.yaml");
    let l = load_until_images(&gen, &LoadOptions::default()).0.unwrap();
    let s = check_summary("hydo/hydo.yaml", &l);
    for p in [
        "workload.run is the placeholder",
        "todo.acked_operations_survive",
    ] {
        assert!(
            s.iter()
                .any(|x| x.starts_with("hydo-build: warning: ") && x.contains(p)),
            "{p}: {s:?}"
        );
    }
    let real = top.join("hydo/onboard/examples/compose-voting/hydo.yaml");
    let l = load_until_images(&real, &LoadOptions::default()).0.unwrap();
    let s = check_summary("hydo/hydo.yaml", &l);
    assert!(!s.iter().any(|x| x.contains("placeholder")), "{s:?}");
}

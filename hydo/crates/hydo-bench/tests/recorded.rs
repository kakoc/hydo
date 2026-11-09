//! `hydo/onboard/recorded.json`, the list of every recorded example run (Milestone 164): it reads
//! into the model `hydo-bench rebaseline` uses, it is kept in the form `--write` writes (so a
//! rewrite changes only values), every verdict has a class, every stage it names is a stage of
//! `hydo/onboard/rebaseline/stage.sh`, and every manifest it builds from the tree is there.

use hydo_bench::recorded;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn list() -> (String, serde_json::Value) {
    let p = root().join("hydo/onboard/recorded.json");
    let text = std::fs::read_to_string(&p).expect("hydo/onboard/recorded.json");
    let doc = serde_json::from_str(&text).expect("JSON");
    (text, doc)
}

#[test]
fn reads_and_keeps_its_form() {
    let (text, doc) = list();
    assert_eq!(
        recorded::to_text(&doc),
        text,
        "recorded.json is not in serde_json's pretty form (rewrite it with hydo-bench rebaseline --write, or reformat it)"
    );
    let images = recorded::images(&doc).expect("the list's model");
    let count = |p: &str| {
        let i: Vec<_> = images.iter().filter(|i| i.platform == p).collect();
        (i.len(), i.iter().map(|i| i.runs.len()).sum::<usize>())
    };
    // docs/REBASELINE.md 13.1 + 14 (67 Mac schedules on 19 images) and 13.4's Mac examples;
    // 13.2 (126 x86_64 schedules on 37 images) and 13.4's Linux examples; M159's KVM table;
    // since 0.3.0 the release example's `disk=cut | run C=12` on each engine (the quickstarts
    // cite it) and the KVM exploration's third minimal schedule (REBASELINE.md section 18); since
    // the fingerprint epoch 2 the hvx exploration's new lying-disk minimal schedule (section 19);
    // since M206 the nats-flaky-8363 pair on several vCPUs (two images, four runs); the
    // qdrant-flaky-10200 images on hvx (two images, four runs) and on KVM (two images, four runs)
    assert_eq!(count("hvm"), (21, 76));
    assert_eq!(count("hvx"), (43, 141));
    assert_eq!(count("kvm"), (6, 17));
    for i in &images {
        assert!(
            i.sha256.len() == 64 && i.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "{}: the image's full sha256",
            i.id
        );
        for r in &i.runs {
            let (class, _) = recorded::verdict_key(&r.verdict);
            assert!(
                ["OK", "VIOLATION", "ERROR"].contains(&class.as_str()),
                "{} `{}`: verdict {:?}",
                i.id,
                r.schedule,
                r.verdict
            );
            assert_eq!(r.fp.len(), 16, "{} `{}`: fp", i.id, r.schedule);
            assert!(
                !r.schedule.contains('`') && r.schedule.contains('|'),
                "{} `{}`: not a schedule",
                i.id,
                r.schedule
            );
            let mut seen = std::collections::BTreeSet::new();
            assert!(
                i.runs.iter().all(|x| seen.insert(x.key())),
                "{}: a schedule listed twice with one vCPU count",
                i.id
            );
        }
    }
}

#[test]
fn stages_and_manifests_exist() {
    let (_, doc) = list();
    let images = recorded::images(&doc).unwrap();
    let stage = std::fs::read_to_string(root().join("hydo/onboard/rebaseline/stage.sh")).unwrap();
    for i in &images {
        // a stage is a case label of linux_prepare (hvx, kvm) or mac_prepare (hvm)
        let labelled = stage.lines().any(|l| {
            let l = l.trim_start();
            l.split(')')
                .next()
                .is_some_and(|lab| !lab.contains(' ') && lab.split('|').any(|s| s == i.stage))
                && l.contains(')')
        });
        assert!(labelled, "{}: stage {} is not in stage.sh", i.id, i.stage);
        // the manifests built from the tree's own folders (a stage may make the folder: the racy
        // composed plan, the generated-driver stack, the release's examples)
        let src = root().join(&i.dir);
        if src.is_dir() {
            assert!(
                src.join(&i.manifest).is_file(),
                "{}: no {}/{}",
                i.id,
                i.dir,
                i.manifest
            );
        } else {
            assert!(
                i.dir.starts_with("release/")
                    || i.dir.ends_with("-racy/hydo")
                    || i.dir.ends_with("/stackgen"),
                "{}: no folder {} in the tree",
                i.id,
                i.dir
            );
        }
    }
}

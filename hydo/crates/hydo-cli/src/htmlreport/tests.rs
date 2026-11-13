//! The page against recorded answers (`tests/golden/report/*.html`; `HYDO_BLESS=1 cargo test -p
//! hydo htmlreport` rewrites them, then review their diff like code), its size bound, and that
//! it loads nothing from anywhere.
//!
//! The inputs are `tests/fixtures/report/`: a real run of the Redis example on the Mac engine
//! (`disk=cut | run C=12 R=everysec`, a power cut that loses an acknowledged write) and its
//! passing neighbour (`R=always`), with the scratch paths replaced by `/work`; an exploration on
//! the explorer's scripted engine (hydo-explore's `tests/fake_engine.rs` engine, seeds 1-12, one
//! worker), with the consoles the page reads; and the recorded CI run of
//! `hydo/schema/examples/results/v1/mac-shop-ci-run/`. The page names directories as the
//! command line gave them, so the goldens hold no path of the machine that made them.

use super::*;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    crate_dir().join("tests/fixtures/report").join(name)
}

fn page_of(dir: &Path, shown: &str, against: Option<PathBuf>) -> String {
    let input = read_input(dir).expect("a results directory");
    let extra = Extra {
        against,
        diff: None,
        shown_dir: shown.into(),
    };
    render(&input, &extra).expect("a page")
}

fn golden(name: &str, got: &str) {
    let p = crate_dir().join("tests/golden/report").join(name);
    // HYDO_BLESS is a test-only switch (kind Test in hydo_config's registry), read here in the
    // test's own code as hydo_config's docs say; tests/inventory.rs keeps direct reads by name
    // out of crates' src, so the test scans the environment instead.
    let bless = std::env::vars().any(|(k, v)| k == "HYDO_BLESS" && !v.is_empty() && v != "0");
    if bless {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (HYDO_BLESS=1 writes it)", p.display()));
    if want != got {
        let line = want
            .lines()
            .zip(got.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(want.lines().count().min(got.lines().count()));
        panic!(
            "{} differs from the page made now, first at line {}:\n  golden: {}\n  now:    {}\n(HYDO_BLESS=1 rewrites it)",
            p.display(),
            line + 1,
            want.lines().nth(line).unwrap_or("<end>"),
            got.lines().nth(line).unwrap_or("<end>")
        );
    }
}

/// Nothing on the page is fetched: no external script, style sheet, font, image or frame, and no
/// address of another site at all.
fn loads_nothing(html: &str) {
    let low = html.to_lowercase();
    for bad in [
        "http://",
        "https://",
        "<link",
        "<iframe",
        "<img",
        "<object",
        "<embed",
        "src=",
        "@import",
        "url(",
        "@font-face",
        "//cdn",
        "fetch(",
        "xmlhttprequest",
    ] {
        assert!(!low.contains(bad), "the page contains {bad:?}");
    }
    // the one style sheet and the one script are inline
    assert_eq!(low.matches("<style>").count(), 1);
    assert_eq!(low.matches("<script>").count(), 1);
    assert!(low.starts_with("<!doctype html>"));
}

fn small(html: &str, kib: usize) {
    assert!(
        html.len() <= kib * 1024,
        "the page has {} bytes, more than {kib} KiB",
        html.len()
    );
}

#[test]
fn a_power_cut_run_against_its_passing_neighbour() {
    let html = page_of(
        &fixture("mac-redis-cut"),
        "hydo-out/run-cut",
        Some(fixture("mac-redis-always")),
    );
    loads_nothing(&html);
    small(&html, 64);
    // the failure, the commands that replay it, the timeline and the first difference
    for want in [
        "redis: a check did not hold",
        "disk=cut | run C=12 R=everysec",
        "hydo repro hydo-out/run-cut",
        "hydo run --image /work/redis/hydo-out/explore-ob-redis-arm64.cpio.gz &quot;disk=cut | run C=12 R=everysec&quot;",
        "POWER CUT before step 12",
        "with R=always instead of R=everysec",
        "Apple M2 Pro",
        "fingerprint epoch 2",
        "never reached",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    golden("run.html", &html);
}

#[test]
fn a_scripted_exploration() {
    let html = page_of(&fixture("scripted-explore"), "hydo-out/explore", None);
    loads_nothing(&html);
    small(&html, 96);
    for want in [
        "disk=lying:1 dseed=63cbe1e459320dd7 | run C=11",
        "with disk=none instead of disk=lying:1",
        "Against its closest passing neighbour",
        "not shrunk",
        "New error lines",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    golden("exploration.html", &html);
}

#[test]
fn a_ci_run() {
    let dir = crate_dir().join("../../schema/examples/results/v1/mac-shop-ci-run");
    let html = page_of(&dir, "hydo-out/ci", None);
    loads_nothing(&html);
    small(&html, 64);
    for want in [
        "shop: the CI run failed, 1 new finding",
        "Found on this commit; the base commit passes the same schedule.",
        "its minimal schedule passes on the base commit&#39;s image",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    golden("ci.html", &html);
}

#[test]
fn a_long_timeline_keeps_its_main_events_within_the_bound() {
    let ev = |i: usize, kind: &str| TimelineEvent {
        point: Some(format!("run:{i}")),
        conversation: "run".into(),
        stop: Some(i as u64),
        at: Some(i as u64 * 10),
        kind: kind.into(),
        text: format!("{kind} {i} {}", "x".repeat(2000)),
        service: None,
        details: None,
    };
    let mut evs: Vec<TimelineEvent> = (0..5000).map(|i| ev(i, "guest")).collect();
    for i in 0..50 {
        evs.insert(i * 100, ev(i * 100, "fault"));
    }
    let html = timeline_html("t", &evs, "the rest: hydo inspect events DIR");
    let rows = html.matches("<tr><td").count() + html.matches("<tr class=\"minor\">").count();
    assert_eq!(rows, MAX_EVENTS);
    // every fault is kept, and each text is cut
    assert_eq!(html.matches(">fault</span>").count(), 50);
    assert!(html.contains("The run has 5050 events; this page keeps 400 of them."));
    small(&html, 400);
}

#[test]
fn text_from_the_run_is_escaped() {
    let mut run = Run::new(
        "disk=none | run W=:<script>alert(1)</script>",
        hydo_results::Verdict {
            kind: "violation".into(),
            text: "HYDO-ORACLE: VIOLATION [a] \"<b>\" & 'c'".into(),
            ..Default::default()
        },
        "0123456789abcdef",
    );
    run.image = Some(Image {
        path: "/x/explore-ob-shop-disk.img".into(),
        ..Default::default()
    });
    let d = std::env::temp_dir().join(format!("hydo-htmlreport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    hydo_results::write_document(&d.join(hydo_results::RUN_DOC), &run).unwrap();
    let html = page_of(&d, "d", None);
    let _ = std::fs::remove_dir_all(&d);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("&quot;&lt;b&gt;&quot; &amp; &#39;c&#39;"));
    assert!(html.contains("shop: a check did not hold"));
    loads_nothing(&html);
}

#[test]
fn how_a_neighbour_was_made() {
    let a = "disk=cut | run C=12 R=everysec";
    assert_eq!(
        schedule_change(a, "disk=cut | run C=12 R=always"),
        "with R=always instead of R=everysec"
    );
    assert_eq!(
        schedule_change(a, "disk=cut | run C=12"),
        "without R=everysec"
    );
    assert_eq!(
        schedule_change(a, "disk=none | run C=12 R=everysec"),
        "with disk=none instead of disk=cut"
    );
    assert_eq!(
        schedule_change("disk=none | run", "disk=none | run K=redis@5+2"),
        "with K=redis@5+2 added"
    );
    assert_eq!(schedule_change(a, a), "the same schedule");
}

#[test]
fn an_image_name_names_its_system() {
    assert_eq!(
        system_of("/w/hydo-out/explore-ob-redis-arm64.cpio.gz").as_deref(),
        Some("redis")
    );
    assert_eq!(
        system_of("explore-ob-stack-fix-disk.img").as_deref(),
        Some("stack-fix")
    );
    assert_eq!(system_of("scripted-disk.img"), None);
}

#[test]
fn versions_are_shown_when_the_results_carry_them() {
    let run = Run::new(
        "disk=cut | run C=12 R=everysec",
        hydo_results::Verdict {
            kind: "violation".into(),
            text: "HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:0 lost".into(),
            ..Default::default()
        },
        "0123456789abcdef",
    );
    let d = std::env::temp_dir().join(format!("hydo-htmlreport-v-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    hydo_results::write_document(&d.join(hydo_results::RUN_DOC), &run).unwrap();
    let without = page_of(&d, "d", None);
    assert!(!without.contains("id=\"versions\""));
    std::fs::write(
        d.join("versions.json"),
        r#"{"findings":[{"finding":"f1:invariant:d4f25624c2","introduced":"7.0.0",
            "last_good":"6.2.14","still_present":["7.2.4","7.4.0"],
            "tested":[{"version":"6.2.14","verdict":"ok"},{"version":"7.0.0","verdict":"violation"}]}]}"#,
    )
    .unwrap();
    let html = page_of(&d, "d", None);
    let _ = std::fs::remove_dir_all(&d);
    assert!(html.contains("id=\"versions\""));
    for want in [
        "Introduced in</dt><dd>7.0.0",
        "The last version without it</dt><dd>6.2.14",
        "Still present in</dt><dd>7.2.4, 7.4.0",
        "<td>6.2.14</td><td>ok</td>",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    loads_nothing(&html);
}

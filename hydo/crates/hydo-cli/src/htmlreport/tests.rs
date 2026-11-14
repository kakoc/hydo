//! The page against recorded answers (`tests/golden/report/*.html`; `HYDO_BLESS=1 cargo test -p
//! hydo htmlreport` rewrites them, then review their diff like code), its size bound, that it
//! loads nothing from anywhere, and that its plain part (everything above the folded details)
//! uses none of hydo's own words.
//!
//! The inputs are `tests/fixtures/report/`: a real run of the Redis example on the Mac engine
//! (`disk=cut | run C=12 R=everysec`, a power cut that loses an acknowledged write) and its
//! passing neighbour (`R=always`), with the scratch paths replaced by `/work`, in a project
//! folder with the example's `hydo.yaml` (`redis/hydo.yaml`, `redis/hydo-out/run-*`); an
//! exploration on
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

/// The words of hydo's own machinery that the plain part of a page never shows. Commands and
/// schedules (`<code>`, `<pre>`) and the folded details are left out of the check.
const JARGON: [&str; 10] = [
    "fingerprint",
    "epoch",
    "engine",
    "schedule",
    "token",
    "invariant",
    "oracle",
    "signature",
    "class",
    "seed",
];

fn plain_words(html: &str) {
    let mut t = html.to_string();
    if let Some(i) = t.find("<details class=\"share\"") {
        t.truncate(i);
    }
    let cut_between = |t: &mut String, a: &str, b: &str| {
        while let Some(i) = t.find(a) {
            let j = t[i..].find(b).map(|j| i + j + b.len()).unwrap_or(t.len());
            t.replace_range(i..j, " ");
        }
    };
    cut_between(&mut t, "<head>", "</head>");
    cut_between(&mut t, "<pre", "</pre>");
    cut_between(&mut t, "<code>", "</code>");
    cut_between(&mut t, "<", ">");
    let low = t.to_lowercase();
    for w in low.split(|c: char| !c.is_ascii_alphanumeric()) {
        for j in JARGON {
            assert!(
                w != j && w != format!("{j}s") && w != format!("{j}es"),
                "the plain part of the page uses {w:?}"
            );
        }
    }
    for banned in ["knob", "\u{2014}"] {
        assert!(!low.contains(banned), "the plain part uses {banned:?}");
    }
}

/// The part above the folded details, commands included, names no path of the machine that made
/// the results, and every link of the page goes to an element the page has.
fn no_machine_paths_and_links_resolve(html: &str) {
    let plain = &html[..html.find("<details class=\"share\"").unwrap_or(html.len())];
    for bad in ["/work/", "/Users/", "/home/", "/private/", "/tmp/"] {
        assert!(!plain.contains(bad), "the plain part names {bad:?}");
    }
    let mut rest = html;
    while let Some(i) = rest.find("href=\"#") {
        let r = &rest[i + 7..];
        let id = &r[..r.find('"').unwrap()];
        assert!(
            html.contains(&format!("id=\"{id}\"")),
            "the link #{id} goes nowhere"
        );
        rest = r;
    }
}

#[test]
fn a_power_cut_run_against_its_passing_neighbour() {
    let html = page_of(
        &fixture("redis/hydo-out/run-cut"),
        "hydo-out/run-cut",
        Some(fixture("redis/hydo-out/run-always")),
    );
    loads_nothing(&html);
    small(&html, 64);
    plain_words(&html);
    no_machine_paths_and_links_resolve(&html);
    // the plain part, as the approved mockup: concrete facts of this run only
    for want in [
        "<h1>Redis lost data it had confirmed, after a power cut, with Redis appendfsync set to <code>everysec</code></h1>",
        "Your workload ran 12 steps and Redis confirmed all 12. Then the power was cut before the 13th step. When Redis started again, <code>key k:0</code> was gone.",
        "<li>The power goes out after 12 steps, before the 13th step.</li>",
        "<li>After the restart, your check finds <code>key k:0 lost</code>.</li>",
        "Expected for this setting",
        "A second run with Redis appendfsync at <code>always</code> instead of <code>everysec</code> passed every check.",
        "keep Redis appendfsync at <code>always</code>",
        // what hydo saw: the four lines of the mockup
        "Redis confirmed <code>k:0</code> … <code>k:11</code> (12 steps)",
        "The power was cut before the 13th step",
        "The machine started again, and Redis started from what was on its disk",
        "<code>key k:0 lost</code> <span class=\"muted\">(the check stops at the first one)</span>",
        "In your project folder (<code>redis</code>), this failure repeats exactly, every time you run it:",
        "<pre class=\"cmd\">hydo run &quot;disk=cut | run C=12 R=everysec&quot;</pre>",
        "In hydo's words: <code>disk=cut</code> = a power cut loses unflushed writes, <code>C=12</code> = power cut before the 13th step, <code>R=everysec</code> = Redis appendfsync set to <code>everysec</code>.",
        "<code>hydo inspect start hydo-out/run-cut --at end</code>",
        "Details for a bug report",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    // the check's own words once above the details
    let plain = &html[..html.find("<details class=\"share\"").unwrap()];
    assert_eq!(plain.matches("every SET the client saw").count(), 1);
    // the details keep what the page showed before
    let details = &html[html.find("<details class=\"share\"").unwrap()..];
    for want in [
        "disk=cut | run C=12 R=everysec",
        "hydo repro hydo-out/run-cut",
        "hydo run --image /work/redis/hydo-out/explore-ob-redis-arm64.cpio.gz &quot;disk=cut | run C=12 R=everysec&quot;",
        "POWER CUT before step 12",
        "with R=always instead of R=everysec",
        "a2b8d71c0c467d15",
        "Apple M2 Pro",
        "fingerprint epoch 2",
        "never reached",
    ] {
        assert!(details.contains(want), "the details lack {want:?}");
    }
    golden("run.html", &html);
}

#[test]
fn a_scripted_exploration() {
    let html = page_of(&fixture("scripted-explore"), "hydo-out/explore", None);
    loads_nothing(&html);
    small(&html, 96);
    plain_words(&html);
    no_machine_paths_and_links_resolve(&html);
    for want in [
        "<h1>hydo found 3 problems in your system</h1>",
        "4 runs failed, in 2 different ways.",
        "Start here",
        "The run stopped early after disk errors",
        "The command also keeps this run&#39;s network faults.",
        "<p><b>Is it a bug?</b> Nothing, unless your production disks",
        "<a href=\"#f2\">Open this failure →</a>",
        "<a href=\"#p1\">problem 1</a>",
        "What held",
        "With a working disk and your safe settings, none of the 7 runs failed",
        "hydo explore --image scripted-disk.img --seeds 13-112",
        "hydo ci accept f1:invariant:",
        // the details
        "disk=lying:1 dseed=63cbe1e459320dd7 | run C=11",
        "with disk=none instead of disk=lying:1",
        "Against its closest passing neighbour",
        "not shrunk",
        "New error lines",
        "Your checks that held in every run",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    // the cards hold no general explanation: no label meaning, no glosses
    let plain = &html[..html.find("<details class=\"share\"").unwrap()];
    for gone in [
        "In hydo&#39;s words",
        "In CI, a new finding",
        "The runs in detail",
    ] {
        assert!(!plain.contains(gone), "the cards still say {gone:?}");
    }
    // the problems in order of attention: worth a look before expected
    let look = html.find("The run stopped early").unwrap();
    let expected = html.find("failed your check with a faulty disk").unwrap();
    assert!(look < expected);
    golden("exploration.html", &html);
}

#[test]
fn a_ci_run() {
    let dir = crate_dir().join("../../schema/examples/results/v1/mac-shop-ci-run");
    let html = page_of(&dir, "hydo-out/ci", None);
    loads_nothing(&html);
    small(&html, 64);
    plain_words(&html);
    no_machine_paths_and_links_resolve(&html);
    for want in [
        "<h1>This change brings 1 new problem to Shop</h1>",
        "New in this change",
        "The base commit passes the same run, so this change brought it.",
        "If it is expected: <code>hydo ci accept f1:invariant:d4f25624c2 --reason &quot;…&quot;</code>",
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
    assert!(html.contains("Shop failed your check"));
    assert!(html.contains("found <code>&quot;&lt;b&gt;&quot; &amp; &#39;c&#39;</code>"));
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
    // `hydo versions <dir>` writes its answer into `<dir>/versions/versions.json`
    let o = |version: &str, outcome: &str, fp: Option<&str>, replay: Option<u64>| {
        hydo_results::VersionOutcome {
            version: version.into(),
            outcome: outcome.into(),
            fingerprint: fp.map(str::to_string),
            verdict: fp.map(|_| "HYDO-ORACLE: OK invariants=3 held".to_string()),
            replay,
            ..Default::default()
        }
    };
    let doc = hydo_results::VersionsDoc {
        engine: hydo_results::Engine {
            backend: Some("hvm".into()),
            ..Default::default()
        },
        from: "d".into(),
        findings: vec![hydo_results::Versions {
            schedule: "disk=cut | run C=12 R=everysec".into(),
            key: Some("f1:invariant:d4f25624c2".into()),
            title: Some("VIOLATION [redis.acked_writes_survive] key k:# lost".into()),
            source: "param version".into(),
            versions: vec![
                o("6.2.14", "passes", Some("1111111111111111"), Some(2)),
                o("7.0.0", "reproduced", Some("2222222222222222"), Some(3)),
                o("7.2.4", "not-replayed", None, None),
                o("7.4.0", "reproduced", Some("3333333333333333"), Some(1)),
            ],
            first_bad: Some("7.0.0".into()),
            last_good: Some("6.2.14".into()),
            last_bad: Some("7.4.0".into()),
            still_present_in_newest: Some(true),
            replays: 3,
            summary: "introduced in 7.0.0 (6.2.14 passes); still present in 7.4.0, the newest \
                      version tried"
                .into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::create_dir_all(d.join("versions")).unwrap();
    hydo_results::write_document(&d.join("versions").join(hydo_results::VERSIONS_DOC), &doc)
        .unwrap();
    let html = page_of(&d, "d", None);
    let _ = std::fs::remove_dir_all(&d);
    assert!(html.contains("id=\"versions\""));
    for want in [
        "Introduced in</dt><dd>7.0.0",
        "The last version without it</dt><dd>6.2.14",
        "Still present in the newest version</dt><dd>yes (7.4.0)",
        "<td>6.2.14</td><td><span class=\"badge ok\">passes</span></td>",
        "<td>7.2.4</td><td>not replayed</td>",
        "introduced in 7.0.0 (6.2.14 passes)",
    ] {
        assert!(html.contains(want), "the page lacks {want:?}");
    }
    loads_nothing(&html);
}

/// `hydo report` in the terminal (Milestone 230) says what the page says: the same title, the
/// same problems in the same order with their labels, sentences and commands, what held and what
/// to run next, in words without hydo's own (the commands aside), within 80 columns apart from
/// the command lines. The end of `hydo explore` in a terminal counts the same problems.
#[test]
fn the_terminal_says_what_the_page_says() {
    let dir = fixture("scripted-explore");
    let html = page_of(&dir, "hydo-out/explore", None);
    let (doc, events) = match hydo_results::read_dir(&dir).unwrap() {
        hydo_results::Results::Exploration {
            doc: Some(doc),
            events,
            ..
        } => (doc, events),
        _ => panic!("an exploration"),
    };
    let text = text::exploration(&dir, &doc, events.as_ref(), "hydo-out/explore");
    let st = exploration_story(&dir, &doc, events.as_ref(), "hydo-out/explore");
    assert!(text.starts_with(&format!("{} (4 of 12 runs failed)\n", st.title)));
    assert!(html.contains(&format!("<h1>{}</h1>", esc(&st.title))));
    let mut at = 0;
    for (n, it) in st.items.iter().enumerate() {
        let head = format!("{}. {}", n + 1, page::plain(&it.problem.title));
        let head: String = head
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ");
        let i = text[at..]
            .find(&head)
            .unwrap_or_else(|| panic!("the terminal lacks {head:?} after byte {at}"));
        at += i + head.len();
        assert!(html.contains(&md(&it.problem.title)));
        assert!(
            it.link == "errors" || text.contains(&format!("   See it again: {}\n", it.command))
        );
    }
    for want in [
        "Faults tried: ",
        "What held: ",
        "Next:\n",
        "Each failure's timeline, as one web page: hydo report --html\n",
        "More runs: hydo explore --image scripted-disk.img --seeds 13-112\n",
        "hydo ci accept f1:invariant:",
    ] {
        assert!(text.contains(want), "the terminal lacks {want:?}");
    }
    // the words are the page's: no hydo jargon outside the commands
    let words: String = text
        .lines()
        .map(|l| l.split(": hydo ").next().unwrap_or(""))
        .map(|l| format!("<p>{}</p>", esc(l)))
        .collect();
    plain_words(&words);
    for l in text.lines() {
        assert!(
            l.chars().count() <= 80 || l.contains(": hydo "),
            "a line wider than 80 columns: {l:?}"
        );
    }
    // a directory other than the default one is named in the commands
    let other = text::exploration(&dir, &doc, events.as_ref(), "runs/x");
    assert!(other.contains("hydo report --html runs/x\n"));
    // the end of hydo explore
    let end = text::explore_end(&dir, "hydo-out/explore").expect("an exploration");
    assert_eq!(
        end,
        "hydo ran your system 12 times: 4 failed, in 2 different ways. Each failing run\n\
         failed again the same way when hydo replayed it. Your services also wrote error\n\
         lines that your list of known errors does not have.\n\
         See them: hydo report   (as one web page: hydo report --html)\n"
    );
    golden("exploration.txt", &text);
}

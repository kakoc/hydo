//! `comment.md` and `hydo ci comment`: the pull-request comment (docs/design/ci.md section 5.1,
//! Milestone 286). It shows only what the change did against its base: findings `new`,
//! `reappeared`, `changed` and `fixed`. Each one gets its minimal schedule, the one line that
//! replays it, and the `hydo ci accept` line for it. Findings already on the base and accepted
//! findings are only counted; `report.md` and the job summary list them.
//!
//! The comment is made from `ci-run.json` alone, so a trusted job can make it from the recorded
//! run of an untrusted one (the `workflow_run` recorder of `hydo guide ci`). Every text from the
//! run is data: it goes into code spans whose fences are longer than any backtick run in it, so
//! no title or schedule can end its span and add Markdown, links or mentions. The comment holds
//! no marker: the adapter puts its own first line on it to find it again (action/comment.sh).

use super::report::wall;
use hydo_results::{ci_state, CiFinding, CiRun};
use std::fmt::Write as _;

/// The most findings of one state the comment lists; the rest are counted.
pub const MAX_PER_STATE: usize = 10;
/// The longest title or schedule the comment shows.
const MAX_TEXT: usize = 400;

/// A code span that no text can end early: a fence one backtick longer than the longest run of
/// backticks in the text, with spaces inside when the text touches a backtick.
pub fn span(s: &str) -> String {
    let s: String = s.replace(['\n', '\r'], " ");
    let s = if s.chars().count() > MAX_TEXT {
        let mut t: String = s.chars().take(MAX_TEXT - 1).collect();
        t.push('…');
        t
    } else {
        s
    };
    let fence = "`".repeat(longest_run(&s, '`') + 1);
    if s.starts_with('`') || s.ends_with('`') || fence.len() > 1 {
        format!("{fence} {s} {fence}")
    } else {
        format!("{fence}{s}{fence}")
    }
}

fn longest_run(s: &str, c: char) -> usize {
    let (mut best, mut n) = (0, 0);
    for x in s.chars() {
        if x == c {
            n += 1;
            best = best.max(n);
        } else {
            n = 0;
        }
    }
    best
}

/// A fenced block of shell text that no text can end early.
fn block(s: &str) -> String {
    let fence = "`".repeat(longest_run(s, '`').max(2) + 1);
    format!("{fence}sh\n{s}\n{fence}\n")
}

/// The command that replays a schedule in the project's folder.
pub fn replay_line(r: &CiRun, schedule: &str) -> String {
    let mut w = vec!["hydo run".to_string()];
    if r.engine.backend.as_deref() == Some("kvm") {
        w.push("--backend kvm".into());
    }
    let s = schedule.replace(['\n', '\r'], " ");
    // the explorer's schedules need no shell quoting inside double quotes; anything else is
    // quoted for the shell
    if s.chars().any(|c| matches!(c, '"' | '$' | '`' | '\\' | '!')) {
        w.push(crate::inspect::work::quote(&s));
    } else {
        w.push(format!("\"{s}\""));
    }
    w.join(" ")
}

/// The line that accepts a finding.
pub fn accept_line(key: &str) -> String {
    format!("hydo ci accept {key} --reason \"<why it is expected>\"")
}

fn short(c: &str) -> String {
    c.chars().take(10).collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A finding's state without its acceptance.
fn underlying(f: &CiFinding) -> &str {
    match &f.accepted {
        Some(a) if f.state == ci_state::ACCEPTED => &a.state,
        _ => &f.state,
    }
}

/// The finding key a `changed` finding fails with on this commit (the one to accept).
fn changed_key(f: &CiFinding) -> Option<String> {
    if f.source.as_deref().is_none_or(|s| s == "explored") {
        return Some(f.key.clone());
    }
    f.replays
        .iter()
        .filter(|r| r.on == "head")
        .find_map(|r| r.key.clone())
}

fn item(o: &mut String, r: &CiRun, n: usize, f: &CiFinding, state: &str) {
    let _ = writeln!(o, "**{n}.** {}\n", span(&f.title));
    // Milestone 387: a finding that the exploration had not shrunk shows its failing run
    if f.not_shrunk == Some(true) {
        let _ = writeln!(
            o,
            "- Schedule (not shrunk: the failing run's): {}",
            span(&f.minimal)
        );
    } else {
        let _ = writeln!(o, "- Minimal schedule: {}", span(&f.minimal));
    }
    match state {
        ci_state::FIXED => {
            let _ = writeln!(
                o,
                "- It passes on this commit. To see it fail, replay it on the base commit's \
                 image (the project folder at the base commit, after `hydo build`):\n"
            );
        }
        ci_state::CHANGED => {
            let other = f
                .replays
                .iter()
                .find_map(|x| x.verdict.as_deref())
                .map(hydo_explore::report::sig_key)
                .unwrap_or_default();
            let on_base = f.replays.first().is_some_and(|x| x.on == "base");
            if !other.is_empty() {
                let _ = writeln!(
                    o,
                    "- {} {}",
                    if on_base {
                        "The base commit fails it differently:"
                    } else {
                        "This commit fails it differently:"
                    },
                    span(&other)
                );
            }
            let _ = writeln!(
                o,
                "- Replay it in the project folder, after `hydo build`:\n"
            );
        }
        _ => {
            let _ = writeln!(
                o,
                "- Replay it in the project folder, after `hydo build`:\n"
            );
        }
    }
    for l in block(&replay_line(r, &f.minimal)).lines() {
        let _ = writeln!(o, "  {l}");
    }
    if let Some(at) = f
        .fixed_at
        .as_deref()
        .filter(|_| state == ci_state::REAPPEARED)
    {
        let _ = writeln!(
            o,
            "- The history recorded it fixed at {}.",
            span(&short(at))
        );
    }
    if let Some(d) = &f.diff {
        let _ = writeln!(
            o,
            "- Where the base and this commit first differ on it: {}",
            span(d)
        );
    }
    let key = match state {
        ci_state::FIXED => None,
        ci_state::CHANGED => changed_key(f),
        _ => Some(f.key.clone()),
    };
    if let Some(k) = key {
        let _ = writeln!(o, "- If it is expected: {}", span(&accept_line(&k)));
    }
    o.push('\n');
}

/// The headline: what the change did.
fn headline(r: &CiRun, counts: &[(&str, usize)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (s, n) in counts {
        if *n == 0 {
            continue;
        }
        parts.push(match *s {
            ci_state::NEW => plural(*n, "new finding", "new findings"),
            ci_state::REAPPEARED => format!("{n} reappeared"),
            ci_state::CHANGED => format!("{n} changed"),
            ci_state::FIXED => format!("{n} fixed"),
            _ => continue,
        });
    }
    if parts.is_empty() {
        parts.push("no change in findings".into());
    }
    let verdict = match r.outcome.exit {
        0 => "passed",
        1 => "failed",
        _ => "could not complete",
    };
    format!("hydo: {} ({verdict})", parts.join(", "))
}

/// `comment.md`.
pub fn markdown(r: &CiRun) -> String {
    let order = [
        (
            ci_state::NEW,
            "New in this change",
            "the base commit passes the same schedule, so this change brought it",
        ),
        (
            ci_state::REAPPEARED,
            "Back again",
            "the history recorded it fixed, it fails again here, and the base passes it",
        ),
        (
            ci_state::CHANGED,
            "Changed",
            "the base and this commit both fail the schedule, in different ways",
        ),
        (
            ci_state::FIXED,
            "Fixed by this change",
            "the base fails it, and its schedule passes here",
        ),
    ];
    let of = |s: &str| -> Vec<&CiFinding> {
        r.findings
            .iter()
            .filter(|f| f.state == s && f.accepted.is_none())
            .collect()
    };
    let counts: Vec<(&str, usize)> = order.iter().map(|(s, _, _)| (*s, of(s).len())).collect();
    let mut o = String::new();
    let _ = writeln!(o, "### {}\n", headline(r, &counts));
    let at = r
        .commit
        .as_deref()
        .map(|c| span(&short(c)))
        .unwrap_or_else(|| "the working tree".into());
    let compared = r.base.as_ref().filter(|b| b.findings_from != "none");
    match (compared, r.base.as_ref()) {
        (Some(b), _) => {
            let _ = writeln!(
                o,
                "Project {} at {at}, compared with {}{}. This comment shows only what the change \
                 did, and it is updated on every push.\n",
                span(&r.project.name),
                b.branch
                    .as_deref()
                    .map(|x| format!("{} at ", span(x)))
                    .unwrap_or_default(),
                b.commit
                    .as_deref()
                    .map(|c| span(&short(c)))
                    .unwrap_or_else(|| "the base".into())
            );
        }
        (None, Some(b)) => {
            let _ = writeln!(
                o,
                "Project {} at {at}. There is no comparison with a base ({}), so every finding \
                 below is listed as new.\n",
                span(&r.project.name),
                span(b.missing.as_deref().unwrap_or("none was given"))
            );
        }
        (None, None) => {
            let _ = writeln!(
                o,
                "Project {} at {at}. No base was given, so every finding below is listed as \
                 new.\n",
                span(&r.project.name)
            );
        }
    }
    let mut n = 0;
    for (s, title, why) in order {
        let fs = of(s);
        if fs.is_empty() {
            continue;
        }
        let _ = writeln!(
            o,
            "#### {title}\n\n{}: {why}.\n",
            plural(fs.len(), "finding", "findings")
        );
        for f in fs.iter().take(MAX_PER_STATE) {
            n += 1;
            item(&mut o, r, n, f, s);
        }
        if fs.len() > MAX_PER_STATE {
            let _ = writeln!(
                o,
                "And {} more; the job's report (`report.md`) lists them all.\n",
                fs.len() - MAX_PER_STATE
            );
        }
    }
    if n == 0 {
        o.push_str("This change brings no new finding, brings none back, and fixes none.\n\n");
    }
    if !r.image.set.is_empty() {
        let _ = writeln!(
            o,
            "Build the image with `hydo build{}` before a replay.\n",
            r.image
                .set
                .iter()
                .map(|s| format!(" --set {}", s.replace('`', "'")))
                .collect::<String>()
        );
    }
    // what the comment does not list
    let mut rest = Vec::new();
    let old = r
        .findings
        .iter()
        .filter(|f| {
            f.accepted.is_none()
                && matches!(f.state.as_str(), ci_state::PRESENT | ci_state::PRE_EXISTING)
        })
        .count();
    if old > 0 {
        rest.push(format!(
            "{} already on the base",
            plural(old, "finding", "findings")
        ));
    }
    let acc = r
        .findings
        .iter()
        .filter(|f| f.state == ci_state::ACCEPTED)
        .count();
    if acc > 0 {
        rest.push(format!("{acc} accepted in `hydo/known-findings.yaml`"));
    }
    let unv = r
        .findings
        .iter()
        .filter(|f| f.accepted.is_none() && f.state == ci_state::UNVERIFIED)
        .count();
    if unv > 0 {
        rest.push(format!("{unv} that could not be replayed"));
    }
    if !rest.is_empty() {
        let _ = writeln!(
            o,
            "Not caused by this change, so not listed here: {}. The job's report lists them.\n",
            rest.join(", ")
        );
    }
    // an accepted finding that this change makes new or brings back is still said once
    let acc_new: Vec<&CiFinding> = r
        .findings
        .iter()
        .filter(|f| {
            f.state == ci_state::ACCEPTED
                && matches!(underlying(f), ci_state::NEW | ci_state::REAPPEARED)
        })
        .collect();
    if !acc_new.is_empty() {
        let _ = writeln!(
            o,
            "Accepted, so it does not fail the check: {}.\n",
            acc_new
                .iter()
                .map(|f| format!("{} ({})", span(&f.title), underlying(f)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let bad: Vec<_> = r.regressions.iter().filter(|g| !g.as_expected).collect();
    if !bad.is_empty() {
        o.push_str("**Saved regressions not as expected**:\n\n");
        for g in bad.iter().take(MAX_PER_STATE) {
            let _ = writeln!(
                o,
                "- {}: expected {}, got {}",
                span(&g.name),
                span(&g.expect),
                span(
                    &g.error
                        .clone()
                        .or_else(|| g.verdict.clone())
                        .unwrap_or_default()
                )
            );
        }
        o.push('\n');
    }
    if r.budget.partial {
        let _ = writeln!(
            o,
            "Partial run: {} of {} seeds explored, so no finding is called fixed.\n",
            r.budget.completed.unwrap_or(0),
            r.budget.planned.unwrap_or(0)
        );
    }
    if !r.nondeterminism.is_empty() {
        let _ = writeln!(
            o,
            "hydo problem: {} gave another fingerprint when run again. This is a bug in hydo or \
             its environment, not a finding of the project.\n",
            plural(r.nondeterminism.len(), "failure", "failures")
        );
    }
    match r.outcome.exit {
        0 => {
            let _ = writeln!(
                o,
                "Result: passed (it fails on: {}).\n",
                r.outcome.fail_on.join(", ")
            );
        }
        1 => {
            let _ = writeln!(
                o,
                "Result: **failed** (it fails on: {}).\n",
                r.outcome.fail_on.join(", ")
            );
        }
        _ => o.push_str("Result: the run could not be completed; the job's log says why.\n\n"),
    }
    let b = &r.budget;
    let mut ran = format!(
        "{} on seeds {} ({}), {}, {}",
        plural(r.summary.runs as usize, "run", "runs"),
        b.completed_seeds
            .as_deref()
            .filter(|c| !c.is_empty())
            .unwrap_or(&b.seeds),
        b.strategy,
        plural(r.summary.replays as usize, "replay", "replays"),
        wall(r.summary.wall_s)
    );
    if b.seeds_from.as_deref() == Some("history") {
        if let Some(s) = b.seconds_per_seed {
            let _ = write!(
                ran,
                "; the seed count came from the history's {s:.1} s per seed"
            );
        }
    }
    let _ = writeln!(o, "<sub>{ran}. hydo {}.</sub>", r.hydo.version);
    o
}

/// `hydo ci comment <ci-run.json | DIR> [-o FILE]`.
pub fn cmd_comment(argv: &[String]) -> i32 {
    use crate::ctx::{die, take};
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: hydo ci comment <ci-run.json or directory> [-o FILE]
  the pull-request comment of a recorded CI run: only what the change did"
        );
        return 0;
    }
    let out = take(&mut args, "-o").or_else(|| take(&mut args, "--out"));
    if args.len() != 1 || args[0].starts_with('-') {
        die("usage: hydo ci comment <ci-run.json or directory> [-o FILE]");
    }
    let p = std::path::Path::new(&args[0]);
    let p = if p.is_dir() {
        p.join("ci-run.json")
    } else {
        p.to_path_buf()
    };
    let r = super::read_ci_run(&p).unwrap_or_else(|e| die(&e));
    let md = markdown(&r);
    match out {
        Some(f) => {
            std::fs::write(&f, &md).unwrap_or_else(|e| die(&format!("{f}: {e}")));
            println!("hydo ci comment: wrote {f}");
        }
        None => print!("{md}"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::{CiAccepted, CiBase, CiOutcome, CiReplay};

    fn f(key: &str, title: &str, state: &str) -> CiFinding {
        CiFinding {
            key: key.into(),
            kind: "invariant".into(),
            title: title.into(),
            state: state.into(),
            minimal: format!("disk=cut dseed=1 | run C={}", key.len()),
            source: Some("explored".into()),
            ..CiFinding::default()
        }
    }

    fn run() -> CiRun {
        CiRun {
            id: "github:1".into(),
            project: hydo_results::CiProject {
                name: "redis".into(),
                ..Default::default()
            },
            commit: Some("9c41e07aaaaaaaa".into()),
            base: Some(CiBase {
                branch: Some("main".into()),
                commit: Some("4b2d11abbbbbbbb".into()),
                findings_from: "explored".into(),
                ..Default::default()
            }),
            outcome: CiOutcome {
                exit: 1,
                fail_on: vec!["new".into(), "reappeared".into(), "regression".into()],
                ..Default::default()
            },
            ..CiRun::default()
        }
    }

    /// Only what the change did, each with its schedule, replay and accept line.
    #[test]
    fn the_comment_shows_only_what_the_change_did() {
        let mut r = run();
        r.findings
            .push(f("f1:invariant:aaaaaaaaaa", "VIOLATION [a] lost", "new"));
        r.findings
            .push(f("f1:invariant:bbbbbbbbbb", "VIOLATION [b] old", "present"));
        let mut fx = f("f1:invariant:cccccccccc", "VIOLATION [c] gone", "fixed");
        fx.source = Some("base".into());
        r.findings.push(fx);
        let mut back = f(
            "f1:invariant:dddddddddd",
            "VIOLATION [d] back",
            "reappeared",
        );
        back.fixed_at = Some("0123456789abcdef".into());
        r.findings.push(back);
        let mut acc = f("f1:invariant:eeeeeeeeee", "VIOLATION [e] known", "accepted");
        acc.accepted = Some(CiAccepted {
            reason: "documented".into(),
            state: "new".into(),
            ..Default::default()
        });
        r.findings.push(acc);
        let mut ch = f("f1:invariant:ffffffffff", "VIOLATION [f] base", "changed");
        ch.source = Some("base".into());
        ch.replays.push(CiReplay {
            on: "head".into(),
            schedule: ch.minimal.clone(),
            verdict: Some("VIOLATION [g] now 3".into()),
            key: Some("f1:invariant:gggggggggg".into()),
            ..Default::default()
        });
        r.findings.push(ch);
        r.engine.backend = Some("kvm".into());
        r.image.set = vec!["variant=buggy".into()];
        let m = markdown(&r);
        assert!(
            m.starts_with("### hydo: 1 new finding, 1 reappeared, 1 changed, 1 fixed (failed)\n"),
            "{m}"
        );
        assert!(m.contains("#### New in this change"));
        assert!(m.contains("**1.** `VIOLATION [a] lost`"));
        assert!(m.contains("hydo run --backend kvm \"disk=cut dseed=1 | run C=23\""));
        assert!(m.contains(
            "If it is expected: `hydo ci accept f1:invariant:aaaaaaaaaa --reason \"<why it is expected>\"`"
        ));
        assert!(m.contains("The history recorded it fixed at `0123456789`."));
        assert!(
            m.contains("This commit fails it differently: `VIOLATION [g] now #`"),
            "{m}"
        );
        assert!(m.contains("hydo ci accept f1:invariant:gggggggggg"));
        // the fixed one has no accept line
        let fixed = m.split("#### Fixed by this change").nth(1).unwrap();
        assert!(!fixed.contains("hydo ci accept"), "{fixed}");
        assert!(!m.contains("VIOLATION [b] old"));
        assert!(m.contains(
            "Not caused by this change, so not listed here: 1 finding already on the base, 1 accepted"
        ));
        assert!(m.contains("Accepted, so it does not fail the check: `VIOLATION [e] known` (new)."));
        assert!(m.contains("`hydo build --set variant=buggy`"));
        assert!(m.contains("Result: **failed**"));
    }

    #[test]
    fn nothing_changed_is_one_sentence() {
        let mut r = run();
        r.outcome.exit = 0;
        r.findings
            .push(f("f1:invariant:bbbbbbbbbb", "VIOLATION [b] old", "present"));
        let m = markdown(&r);
        assert!(
            m.starts_with("### hydo: no change in findings (passed)\n"),
            "{m}"
        );
        assert!(m.contains("This change brings no new finding, brings none back, and fixes none."));
    }

    /// A title from the guest cannot end its code span and add Markdown or mentions.
    #[test]
    fn text_from_a_run_stays_inside_its_span() {
        assert_eq!(span("a`b"), "`` a`b ``");
        assert_eq!(span("x``y"), "``` x``y ```");
        assert_eq!(span("plain"), "`plain`");
        assert_eq!(span("two\nlines"), "`two lines`");
        let mut r = run();
        r.findings.push(f(
            "f1:invariant:aaaaaaaaaa",
            "VIOLATION ``` [x](http://e) @someone",
            "new",
        ));
        let m = markdown(&r);
        assert!(
            m.contains("```` VIOLATION ``` [x](http://e) @someone ````"),
            "{m}"
        );
        let mut g = f("k", "t", "new");
        g.minimal = "disk=none | run $(rm -rf /)".into();
        assert_eq!(
            replay_line(&r, &g.minimal),
            "hydo run 'disk=none | run $(rm -rf /)'"
        );
    }

    #[test]
    fn many_findings_are_counted_past_the_limit() {
        let mut r = run();
        for i in 0..(MAX_PER_STATE + 3) {
            r.findings.push(f(
                &format!("f1:invariant:{i:010}"),
                &format!("VIOLATION [n{i}]"),
                "new",
            ));
        }
        let m = markdown(&r);
        assert!(m.contains("And 3 more; the job's report"), "{m}");
        assert!(!m.contains("VIOLATION [n12]"));
    }

    /// A fake `gh api` over comments kept as files: list (through jq, as gh does), post, edit.
    const FAKE_GH: &str = r#"#!/usr/bin/env bash
set -eu
st="$FAKE_GH"
echo "$*" >> "$st/calls"
[ "$1" = api ] || exit 9
shift
method=GET jq="" body="" path=""
while [ $# -gt 0 ]; do
  case "$1" in
    --paginate) ;;
    -X) method=$2; shift ;;
    --jq) jq=$2; shift ;;
    -F | -f) case "$2" in body=@*) body="${2#body=@}" ;; *) exit 8 ;; esac; shift ;;
    *) path=$1 ;;
  esac
  shift
done
if [ -f "$st/readonly" ] && [ "$method" != GET ]; then echo "HTTP 403" >&2; exit 1; fi
case "$method $path" in
  "GET repos/"*/issues/*/comments)
    { printf '['; sep=''
      for f in $(ls "$st" | grep '^c\.' | sort -t. -k2 -n); do
        printf '%s{"id":%s,"body":%s}' "$sep" "${f#c.}" "$(jq -Rs . < "$st/$f")"; sep=','
      done; printf ']'; } | jq -r "$jq" ;;
  "POST repos/"*/issues/*/comments)
    n=$(( $(ls "$st" | grep -c '^c\.') + 100 )); cp "$body" "$st/c.$n" ;;
  "PATCH repos/"*/issues/comments/*)
    id=${path##*/}; [ -f "$st/c.$id" ] || exit 4; cp "$body" "$st/c.$id" ;;
  *) exit 7 ;;
esac
"#;

    /// action/comment.sh keeps one comment per pull request and project: it posts the first one,
    /// edits it in place on the next push, leaves every other comment alone, and only warns when
    /// the token cannot write.
    #[test]
    fn the_action_edits_its_one_comment_in_place() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        if Command::new("jq").arg("--version").output().is_err() {
            eprintln!("skipped: no jq on PATH (gh's --jq is jq)");
            return;
        }
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../action/comment.sh");
        let d = std::env::temp_dir().join(format!("hydo-comment-sh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (bin, st) = (d.join("bin"), d.join("state"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&st).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, FAKE_GH).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let marker = "<!-- hydo-ci: . -->";
        // a person's comment that quotes the marker, and the comment of another project
        std::fs::write(st.join("c.10"), format!("see this:\n{marker}\n")).unwrap();
        std::fs::write(st.join("c.11"), "<!-- hydo-ci: other -->\nold\n").unwrap();
        let run = |pr: &str, body: &str| {
            let f = d.join("comment.md");
            std::fs::write(&f, body).unwrap();
            let path = format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let o = Command::new("bash")
                .arg(&script)
                .args(["acme/shop", pr, marker])
                .arg(&f)
                .env("PATH", path)
                .env("FAKE_GH", &st)
                .output()
                .unwrap();
            (
                o.status.code(),
                String::from_utf8_lossy(&o.stdout).to_string(),
            )
        };
        let comments = || {
            let mut v: Vec<String> = std::fs::read_dir(&st)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("c."))
                .collect();
            v.sort();
            v
        };
        let (rc, out) = run("7", "### hydo: 1 new finding (failed)\n");
        assert_eq!(rc, Some(0), "{out}");
        assert!(out.contains("posted a comment on #7"), "{out}");
        assert_eq!(comments(), ["c.10", "c.102", "c.11"]);
        let first = std::fs::read_to_string(st.join("c.102")).unwrap();
        assert_eq!(
            first,
            format!("{marker}\n### hydo: 1 new finding (failed)\n")
        );
        // the next push edits it in place
        let (rc, out) = run("7", "### hydo: 1 fixed (passed)\n");
        assert_eq!(rc, Some(0));
        assert!(out.contains("updated comment 102 on #7"), "{out}");
        assert_eq!(comments(), ["c.10", "c.102", "c.11"]);
        assert!(std::fs::read_to_string(st.join("c.102"))
            .unwrap()
            .contains("1 fixed"));
        assert!(std::fs::read_to_string(st.join("c.10"))
            .unwrap()
            .starts_with("see this"));
        assert!(std::fs::read_to_string(st.join("c.11"))
            .unwrap()
            .ends_with("old\n"));
        // a read-only token (a fork's) warns and passes
        std::fs::write(st.join("readonly"), "").unwrap();
        let (rc, out) = run("7", "x\n");
        assert_eq!(rc, Some(0));
        assert!(
            out.contains("::warning::could not update the hydo comment"),
            "{out}"
        );
        // a number that is not one is refused before any call
        let calls = std::fs::read_to_string(st.join("calls"))
            .unwrap()
            .lines()
            .count();
        let (rc, _) = run("7; true", "x\n");
        assert_eq!(rc, Some(2));
        assert_eq!(
            std::fs::read_to_string(st.join("calls"))
                .unwrap()
                .lines()
                .count(),
            calls
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! The parts of the release list that need no server: where a token may go, the line at
//! GitHub's limit, `gh auth token` with its timeout, and `hydo doctor`'s daily look with the
//! kept answer. The commands against a fake release server are tested in tests/update.rs.

use super::*;
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-releaselist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_token_goes_to_github_s_api_only() {
    let api = "https://api.github.com/repos/kakoc/hydo/releases?per_page=100";
    assert!(token_may_go(api, None));
    assert!(token_may_go("https://API.github.com/x", None));
    for other in [
        "http://api.github.com/repos/x",
        "https://api.github.com.example.org/x",
        "https://api.github.com@example.org/x",
        "https://github.com/kakoc/hydo/releases/download/v0.4.0/SHA256SUMS",
        "https://objects.githubusercontent.com/x",
        "https://api.github.com:8443/x",
        "http://127.0.0.1:9/api/releases",
        "no url",
    ] {
        assert!(!token_may_go(other, None), "{other}");
    }
    // the stand-in of the tests: a loopback origin only, and exactly that origin
    let s = Some("http://127.0.0.1:4000");
    assert!(token_may_go(
        "http://127.0.0.1:4000/api/releases?per_page=100",
        s
    ));
    assert!(!token_may_go("http://127.0.0.1:4001/api/releases", s));
    assert!(!token_may_go("http://localhost:4000/api/releases", s));
    assert!(!token_may_go(
        "http://example.org/api",
        Some("http://example.org")
    ));
    assert!(!token_may_go(
        "https://example.org/api",
        Some("https://example.org")
    ));
}

#[test]
fn the_limit_s_line_says_when_it_resets_and_how_to_lift_it() {
    let l = Fail::Limited {
        reset: Some(1000 + 37 * 60 + 30),
        login: false,
    }
    .line(1000);
    assert!(
        l.contains("GitHub's limit for requests without a login"),
        "{l}"
    );
    assert!(l.contains("resets in 38 minutes"), "{l}");
    assert!(l.contains("GH_TOKEN") && l.contains("gh auth login"), "{l}");
    assert!(!l.contains('\n'), "one line: {l}");
    let l = Fail::Limited {
        reset: None,
        login: true,
    }
    .line(1000);
    assert!(
        l.contains("with your token") && l.contains("within an hour"),
        "{l}"
    );
    assert!(!l.contains("gh auth login"), "{l}");
    assert_eq!(resets_in(Some(1030), 1000), "in 1 minute");
    assert_eq!(resets_in(Some(900), 1000), "now");
}

fn script(dir: &Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("gh");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn gh_auth_token_is_read_through_a_file_with_a_timeout() {
    let d = scratch("gh");
    let args = d.join("args");
    let gh = script(
        &d,
        &format!("echo \"$*\" > '{}'\necho gho_fake123", args.display()),
    );
    let t = gh_token(&gh, Duration::from_secs(5)).unwrap();
    assert_eq!(t.expose(), "gho_fake123");
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().trim(),
        "auth token --hostname github.com"
    );
    // a gh that is not logged in, prints nothing, or prints no token
    assert!(gh_token(
        &script(&d, "echo 'not logged in' >&2; exit 1"),
        Duration::from_secs(5)
    )
    .is_none());
    assert!(gh_token(&script(&d, "exit 0"), Duration::from_secs(5)).is_none());
    assert!(gh_token(&script(&d, "echo 'two words'"), Duration::from_secs(5)).is_none());
    // a gh that hangs, with a child that keeps the output open: killed after the wait
    let t0 = std::time::Instant::now();
    assert!(gh_token(
        &script(&d, "(sleep 30 &)\nsleep 30"),
        Duration::from_millis(300)
    )
    .is_none());
    assert!(t0.elapsed() < Duration::from_secs(10));
    // no output file is left behind
    let left = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("hydo-gh-{}-", std::process::id()))
        })
        .count();
    assert_eq!(left, 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_doctor_looks_once_a_day_and_keeps_a_failed_look_too() {
    let d = scratch("doctor");
    let f = d.join(CHECK_FILE);
    let calls = Cell::new(0);
    let list =
        br#"[{"tag_name":"v0.4.0"},{"tag_name":"v0.9.0"},{"tag_name":"skill-v2.9.0"}]"#.to_vec();
    let ok = |_: &str| {
        calls.set(calls.get() + 1);
        Ok(list.clone())
    };
    let down = |_: &str| -> Result<Vec<u8>, GetError> {
        calls.set(calls.get() + 1);
        Err(GetError::Failed("offline".into()))
    };
    assert_eq!(
        doctor_newest(&f, "http://a", 1000, &ok).as_deref(),
        Some("0.9.0")
    );
    assert_eq!(
        doctor_newest(&f, "http://a", 2000, &ok).as_deref(),
        Some("0.9.0")
    );
    assert_eq!(
        calls.get(),
        1,
        "the second look within a day is the kept answer"
    );
    // the doctor's answer serves the commands for 10 minutes
    let k = load(&f).unwrap();
    let l = kept_list(&k, "http://a", 1000 + FRESH - 1, FRESH).unwrap();
    assert_eq!(l.skill.as_deref(), Some("2.9.0"));
    assert_eq!(l.hydo.last().unwrap().version, "0.9.0");
    assert_eq!(l.age, Some(FRESH - 1));
    assert!(kept_list(&k, "http://a", 1000 + FRESH, FRESH).is_none());
    assert!(kept_list(&k, "http://b", 1000, FRESH).is_none());
    // another list, or a day later: a new look; a failed look is kept as no answer
    assert_eq!(doctor_newest(&f, "http://b", 2000, &down), None);
    assert_eq!(doctor_newest(&f, "http://b", 3000, &down), None);
    assert_eq!(
        calls.get(),
        2,
        "a failed look is not repeated within the day"
    );
    assert!(
        kept_list(&load(&f).unwrap(), "http://b", 3000, FRESH).is_none(),
        "a failed look is no list for the commands"
    );
    assert_eq!(
        doctor_newest(&f, "http://b", 2000 + DAY, &ok).as_deref(),
        Some("0.9.0")
    );
    // a file that cannot be written is no error
    assert_eq!(
        doctor_newest(Path::new("/nonexistent/x/y.json"), "http://a", 1, &ok).as_deref(),
        Some("0.9.0")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn at_the_limit_the_doctor_keeps_its_answer_and_waits_for_the_reset() {
    let d = scratch("limit");
    let f = d.join(CHECK_FILE);
    let calls = Cell::new(0);
    let list = br#"[{"tag_name":"v0.9.0"}]"#.to_vec();
    let ok = |_: &str| {
        calls.set(calls.get() + 1);
        Ok(list.clone())
    };
    let limited = |_: &str| -> Result<Vec<u8>, GetError> {
        calls.set(calls.get() + 1);
        Err(GetError::Limited {
            status: 403,
            reset: Some(DAY + 5000),
        })
    };
    assert_eq!(
        doctor_newest(&f, "http://a", 1000, &ok).as_deref(),
        Some("0.9.0")
    );
    // a day later the look meets the limit: no answer, and the earlier one is not overwritten
    assert_eq!(doctor_newest(&f, "http://a", DAY + 2000, &limited), None);
    let k = load(&f).unwrap();
    assert_eq!(k["newest"].as_str(), Some("0.9.0"));
    assert_eq!(k["checked"].as_u64(), Some(1000));
    assert_eq!(k["limited_until"].as_u64(), Some(DAY + 5000));
    // no look before the reset, a look after it
    assert_eq!(doctor_newest(&f, "http://a", DAY + 3000, &ok), None);
    assert_eq!(calls.get(), 2);
    assert_eq!(
        doctor_newest(&f, "http://a", DAY + 5000, &ok).as_deref(),
        Some("0.9.0")
    );
    assert_eq!(calls.get(), 3);
    assert!(load(&f).unwrap().get("limited_until").is_none());
    let _ = std::fs::remove_dir_all(&d);
}

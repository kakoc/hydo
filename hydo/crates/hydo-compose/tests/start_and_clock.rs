//! Two things an import writes since Milestone 202: `start_after: started` for a service whose
//! dependencies are all Compose's `condition: service_started` (long form), and an explicit
//! `guest.clock_start` (the day of the import, or `HYDO_INIT_CLOCK_START`).

use hydo_compose::registry::Registry;
use std::path::PathBuf;

struct Refuses;

impl Registry for Refuses {
    fn manifest(&mut self, _: &str, repo: &str, r: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}:{r}"))
    }
    fn blob(&mut self, _: &str, repo: &str, d: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}@{d}"))
    }
}

fn import(tag: &str, compose: &str, clock: Option<i64>) -> String {
    let d: PathBuf =
        std::env::temp_dir().join(format!("hydo-compose-sc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("docker-compose.yml"), compose).unwrap();
    let a = vec![
        d.join("docker-compose.yml").display().to_string(),
        "--no-resolve".into(),
    ];
    assert_eq!(hydo_compose::main_with_clock(&a, &mut Refuses, clock), 0);
    let m = std::fs::read_to_string(d.join("hydo.yaml")).unwrap();
    let _ = std::fs::remove_dir_all(&d);
    m
}

/// The lines of one service's entry in the written manifest.
fn entry<'a>(m: &'a str, name: &str) -> Vec<&'a str> {
    m.lines()
        .skip_while(|l| *l != format!("  - name: {name}"))
        .skip(1)
        .take_while(|l| !l.starts_with("  - name:") && !l.is_empty())
        .collect()
}

const PEERS: &str = "services:
  a:
    image: redis:7.2.4
  b:
    image: redis:7.2.4
    depends_on:
      a: {condition: service_started}
  c:
    image: redis:7.2.4
    depends_on: [a]
  d:
    image: redis:7.2.4
    depends_on:
      a: {condition: service_started}
      b: {condition: service_healthy}
  e:
    image: redis:7.2.4
    depends_on:
      a: {}
";

#[test]
fn service_started_dependencies_start_without_waiting() {
    let m = import("peers", PEERS, Some(1794096000));
    let has = |s: &str| {
        entry(&m, s)
            .iter()
            .any(|l| l.trim_start().starts_with("start_after: started"))
    };
    assert!(has("b"), "{m}");
    assert!(
        has("e"),
        "the long form without a condition is service_started\n{m}"
    );
    assert!(!has("a"), "{m}");
    assert!(!has("c"), "the short form keeps the readiness wait\n{m}");
    assert!(
        !has("d"),
        "one service_healthy keeps the readiness wait\n{m}"
    );
    // and the written manifest is valid
    hydo_manifest::from_embedded(&m, None).unwrap_or_else(|e| panic!("{e}\n{m}"));
}

#[test]
fn the_clock_starts_on_the_given_day() {
    let m = import("clock", PEERS, Some(1794096000));
    assert!(
        m.contains("  clock_start: 1794096000   # 2026-11-08 00:00 UTC"),
        "{m}"
    );
    // without a value: today at 00:00 UTC
    let m = import("today", PEERS, None);
    let v: i64 = m
        .lines()
        .find_map(|l| l.trim().strip_prefix("clock_start: "))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .expect("a clock_start line");
    assert_eq!(v, hydo_manifest::new_clock_start(None));
}

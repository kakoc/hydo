//! How long an `eventually` check needed, read against its deadline (the libSQL onboarding chose
//! a deadline by rebuilding its image with a longer one). The host controller's eventually record
//! carries `elapsed_s` (string) and `within_s`; a run's property table and an exploration's
//! liveness summary keep the deadline, and the slowest episode that held only after it (the
//! host's "satisfied only <t>s after ...").

use hydo_explore::results::{liveness, property_table, render_liveness, render_properties};

/// One eventually record as hydo-control's liveness feature prints it.
fn rec(ok: bool, el: &str, within: u32, out: &str) -> String {
    format!(
        "HYDO-ASSERT {{\"t\":\"eventually\",\"id\":\"replica.converges\",\"ok\":{ok},\"msg\":\"the replica catches up\",\"details\":{{\"after\":\"heal\",\"elapsed_s\":\"{el}\",\"out\":\"{out}\",\"within_s\":{within}}}}}\n"
    )
}

#[test]
fn a_run_keeps_the_deadline_beside_the_slowest_time() {
    let t = property_table(&[rec(
        true,
        "47.12",
        120,
        "satisfied 47.12s after heal (within 120s): ok",
    )])
    .unwrap();
    let p = &t.properties[0];
    assert_eq!(p.slowest_ok_s, Some(47.12));
    assert_eq!(p.within_s, Some(120.0));
    assert!(render_properties(&t).contains("[slowest in time: 47.12 s of 120 s]"));
}

#[test]
fn an_exploration_names_the_deadline_and_the_slowest_late_run() {
    let runs = vec![
        (
            "seed-1".to_string(),
            rec(
                true,
                "12.00",
                30,
                "satisfied 12.00s after heal (within 30s): ok",
            ),
        ),
        (
            "seed-2".to_string(),
            rec(
                false,
                "47.31",
                30,
                "satisfied only 47.31s after heal, deadline 30s: ok",
            ),
        ),
        (
            "seed-3".to_string(),
            rec(
                false,
                "31.36",
                30,
                "not satisfied 31.36s after heal, deadline 30s (last check rc=1: lag)",
            ),
        ),
    ];
    let l = liveness(&runs);
    assert_eq!(l.len(), 1);
    let p = &l[0];
    assert_eq!((p.held_runs, p.missed_runs), (1, 2));
    assert_eq!(p.slowest_s, Some(12.0));
    assert_eq!(p.within_s, Some(30.0));
    // the late run needed 47.31 s; the expired one says nothing about what it needed
    assert_eq!(p.slowest_late_s, Some(47.31));
    assert_eq!(p.slowest_late_run.as_deref(), Some("seed-2"));
    let md = render_liveness(&l).join("\n");
    assert!(
        md.contains("| `replica.converges` | 1 | 2 | 12.00 s (seed-1) | 30 s | 47.31 s (seed-2) |"),
        "{md}"
    );
}

//! `--protocol-cover` (Milestone 257, `hydo_explore::cover`): a walk over every (route, step,
//! rule kind) of a project's fakes and proxies, the pairs a probe run saw called first.
//!
//! - Without a cover nothing changes: the explorer's identity and corpus goldens pin every drawn
//!   schedule, and here a cover changes only the schedule's first `H=` token.
//! - On the web-shop example (Milestone 253), the double order needs `place_order` faulted in step
//!   0. The 40 schedules of `--protocol-faults` that ran there missed it; the cover reaches every
//!   called pair in its first 6 schedules and every cell in 24.

use hydo_explore::cover::{self, Cover};
use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn top() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn ex_of(example: &str) -> Ex {
    let text = std::fs::read_to_string(
        top()
            .join("hydo/onboard/examples")
            .join(example)
            .join("hydo.yaml"),
    )
    .unwrap();
    let sp = Space::from_text(&text, None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e
}

fn names(e: &Ex) -> Vec<&str> {
    e.pf_targets.iter().map(|t| t.name.as_str()).collect()
}

/// What the web-shop buggy image's probe run (`disk=none dseed=0 | run H=api@0-1:delay=1
/// H=api@1-2:delay=2`) recorded on hvx (Milestone 257): step 0 ("places an order", "lists the
/// products") calls every route; step 1 ("adds two products to the cart") lists the products
/// and adds items.
const WEB_SHOP_RECORD: &str = "\
hydo-world: 1 api.products ref=- idem=- fx=0 ok+delay1
hydo-world: 2 api.add_item ref=- idem=- fx=1 ok+delay1
hydo-world: 3 api.place_order ref=cart-1767225605222-541253 idem=- fx=1 ok+delay1
hydo-world: 4 api.get_order ref=- idem=- fx=0 ok+delay1
hydo-world: 5 api.products ref=- idem=- fx=0 ok+delay1
hydo-world: 6 api.products ref=- idem=- fx=0 ok+delay2
hydo-world: 7 api.add_item ref=- idem=- fx=1 ok+delay2
hydo-world: 8 api.add_item ref=- idem=- fx=1 ok+delay2
";

fn web_shop() -> (Ex, Cover) {
    let mut e = ex_of("web-shop");
    let steps = e.cover_steps();
    let called = cover::called(&[WEB_SHOP_RECORD], &e.pf_targets, steps);
    let c = cover::plan(&e.pf_targets, steps, called.as_ref());
    e.pf_cover = Some(c.clone());
    (e, c)
}

#[test]
fn the_probe_puts_a_delay_on_every_route_in_every_step() {
    let e = ex_of("web-shop");
    assert_eq!(
        names(&e),
        [
            "api.products",
            "api.add_item",
            "api.place_order",
            "api.get_order"
        ]
    );
    // two shards: two steps, not the four a test workload's generators draw as
    assert_eq!(e.cover_steps(), 2);
    let runs = cover::probe_runs(&e.pf_targets, e.cover_steps());
    let text: Vec<Vec<String>> = runs
        .iter()
        .map(|r| r.iter().map(hydo_explore::sched::render_h).collect())
        .collect();
    assert_eq!(text, [["H=api@0-1:delay=1", "H=api@1-2:delay=2"]]);
    // openapi-payouts: one fake, 8 steps, one run
    let e = ex_of("openapi-payouts");
    let runs = cover::probe_runs(&e.pf_targets, e.cover_steps());
    assert_eq!((runs.len(), runs[0].len()), (1, 8));
    // shop: two fakes, 12 steps, two runs
    let e = ex_of("shop");
    let runs = cover::probe_runs(&e.pf_targets, e.cover_steps());
    assert_eq!(runs.iter().map(Vec::len).collect::<Vec<_>>(), [22, 2]);
}

#[test]
fn the_web_shop_s_cells_called_pairs_first() {
    let (e, c) = web_shop();
    assert_eq!((c.pairs, c.called, c.cells.len()), (8, Some(6), 32));
    // the first 6 seeds fault each called pair once
    let first: BTreeSet<(usize, i64)> = (1..=6)
        .map(|sd| {
            let x = cover::cell_of(&c, sd).unwrap();
            (x.target, x.step)
        })
        .collect();
    assert_eq!(first.len(), 6);
    // seeds 1-24: every cell of a called pair once
    let mut cells: Vec<(usize, i64, String)> = (1..=24)
        .map(|sd| {
            let x = cover::cell_of(&c, sd).unwrap();
            (x.target, x.step, x.kind.clone())
        })
        .collect();
    cells.sort();
    cells.dedup();
    assert_eq!(cells.len(), 24);
    // the schedules: one H= token per seed, the cell's, with p=100
    let text = |sd: u64| e.render(&e.compose_seed("random", sd, false));
    let h: Vec<String> = (1..=8)
        .map(|sd| {
            text(sd)
                .split_whitespace()
                .find(|w| w.starts_with("H="))
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        h,
        [
            "H=api.products@0-1:status=500,effect=yes",
            "H=api.add_item@0-1:delay=1000",
            "H=api.place_order@0-1:drop",
            "H=api.get_order@0-1:reset",
            "H=api.products@1-2:status=500,effect=yes",
            "H=api.add_item@1-2:delay=3000",
            "H=api.products@0-1:delay=10000",
            "H=api.add_item@0-1:drop",
        ]
    );
    assert!((1..=40).all(|sd| !text(sd).contains("p=")));
    // the values come from the cover's own stream: the status codes vary
    let codes: BTreeSet<String> = (1..=96)
        .filter_map(|sd| {
            let t = text(sd);
            let i = t.find("status=")?;
            Some(t[i..].split([',', ' ']).next()?.to_string())
        })
        .collect();
    assert_eq!(codes.len(), 4, "{codes:?}");
}

#[test]
fn a_cover_changes_only_the_first_h_token() {
    for example in ["web-shop", "openapi-payouts", "shop"] {
        let mut plain = ex_of(example);
        plain.new_faults = true;
        let mut with = plain.clone();
        let steps = with.cover_steps();
        with.pf_cover = Some(cover::plan(&with.pf_targets, steps, None));
        for random in [false, true] {
            plain.protocol_faults = random;
            with.protocol_faults = random;
            for sd in 1..=200u64 {
                for strategy in ["random", "process", "swarm"] {
                    let a = plain.render(&plain.compose_seed(strategy, sd, false));
                    let b = with.render(&with.compose_seed(strategy, sd, false));
                    let mut words: Vec<&str> = b.split_whitespace().collect();
                    let at = words.iter().position(|w| w.starts_with("H=")).unwrap();
                    let first = words.remove(at);
                    let cell = cover::cell_of(with.pf_cover.as_ref().unwrap(), sd).unwrap();
                    assert!(
                        first.starts_with(&format!(
                            "H={}@{}-{}:",
                            with.pf_targets[cell.target].name,
                            cell.step,
                            cell.step + 1
                        )),
                        "{example} seed {sd}: {b}"
                    );
                    assert_eq!(words.join(" "), a, "{example} seed {sd} {strategy}");
                }
            }
        }
    }
}

#[test]
fn without_a_cover_the_draws_are_as_before() {
    let e = ex_of("shop");
    let mut on = e.clone();
    on.protocol_faults = true;
    let mut off = on.clone();
    off.pf_cover = None;
    for sd in 1..=100u64 {
        assert_eq!(
            on.render(&on.compose_seed("process", sd, false)),
            off.render(&off.add_protocol_cover(off.compose_seed("process", sd, false), sd))
        );
    }
}

/// Where `--protocol-faults` alone puts the web-shop's double order: the first schedule whose
/// token loses every answer to `place_order` in step 0 (a `drop` or a delay past the client's
/// 3 s, without `p=`) is seed 120; before it, seeds 97 and 117 hold such a token with `p=25` and
/// `p=50`. The cover's is seed 3.
#[test]
fn the_random_draws_reach_the_double_order_late() {
    let mut e = ex_of("web-shop");
    e.protocol_faults = true;
    let loses = |w: &str| {
        w.starts_with("H=api.place_order@0-")
            && (w.contains(":drop") || w.contains(":delay=3000") || w.contains(":delay=10000"))
    };
    let first = |e: &Ex, certain: bool| {
        (1..=200u64).find(|sd| {
            let t = e.render(&e.compose_seed("random", *sd, false));
            t.split_whitespace()
                .any(|w| loses(w) && (!certain || !w.contains(",p=")))
        })
    };
    assert_eq!((first(&e, false), first(&e, true)), (Some(97), Some(120)));
    let (cover, _) = web_shop();
    assert_eq!(first(&cover, true), Some(3));
}

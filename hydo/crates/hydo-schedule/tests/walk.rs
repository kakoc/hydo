//! `I=`: the walk of `browser.explore` (Milestone 258): a walk prints as it was written, its
//! left-out actions in order, and what it refuses.

use hydo_schedule::{GuestToken, Walk};

fn walk(v: &str) -> Result<Walk, String> {
    match GuestToken::parse(&format!("I={v}")) {
        Ok(GuestToken::Walk(w)) => Ok(w),
        Ok(t) => Err(format!("not a walk: {t}")),
        Err(e) => Err(e.message),
    }
}

#[test]
fn a_walk_prints_as_it_was_written() {
    for t in [
        "9f3a01c2",
        "1,n6",
        "abc,n4,x0.2,x1.0,rec",
        "0,rec",
        "0,x3.0-11,rec2",
    ] {
        let w = walk(t).unwrap();
        assert_eq!(w.to_string(), t);
        assert_eq!(GuestToken::Walk(w).to_string(), format!("I={t}"));
    }
    let w = walk("5,x1.0,x0.3").unwrap();
    assert_eq!(w.to_string(), "5,x0.3,x1.0");
    assert!(!w.takes(0, 3));
    assert!(w.takes(0, 2));
    assert_eq!(GuestToken::Walk(w).letter(), 'I');
}

/// Milestone 264: a range leaves out several actions of one step; four or more in a row print
/// as a range, fewer one by one (as Milestone 258's walks were written).
#[test]
fn a_range_leaves_out_actions_of_one_step() {
    let w = walk("7,x2.3-11").unwrap();
    assert_eq!(w.skip.len(), 9);
    assert!(w.takes(2, 2) && !w.takes(2, 3) && !w.takes(2, 11) && w.takes(2, 12));
    assert_eq!(w.to_string(), "7,x2.3-11");
    for (t, out) in [
        ("7,x0.0,x0.1,x0.2", "7,x0.0,x0.1,x0.2"),
        ("7,x0.0-2", "7,x0.0,x0.1,x0.2"),
        ("7,x0.0,x0.1,x0.2,x0.3", "7,x0.0-3"),
        ("7,x1.4,x0.0-5,x0.7", "7,x0.0-5,x0.7,x1.4"),
        ("7,x3.0-11,x4.0-11,x5.0-11", "7,x3.0-11,x4.0-11,x5.0-11"),
    ] {
        assert_eq!(walk(t).unwrap().to_string(), out, "{t}");
        assert_eq!(walk(out).unwrap(), walk(t).unwrap(), "{t}");
    }
}

/// Milestone 267: `rec<step>` records the step of a failure; `rec` the last step with an action.
#[test]
fn a_recording_names_its_step() {
    let w = walk("7,rec2").unwrap();
    assert!(w.rec);
    assert_eq!(w.rec_step, Some(2));
    let w = walk("7,rec").unwrap();
    assert!(w.rec);
    assert_eq!(w.rec_step, None);
    assert!(walk("7").unwrap().rec_step.is_none());
}

#[test]
fn what_a_walk_refuses() {
    for t in [
        "",
        "0x1",
        "01",
        "g",
        "1,n0",
        "1,n201",
        "1,x1",
        "1,y",
        "1,rec,rec",
        "1,rec1,rec",
        "1,rec02",
        "1,recx",
        "1,x0.1,x0.1",
        "1,x0.3-2",
        "1,x0.0-200",
        "1,x0.0-3,x0.2",
        "1,x0.-3",
    ] {
        assert!(walk(t).is_err(), "{t}");
    }
}

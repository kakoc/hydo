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
    for t in ["9f3a01c2", "1,n6", "abc,n4,x0.2,x1.0,rec", "0,rec"] {
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
        "1,x0.1,x0.1",
    ] {
        assert!(walk(t).is_err(), "{t}");
    }
}

//! types: the values function contracts read (docs/design/contracts.md, "Reading values"), one
//! function per group of types. `types run <n>` calls each function n times with values the
//! contracts know, and `types bad` breaks one clause of each function once. hydo-contracts'
//! tests compile this program's plan; the example's hydo-types.yaml runs it.
use std::hint::black_box;

pub struct Pt {
    pub x: u64,
    pub y: i32,
}

/// Strings, vectors and slices: their lengths and their first elements.
// hydo: requires len(s) == 5 && s == "hello"
// hydo: requires len(owned) == 3 && owned == "abc" && owned != "abd"
// hydo: requires len(v) == 4 && v[0] == 10 && v[3] == 40 && v[k] == 10 * (k + 1)
// hydo: requires len(sl) == 2 && sl[1] == 7
// hydo: ensures result == len(s) + len(v)
#[inline(never)]
fn text(s: &str, owned: &String, v: &Vec<u32>, sl: &[u16], k: usize) -> usize {
    black_box((s, owned, v, sl, k));
    s.len() + v.len()
}

/// Options and results: the variant and its value, by value and through a reference.
// hydo: requires is_some(o) && some(o) == 7 && !is_none(o)
// hydo: requires is_ok(r) == (some(o) == 7) && (is_ok(r) || err(r) == -3)
// hydo: requires is_some(p) && some(p).x == 1 && some(p).y == -2
// hydo: ensures is_some(result) && some(result) == some(o) + 1
#[inline(never)]
fn opt(o: Option<u64>, r: Result<u32, i64>, p: Option<&Pt>) -> Option<u64> {
    black_box((&r, p));
    o.map(|v| v + 1)
}

/// A vector and a string by value, and a result returned in registers.
// hydo: requires len(v) == 3 && v[2] == 3 && len(s) == 2 && s == "ok"
// hydo: ensures is_ok(result) && ok(result) == len(v) + len(s)
// hydo: ensures !is_err(result)
#[inline(never)]
fn owned(v: Vec<u8>, s: String) -> Result<usize, u8> {
    Ok(black_box(v.len() + s.len()))
}

/// A string returned through memory, and a Result read from a reference.
// hydo: requires is_err(r) && len(err(r)) == 4 && err(r) == "oops"
// hydo: ensures len(result) == 4 && result == "oops"
#[inline(never)]
fn copy(r: &Result<u64, String>) -> String {
    match r {
        Ok(_) => String::new(),
        Err(e) => black_box(e.clone()),
    }
}

fn round(bad: bool) {
    let s = if bad { "hellO" } else { "hello" };
    let abc = String::from("abc");
    let v = vec![10u32, 20, 30, 40];
    let sl: [u16; 2] = [6, 7];
    black_box(text(
        black_box(s),
        black_box(&abc),
        black_box(&v),
        black_box(&sl),
        black_box(2),
    ));
    let pt = Pt { x: 1, y: -2 };
    black_box(opt(
        black_box(Some(if bad { 8 } else { 7 })),
        black_box(Ok(5)),
        black_box(Some(&pt)),
    ));
    let _ = black_box(owned(
        black_box(vec![1, 2, if bad { 4 } else { 3 }]),
        black_box(String::from("ok")),
    ));
    let e: Result<u64, String> = Err(if bad { "oopz" } else { "oops" }.to_string());
    black_box(copy(black_box(&e)));
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("run") => {
            let n: u64 = a.get(2).and_then(|n| n.parse().ok()).unwrap_or(1);
            for _ in 0..n {
                round(false);
            }
            println!("types: {n} rounds");
        }
        Some("bad") => {
            round(true);
            println!("types: one round with broken clauses");
        }
        _ => {
            eprintln!("usage: types run <n> | types bad");
            std::process::exit(2);
        }
    }
}

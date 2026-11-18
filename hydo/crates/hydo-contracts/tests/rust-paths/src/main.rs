// The error paths of Milestone 306's test (tests/paths.rs): the line numbers are the test's.
use std::env;

#[derive(Debug)]
struct Bad(u32);

#[derive(Debug)]
struct Wrapped(String);

impl From<Bad> for Wrapped {
    fn from(b: Bad) -> Wrapped {
        Wrapped(format!("bad input {}", b.0))
    }
}

#[inline(never)]
fn parse(s: &str) -> Result<u32, Bad> {
    let n: u32 = s.parse().map_err(|_| Bad(0))?;
    if n == 0 {
        return Err(Bad(1));
    }
    Ok(n)
}

#[inline(never)]
fn load(args: &[String]) -> Result<u32, Wrapped> {
    let a = parse(&args[0])?;
    match parse(&args[1]) {
        Ok(b) => Ok(a + b),
        Err(e) => {
            eprintln!("second: {e:?}");
            Err(e.into())
        }
    }
}

#[inline(never)]
fn retry_load(args: &[String]) -> u32 {
    for _ in 0..3 {
        if let Ok(v) = load(args) {
            return v;
        }
    }
    let v: u32 = args[2].parse().unwrap();
    if v > 100 {
        panic!("too large: {v}");
    }
    v
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    println!("{}", retry_load(&args));
}

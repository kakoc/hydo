//! bank-rs: the planted deadlock of `go/main.go` with two threads and two `std::sync::Mutex`
//! statics (docs/design/inspect-stacks.md).
//!
//! The worker takes `ACCOUNTS`, works for a while, then takes `JOURNAL`. The watchdog wakes every
//! 10 ms; after a gap of more than 200 ms (the process was paused) it checks the journal against
//! the accounts and takes the two locks in the other order. A pause while the worker holds
//! `ACCOUNTS` makes the two wait on each other for good, without an error.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

static ACCOUNTS: Mutex<[i64; 4]> = Mutex::new([0; 4]);
static JOURNAL: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[inline(never)]
fn transfer(i: u64) {
    let mut a = ACCOUNTS.lock().unwrap();
    a[(i % 4) as usize] -= 1;
    a[((i + 1) % 4) as usize] += 1;
    thread::sleep(Duration::from_millis(20)); // the accounts' update takes a while
    thread::sleep(Duration::from_millis(5));
    append_journal(format!("transfer {i}"));
    drop(a);
}

#[inline(never)]
fn append_journal(line: String) {
    let mut j = JOURNAL.lock().unwrap();
    println!("{line}");
    j.push(line);
    let _ = std::fs::write("/tmp/bank-rs.count", j.len().to_string());
}

#[inline(never)]
fn check_journal(gap: Duration) {
    let j = JOURNAL.lock().unwrap();
    println!("watchdog: no wake for {gap:?}, checking the journal against the accounts");
    thread::sleep(Duration::from_millis(5));
    let a = ACCOUNTS.lock().unwrap(); // the other order
    println!(
        "watchdog: {} entries, balances sum to {}",
        j.len(),
        a.iter().sum::<i64>()
    );
}

fn worker() {
    for i in 0.. {
        transfer(i);
        thread::sleep(Duration::from_millis(1));
    }
}

fn watchdog() {
    let mut last = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(10));
        let gap = last.elapsed();
        if gap > Duration::from_millis(200) {
            check_journal(gap);
        }
        last = Instant::now();
    }
}

fn main() {
    let w = thread::Builder::new()
        .name("worker".into())
        .spawn(worker)
        .unwrap();
    let d = thread::Builder::new()
        .name("watchdog".into())
        .spawn(watchdog)
        .unwrap();
    let _ = (w.join(), d.join());
}

//! examples/redis-composed written with hydo-test: the same plan (services, knob, faults,
//! composer options) and the same commands and checks, under the same ids, as the shell
//! version in ../redis-composed/hydo/, so a schedule composes the same commands in the same
//! rounds on both. `system()` runs on the host only (`redis-composed-rs plan`); in the guest
//! hydo runs `/bin/redis-composed-rs driver <name>` and `... check <name>`.
//!
//!   cargo hydo plan --arch x86_64                     -> .hydo/plan.json + .hydo/guest/redis-composed-rs
//!   TRANSFER=racy cargo hydo plan --arch x86_64       the planted lost-update race
use hydo_test::prelude::*;
use std::time::Duration;

const ACCOUNTS: u64 = 5;
const TOTAL: i64 = 500;

fn system() -> System {
    let transfer = std::env::var("TRANSFER").unwrap_or_else(|_| "atomic".into());
    System::new(if transfer == "racy" { "redis-composed-rs-racy" } else { "redis-composed-rs" })
        .description("Redis with an append-only file, driven by composed commands (hydo-test, Rust).")
        .file("~/hydo/build/redis-server", "/bin/redis-server")
        .file("~/hydo/build/redis-cli", "/bin/redis-cli")
        .text("/etc/redis.conf", "port 6379\nbind 127.0.0.1\ndir /data/redis\nsave \"\"\nappendonly yes\nappendfsync always\nhz 1\ndynamic-hz no\n")
        .data(64, &["redis"])
        .env("TRANSFER", &transfer)
        .env("ACCOUNTS", &ACCOUNTS.to_string())
        .env("TOTAL", &TOTAL.to_string())
        .service(Service::new("redis", "/bin/redis-server /etc/redis.conf").ready("/bin/redis-cli ping | grep -q PONG").port(6379))
        .steps(20)
        .workload_timeout(120)
        .test(TestParams { parallel: Some(4), per_round: Some(2), round_ms: Some(100), command_timeout: Some(20),
                           drain_timeout: Some(30), ..Default::default() })
        .knob(Knob::new('R', "Redis appendfsync", "always", "/bin/redis-cli CONFIG SET appendfsync $HYDO_VALUE")
              .values(&["always", "everysec", "no"]))
        .faults(&[Fault::Kill, Fault::Pause, Fault::Cut, Fault::Clock, Fault::Disk])
        .regression("lost-update", "disk=none dseed=1 | run W=0000002a,p4,q2,r100", Expect::Ok)
}

/// redis-cli with a 2 s client deadline (the shell version's `timeout 2 redis-cli`).
fn r(args: &[&str]) -> Result<String> {
    let mut argv = vec!["/bin/redis-cli"];
    argv.extend_from_slice(args);
    hydo_test::exec(&argv, Duration::from_secs(2))
}

fn ping() -> bool {
    r(&["PING"]).map(|s| s == "PONG").unwrap_or(false)
}

/// From HYDO_SEED: two distinct accounts and an amount (the shell version's `pick`).
fn pick(ctx: &Ctx) -> (u64, u64, i64) {
    let s = ctx.seed();
    let a = s % ACCOUNTS;
    let mut b = (s / ACCOUNTS) % (ACCOUNTS - 1);
    if b >= a {
        b += 1;
    }
    (a, b, ((s / 100) % 5 + 1) as i64)
}

#[driver(first)]
fn accounts(_ctx: &Ctx) -> Result<()> {
    for i in 0..ACCOUNTS {
        if r(&["SET", &format!("acct:{}", i), &(TOTAL / ACCOUNTS as i64).to_string()])? != "OK" {
            return Err(fail!("SET acct:{} failed", i));
        }
    }
    Ok(())
}

#[driver(parallel)]
fn set(ctx: &Ctx) -> Result<()> {
    let id = ctx.id().to_string();
    if r(&["SET", &format!("k:{}", id), &format!("v{}", id)]).map(|x| x == "OK").unwrap_or(false) {
        ctx.ack(&id)
    } else {
        ctx.outcome(&id, "unknown")?;
        Err(fail!("SET k:{} not acknowledged", id))
    }
}

#[driver(parallel)]
fn transfer(ctx: &Ctx) -> Result<()> {
    let (a, b, amt) = pick(ctx);
    let (ka, kb) = (format!("acct:{}", a), format!("acct:{}", b));
    if ctx.env("TRANSFER") == "racy" {
        // read-modify-write in four commands: two transfers that overlap can lose an update
        let va: i64 = r(&["GET", &ka])?.parse()?;
        let vb: i64 = r(&["GET", &kb])?.parse()?;
        if r(&["SET", &ka, &(va - amt).to_string()])? != "OK" || r(&["SET", &kb, &(vb + amt).to_string()])? != "OK" {
            return Err(fail!("SET failed"));
        }
        Ok(())
    } else {
        let script = "redis.call('DECRBY', KEYS[1], ARGV[1]); return redis.call('INCRBY', KEYS[2], ARGV[1])";
        let out = r(&["EVAL", script, "2", &ka, &kb, &amt.to_string()])?;
        if out.is_empty() { Err(fail!("EVAL gave no answer")) } else { Ok(()) }
    }
}

#[driver(serial)]
fn rewrite(_ctx: &Ctx) -> Result<()> {
    let out = r(&["BGREWRITEAOF"])?;
    if !out.contains("started") && !out.contains("scheduled") {
        return Err(fail!("BGREWRITEAOF: {}", out));
    }
    for _ in 0..50 {
        if !r(&["INFO", "persistence"]).unwrap_or_default().contains("aof_rewrite_in_progress:1") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[driver(anytime)]
fn read(_ctx: &Ctx) -> Result<()> {
    r(&["MGET", "acct:0", "acct:1", "acct:2", "acct:3", "acct:4"]).map(|_| ())
}

#[driver(eventually)]
fn serves(_ctx: &Ctx) -> Result<()> {
    for n in 0..50 {
        if ping() {
            println!("PONG after {} retries", n);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(fail!("no PONG after 50 tries"))
}

#[driver(finally)]
fn summary(ctx: &Ctx) -> Result<()> {
    println!("acked={} keys={}", ctx.acked().len(), r(&["DBSIZE"]).unwrap_or_default());
    Ok(())
}

#[check(final, msg = "every SET the client saw acknowledged reads back with its value")]
fn acked_writes_survive(ctx: &Ctx) -> Result<()> {
    if !ping() {
        return Err(fail!("redis does not answer"));
    }
    let acked = ctx.acked();
    for i in &acked {
        if r(&["GET", &format!("k:{}", i)]).unwrap_or_default() != format!("v{}", i) {
            return Err(violation!("key k:{} lost", i));
        }
    }
    println!("acked={} all present", acked.len());
    Ok(())
}

#[check(final, msg = "transfers never create or destroy money: the balances sum to TOTAL")]
fn total_conserved(_ctx: &Ctx) -> Result<()> {
    if !ping() {
        return Err(fail!("redis does not answer"));
    }
    let mut s = 0i64;
    for i in 0..ACCOUNTS {
        s += r(&["GET", &format!("acct:{}", i)]).unwrap_or_default().parse::<i64>().unwrap_or(0);
    }
    if s != TOTAL {
        return Err(violation!("balances sum to {}, not {}", s, TOTAL));
    }
    println!("balances sum to {}", TOTAL);
    Ok(())
}

#[check(eventually, within = 15, msg = "Redis answers PING within 15 s after the faults are healed")]
fn answers(_ctx: &Ctx) -> Result<()> {
    if ping() { Ok(()) } else { Err(fail!("no PONG yet")) }
}

hydo_test::main!(system);

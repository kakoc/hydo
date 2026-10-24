//! hydo-test: write a hydo test as one Rust program.
//!
//! ```ignore
//! use hydo_test::prelude::*;
//!
//! fn system() -> System {                       // runs on the HOST only (`<bin> plan`)
//!     System::new("shop")
//!         .file("bin/shopd", "/bin/shopd")
//!         .service(Service::new("shopd", "/bin/shopd --data /data/shop").ready("/bin/shopd ping").port(8080))
//!         .data(64, &["shop"])
//!         .faults(&[Fault::Kill, Fault::Pause, Fault::Cut])
//! }
//!
//! #[driver(parallel)]
//! fn place_order(ctx: &Ctx) -> Result<()> { /* call the API, ctx.ack(id) on success */ Ok(()) }
//!
//! #[check(eventually, within = 20)]
//! fn orders_settle(ctx: &Ctx) -> Result<()> { /* Err(violation!(..)) while not yet */ Ok(()) }
//!
//! hydo_test::main!(system);
//! ```
//!
//! The binary has two lives. On the host, `<bin> plan --guest-binary <path>` calls `system()`
//! and prints hydo's plan (the manifest format of hvx/onboard/MANIFEST.md, as JSON): the
//! system plus one `test.commands` / `test.checks` entry per registered function, each run as
//! `/bin/<bin> driver <name>` / `/bin/<bin> check <name>`. In the guest, hydo's composer runs
//! those subcommands; `system()` is never called there, so no host state can leak into a
//! deterministic run. `cargo hydo` (src/bin/cargo-hydo.rs) does both builds and calls the
//! installed `hydo`.
//!
//! Exit statuses (the composer's contract): a driver returns Ok -> 0, Err(failed) -> 1 (the
//! operation failed, recorded), Err(violation) -> 100 (the run fails). A check returns Ok -> 0
//! (holds), Err(violation) -> 1 (violated), Err(failed) -> 2 (could not be evaluated now).
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod plan;
pub use plan::{Expect, Fault, Knob, Service, System, TestParams};

pub use hydo_test_macros::{check, driver};
#[doc(hidden)]
pub use inventory;
/// The assertion SDK (sdk/rust): drivers and checks, and the services under test, can state
/// properties directly (`hydo_test::assert::always(..)`, `hydo_buggify!`, ...).
pub use hydo_assert as assert;

pub mod prelude {
    pub use crate::{check, driver, fail, violation, Ctx, Error, Expect, Fault, Knob, Result, Service, System, TestParams};
}

/// The engine version this SDK writes into the plan (`hydo_version`), and the plan format.
pub const HYDO_VERSION: &str = "0.1";
pub const PLAN_FORMAT: u32 = 1;

// ------------------------------------------------------------------ results
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The operation failed (a timeout, a refused connection): normal under faults. A check
    /// returning it could not be evaluated now.
    Failed(String),
    /// A property is broken: a driver returning it fails the run; a check returning it is violated.
    Violation(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Failed(m) => write!(f, "failed: {}", m),
            Error::Violation(m) => write!(f, "violation: {}", m),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Failed(e.to_string())
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Error::Failed(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `fail!("no reply from {}", host)`: an `Error::Failed`.
#[macro_export]
macro_rules! fail {
    ($($a:tt)*) => { $crate::Error::Failed(format!($($a)*)) };
}

/// `violation!("order {} charged twice", id)`: an `Error::Violation`.
#[macro_export]
macro_rules! violation {
    ($($a:tt)*) => { $crate::Error::Violation(format!($($a)*)) };
}

// ------------------------------------------------------------------ registration
/// One `#[driver]` / `#[check]` function (written by the attribute macros).
pub struct Registration {
    pub role: &'static str,
    pub kind: &'static str,
    pub name: &'static str,
    pub func: fn(&Ctx) -> Result<()>,
    pub timeout: u32,
    pub every: u32,
    pub within: u32,
    pub poll: u32,
    pub after: &'static str,
    pub msg: &'static str,
}

inventory::collect!(Registration);

/// Every registered driver and check, sorted by (role, kind, name): the order never depends on
/// link order.
pub fn registrations() -> Vec<&'static Registration> {
    let mut v: Vec<&'static Registration> = inventory::iter::<Registration>.into_iter().collect();
    v.sort_by(|a, b| (a.role, a.kind, a.name).cmp(&(b.role, b.kind, b.name)));
    v
}

// ------------------------------------------------------------------ the context of one command
/// What a driver or check sees: the composer's environment (MANIFEST.md "Environment of a command").
pub struct Ctx {
    seed: u64,
    id: String,
    name: String,
    round: Option<u32>,
    rng: std::cell::Cell<u64>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let seed = std::env::var("HYDO_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        Ctx {
            seed,
            id: std::env::var("HYDO_CMD_ID").unwrap_or_default(),
            name: std::env::var("HYDO_CMD").or_else(|_| std::env::var("HYDO_CHECK")).unwrap_or_default(),
            round: std::env::var("HYDO_ROUND").ok().and_then(|s| s.parse().ok()),
            rng: std::cell::Cell::new(seed ^ 0x9E37_79B9_7F4A_7C15),
        }
    }
    /// HYDO_SEED: this command's number from the composition's random stream.
    pub fn seed(&self) -> u64 {
        self.seed
    }
    /// HYDO_CMD_ID: the entry number, unique in a run (a good key for the operation).
    pub fn id(&self) -> &str {
        &self.id
    }
    /// HYDO_CMD: `<kind>_<name>` of the running command (or the check's name).
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn round(&self) -> Option<u32> {
        self.round
    }
    /// A deterministic number in [0, n) from this command's seed (splitmix64 steps).
    pub fn below(&self, n: u64) -> u64 {
        let mut z = self.rng.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.rng.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    }
    /// An environment variable of the plan (`System::env`), or "".
    pub fn env(&self, k: &str) -> String {
        std::env::var(k).unwrap_or_default()
    }
    /// Record that the operation `id` was acknowledged (HYDO_ACKED, HYDO_OUTCOMES).
    pub fn ack(&self, id: &str) -> Result<()> {
        append("HYDO_ACKED", &format!("{}\n", id))?;
        self.outcome(id, "ack")
    }
    /// Record an attempt's outcome: "ack", "failed" (definitely not applied) or "unknown".
    pub fn outcome(&self, id: &str, what: &str) -> Result<()> {
        append("HYDO_OUTCOMES", &format!("{} {}\n", id, what))
    }
    /// Every acknowledged id so far (after a power cut: the ones the first boot recorded).
    pub fn acked(&self) -> Vec<String> {
        std::env::var("HYDO_ACKED").ok().and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default()
    }
    /// Run a program with a deadline; its stdout (trimmed) on exit status 0, else Err(failed).
    pub fn exec(&self, argv: &[&str], deadline: Duration) -> Result<String> {
        exec(argv, deadline)
    }
}

fn append(var: &str, text: &str) -> Result<()> {
    let p = std::env::var(var).map_err(|_| fail!("{} is not set (not running under hydo?)", var))?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// Run `argv` with a deadline (killed after it); stdout trimmed on exit status 0.
pub fn exec(argv: &[&str], deadline: Duration) -> Result<String> {
    let mut child = Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped())
        .stderr(Stdio::null()).spawn().map_err(|e| fail!("{}: {}", argv[0], e))?;
    let t0 = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            let mut out = String::new();
            if let Some(mut o) = child.stdout.take() {
                o.read_to_string(&mut out)?;
            }
            return if st.success() { Ok(out.trim().to_string()) } else { Err(fail!("{} exited {}", argv.join(" "), st)) };
        }
        if t0.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail!("{}: no answer within {:?}", argv.join(" "), deadline));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ------------------------------------------------------------------ the binary
/// `hydo_test::main!(system);` -- the test binary's `main`.
#[macro_export]
macro_rules! main {
    ($system:path) => {
        fn main() {
            ::std::process::exit($crate::run_main($system))
        }
    };
}

fn usage() -> i32 {
    eprintln!("usage: <bin> plan --guest-binary PATH [--guest-path /bin/NAME] [--out FILE]   (host: print the plan)\n       \
               <bin> driver NAME | check NAME                                    (guest: run one)\n       \
               <bin> list");
    2
}

/// Dispatch: `plan` (host), `driver <name>` / `check <name>` (guest), `list`.
pub fn run_main(system: fn() -> System) -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let prog = std::path::Path::new(&args[0]).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    match args.get(1).map(|s| s.as_str()) {
        Some("plan") => {
            let mut opt: BTreeMap<String, String> = BTreeMap::new();
            let mut i = 2;
            while i + 1 < args.len() {
                opt.insert(args[i].clone(), args[i + 1].clone());
                i += 2;
            }
            let guest = match opt.get("--guest-binary") {
                Some(g) => g.clone(),
                None => return usage(),
            };
            let gpath = opt.get("--guest-path").cloned().unwrap_or_else(|| format!("/bin/{}", prog));
            let out = opt.get("--out").map(std::path::PathBuf::from);
            let base = out.as_ref().map(|o| o.parent().map(|d| if d.as_os_str().is_empty() { std::path::Path::new(".") } else { d })
                .unwrap_or(std::path::Path::new(".")).to_path_buf());
            let text = system().plan_json_at(&guest, &gpath, &registrations(), base.as_deref());
            match opt.get("--out") {
                Some(p) => {
                    if let Err(e) = std::fs::write(p, &text) {
                        eprintln!("{}: {}", p, e);
                        return 1;
                    }
                }
                None => print!("{}", text),
            }
            0
        }
        Some(role @ ("driver" | "check")) => {
            let name = match args.get(2) {
                Some(n) => n,
                None => return usage(),
            };
            let reg = match registrations().into_iter().find(|r| r.role == role && r.name == name) {
                Some(r) => r,
                None => {
                    eprintln!("{}: no {} named {}", prog, role, name);
                    return 2;
                }
            };
            let ctx = Ctx::from_env();
            let r = (reg.func)(&ctx);
            match (&r, role) {
                (Ok(()), _) => 0,
                (Err(e), _) => {
                    let (Error::Failed(m) | Error::Violation(m)) = e;
                    println!("{}", m);      // the last output line is the invariant's details
                    let v = matches!(e, Error::Violation(_));
                    match (role, v) { ("driver", true) => 100, ("driver", false) => 1, (_, true) => 1, _ => 2 }
                }
            }
        }
        Some("list") => {
            for r in registrations() {
                println!("{} {} {}", r.role, r.kind, r.name);
            }
            0
        }
        _ => usage(),
    }
}

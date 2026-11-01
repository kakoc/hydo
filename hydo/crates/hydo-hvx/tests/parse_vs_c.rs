//! The schedule parser against the C engine's own `parse_sched`, for hydo-schedule's sample
//! corpus and for seeded mutations of it (bytes changed, tokens dropped or doubled, numbers made
//! huge, negative or not numbers, cut short): the Rust parser must print what the C one printed
//! for every input, refusals included.
//!
//! The C engine (`hv/hvx/hvx_explore.c`) was deleted in M137, so its answers are recorded in
//! `tests/golden/parse_sched_c.txt`: the number of inputs and of refusals, then the SHA-256 of
//! every chunk of `CHUNK` answer lines, so a difference is placed within a chunk. They were
//! recorded on the bare-metal test machine by compiling `hvx_explore.c` with its `main` renamed
//! and the probe below that prints every field `parse_sched` fills. To check against the C itself
//! again (line by line), or to record the golden anew from it, give the C file:
//!
//! ```text
//! git show d9202e7:hv/hvx/hvx_explore.c > /tmp/hvx_explore.c   # and ptedges.h beside it
//! HYDO_HVX_C_SRC=/tmp/hvx_explore.c [HYDO_BLESS=1] cargo test -p hydo-hvx --test parse_vs_c
//! ```
//! (Linux with libipt; the C includes `hvx_stack_verify.c`, `hvx_boot.h` and `hvx_abi.h` from
//! `hv/hvx`, which stay.)

use hydo_hvx::sched::Sched;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Answer lines per chunk of the golden.
const CHUNK: usize = 256;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

const PROBE: &str = r#"
static void pr(const char *in) {
    struct sched s;
    int i;
    if (parse_sched(in, &s)) { printf("REFUSED\n"); return; }
    printf("host=[%s] guest=[%s] cl=%u pr=%u ds=%llx cut=%d tj=%d %llx %llu %u %u %llu %llu",
        s.host, s.guest, s.classes, s.prob, (unsigned long long)s.dseed, s.cut, s.tj_on,
        (unsigned long long)s.tj.seed, (unsigned long long)s.tj.max_delay, s.tj.permille,
        s.tj.n_explicit, (unsigned long long)s.tj.tick_lo, (unsigned long long)s.tj.tick_hi);
    for (i = 0; i < (int)s.tj.n_explicit; i++)
        printf(" %llu:%llu", (unsigned long long)s.tj.explicit_[i].tick, (unsigned long long)s.tj.explicit_[i].delay);
    printf(" rb=%llu pp=%llu pptsc=%llu xt=%d %llx %u %u %llu %llu %llu %u", (unsigned long long)s.rb,
        (unsigned long long)s.pp, (unsigned long long)s.pptsc, s.xt_on, (unsigned long long)s.xt.seed,
        s.xt.rate, s.xt.permille, (unsigned long long)s.xt.lo, (unsigned long long)s.xt.hi,
        (unsigned long long)s.xt.tsc, s.xt.n_explicit);
    for (i = 0; i < (int)s.xt.n_explicit; i++) printf(" %llu", (unsigned long long)s.xt.explicit_[i]);
    printf(" cf=%d %llu q=%d %llu %llx tc=%d %llu %llx %llu %llu %u\n", s.cf_on, (unsigned long long)s.cf,
        s.q_on, (unsigned long long)s.q, (unsigned long long)s.qs, s.tc_on, (unsigned long long)s.tc.margin,
        (unsigned long long)s.tc.burst_seed, (unsigned long long)s.tc.burst_len,
        (unsigned long long)s.tc.burst_q, s.tc.burst_permille);
}
int main(void) {
    char *ln = NULL; size_t cap = 0; ssize_t r;
    while ((r = getline(&ln, &cap, stdin)) > 0) {
        /* the corpus has one input per line; a \x01 stands for the input's own new line */
        char *p;
        if (r && ln[r - 1] == '\n') ln[--r] = 0;
        for (p = ln; *p; p++) if (*p == 1) *p = '\n';
        pr(ln);
    }
    return 0;
}
"#;

fn rust_line(input: &[u8]) -> String {
    let Some(s) = Sched::parse(input) else {
        return "REFUSED".into();
    };
    let mut o = format!(
        "host=[{}] guest=[{}] cl={} pr={} ds={:x} cut={} tj={} {:x} {} {} {} {} {}",
        String::from_utf8_lossy(s.host()),
        String::from_utf8_lossy(s.guest()),
        s.classes,
        s.prob,
        s.dseed,
        s.cut as i32,
        s.tj_on as i32,
        s.tj.seed,
        s.tj.max_delay,
        s.tj.permille,
        s.tj.n_explicit,
        s.tj.tick_lo,
        s.tj.tick_hi
    );
    for e in &s.tj.explicit_[..s.tj.n_explicit as usize] {
        let _ = write!(o, " {}:{}", e.tick, e.delay);
    }
    let _ = write!(
        o,
        " rb={} pp={} pptsc={} xt={} {:x} {} {} {} {} {} {}",
        s.rb,
        s.pp,
        s.pptsc,
        s.xt_on as i32,
        s.xt.seed,
        s.xt.rate,
        s.xt.permille,
        s.xt.lo,
        s.xt.hi,
        s.xt.tsc,
        s.xt.n_explicit
    );
    for e in &s.xt.explicit_[..s.xt.n_explicit as usize] {
        let _ = write!(o, " {e}");
    }
    let _ = write!(
        o,
        " cf={} {} q={} {} {:x} tc={} {} {:x} {} {} {}",
        s.cf_on as i32,
        s.cf,
        s.q_on as i32,
        s.q,
        s.qs,
        s.tc_on as i32,
        s.tc.margin,
        s.tc.burst_seed,
        s.tc.burst_len,
        s.tc.burst_q,
        s.tc.burst_permille
    );
    o
}

/// Seeded mutations of a schedule (printable ASCII only, one line each).
fn mutations(s: &str, seed: u64, out: &mut Vec<String>) {
    let mut x = seed.wrapping_mul(0x9e3779b97f4a7c15) | 1;
    let mut rnd = |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % n.max(1) as u64) as usize
    };
    let words: Vec<&str> = s.split(' ').collect();
    let pieces = [
        "0",
        "-1",
        "x",
        "99999999999999999999",
        "0x10",
        ":",
        ",",
        "=",
        " ",
        "",
        "18446744073709551615",
        "tjx=1:2,3:4",
        "xtx=5,6",
        "q=7",
        "qs=0",
        "qb=10:2:1001",
        "qbs=5",
        "qm=7",
        "cf=0",
        "cfn",
        "disk=cut",
        "C=3",
        "rb=",
        "tj=",
        "pp=",
        "xt=ff",
        "tjx=9",
        "xtx=",
        "qb=1:1:1",
    ];
    for _ in 0..12 {
        let mut w: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        match rnd(6) {
            0 => {
                let i = rnd(w.len());
                w.remove(i);
            }
            1 => {
                let i = rnd(w.len());
                let d = w[i].clone();
                w.insert(i, d);
            }
            2 => {
                let i = rnd(w.len());
                if let Some((k, _)) = w[i].clone().split_once('=') {
                    w[i] = format!("{k}={}", pieces[rnd(pieces.len())]);
                }
            }
            3 => {
                let i = rnd(w.len() + 1);
                w.insert(i, pieces[rnd(pieces.len())].to_string());
            }
            4 => {
                let j = w.join(" ");
                let n = rnd(j.len() + 1);
                out.push(j[..n].to_string());
                continue;
            }
            _ => {
                let mut b: Vec<u8> = w.join(" ").into_bytes();
                if !b.is_empty() {
                    let i = rnd(b.len());
                    b[i] = b" =:,|0123456789abcdefxq-"[rnd(24)];
                }
                out.push(String::from_utf8(b).unwrap());
                continue;
            }
        }
        out.push(w.join(" "));
    }
}

/// The inputs: every sample schedule, the same with a new line, its mutations, and some extras.
fn inputs() -> Vec<String> {
    let sample =
        std::fs::read_to_string(root().join("hydo/crates/hydo-schedule/tests/sample.txt")).unwrap();
    let mut inputs: Vec<String> = Vec::new();
    for (i, l) in sample.lines().enumerate() {
        inputs.push(l.to_string());
        inputs.push(format!("{l}\x01"));
        mutations(l, i as u64 + 1, &mut inputs);
    }
    for extra in [
        "|",
        " | ",
        "disk=none|run",
        "disk=none | ",
        "disk= cut | run C=1",
        "dseed=zz | run",
        "disk=cut cf=4 | run",
        "disk=cut cf=4 | run C=2",
        "disk=ioerr:abc | run",
        "disk=lying:-5 | run",
        "q=5000 qs=1f qb=100:3:50 qbs=9 qm=300 | run",
        "tjx=1:2,3 | run",
        "xtx=1,2,a | run",
    ] {
        inputs.push(extra.to_string());
    }
    inputs
}

/// The golden's text for these answer lines.
fn digest(lines: &[String]) -> String {
    let refused = lines.iter().filter(|l| *l == "REFUSED").count();
    let mut o = format!("inputs {} refused {}\n", lines.len(), refused);
    for (i, ch) in lines.chunks(CHUNK).enumerate() {
        let mut h = Sha256::new();
        for l in ch {
            h.update(l.as_bytes());
            h.update(b"\n");
        }
        let _ = writeln!(o, "chunk {i} {} {:x}", ch.len(), h.finalize());
    }
    o
}

/// The C parser's answers, from `HYDO_HVX_C_SRC` compiled with the probe (None: not given, or it
/// does not compile here).
fn c_answers(src: &Path, inputs: &[String]) -> Option<Vec<String>> {
    let d = std::env::temp_dir().join(format!("hydo-hvx-parse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let text = std::fs::read_to_string(src).unwrap();
    let text = text.replacen(
        "int main(int argc, char **argv)\n{",
        "int hvx_explore_main(int argc, char **argv)\n{",
        1,
    );
    assert!(
        text.contains("hvx_explore_main"),
        "hvx_explore.c's main moved"
    );
    std::fs::write(d.join("probe.c"), format!("{text}\n{PROBE}")).unwrap();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let st = Command::new(cc)
        .args(["-O0", "-w", "-o"])
        .arg(d.join("probe"))
        .arg(d.join("probe.c"))
        .arg("-I")
        .arg(src.parent().unwrap())
        .arg("-I")
        .arg(root().join("hv/hvx"))
        .arg("-lipt")
        .status()
        .unwrap();
    if !st.success() {
        eprintln!(
            "parse_vs_c: {} does not compile here (libipt?)",
            src.display()
        );
        return None;
    }
    let mut c = Command::new(d.join("probe"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = c.stdin.take().unwrap();
    let feed = inputs.join("\n") + "\n";
    let t = std::thread::spawn(move || stdin.write_all(feed.as_bytes()).unwrap());
    let out = c.wait_with_output().unwrap();
    t.join().unwrap();
    let got = String::from_utf8_lossy(&out.stdout).into_owned();
    let got: Vec<String> = got.lines().map(str::to_string).collect();
    assert_eq!(got.len(), inputs.len());
    let _ = std::fs::remove_dir_all(&d);
    Some(got)
}

#[test]
fn parse_sched_matches_the_c_engine() {
    let inputs = inputs();
    let rust: Vec<String> = inputs
        .iter()
        .map(|inp| rust_line(&inp.replace('\x01', "\n").into_bytes()))
        .collect();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/parse_sched_c.txt");
    if let Some(src) = std::env::var_os("HYDO_HVX_C_SRC") {
        if let Some(c) = c_answers(Path::new(&src), &inputs) {
            let mut bad = 0;
            for ((inp, cl), r) in inputs.iter().zip(&c).zip(&rust) {
                if r != cl {
                    bad += 1;
                    if bad <= 10 {
                        eprintln!("input {inp:?}\n  C    {cl}\n  Rust {r}");
                    }
                }
            }
            assert_eq!(bad, 0, "{bad} inputs parse differently from the C");
            if std::env::var("HYDO_BLESS").as_deref() == Ok("1") {
                std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
                std::fs::write(&golden, digest(&c)).unwrap();
                println!("parse_vs_c: recorded {}", golden.display());
            }
        }
    }
    let want = std::fs::read_to_string(&golden).expect("tests/golden/parse_sched_c.txt");
    let have = digest(&rust);
    if have != want {
        for (h, w) in have.lines().zip(want.lines()) {
            if h != w {
                eprintln!("differs: Rust {h}\n         C    {w}");
            }
        }
        if let Some(i) = have
            .lines()
            .zip(want.lines())
            .position(|(h, w)| h != w)
            .and_then(|l| l.checked_sub(1))
        {
            for (inp, r) in inputs.iter().zip(&rust).skip(i * CHUNK).take(CHUNK).take(5) {
                eprintln!("  first inputs of the chunk: {inp:?} -> {r}");
            }
        }
        panic!("the Rust parser's answers are not the recorded C answers");
    }
    let refused = rust.iter().filter(|l| *l == "REFUSED").count();
    println!(
        "parse_vs_c: {} inputs ({refused} refused) parsed as the C engine did",
        inputs.len()
    );
}

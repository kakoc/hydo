//! The round trip on the explorer's own output: `tests/corpus.py` prints every schedule string
//! explore.py produces over a large deterministic range (several hundred thousand), each line
//! `<backend>\t<schedule>`, and each schedule must parse and print back byte for byte, with its
//! host and guest halves where the engines split it (at the first '|').
//!
//! One known exception, an explore.py inconsistency (reported, not papered over): with
//! `--backend hvm` on an image without a data disk, `hvm_backend.hvm_supported` turns the disk
//! into `none` after `render` has kept a `cf=` token, so the explorer prints `disk=none ... cf=<k>`.
//! hvx_explore.c refuses that string ("bad schedule"), and serve.c ignores `cf=` altogether, so
//! the parser refuses it as hvx does. Such lines are counted, allowed only on hvm lines, and every
//! other line must round-trip.
//!
//! Needs python3 (the explorer is Python); without it the test says so and passes. Set
//! HYDO_CORPUS_QUICK=1 for a tenth of the corpus.

use hydo_schedule::Schedule;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[test]
fn explorer_corpus_round_trips() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut cmd = Command::new("python3");
    cmd.arg(dir.join("corpus.py"));
    if std::env::var_os("HYDO_CORPUS_QUICK").is_some() {
        cmd.arg("--quick");
    }
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIPPED: python3 is not available ({e}); the corpus needs explore.py");
            return;
        }
    };
    let t0 = Instant::now();
    let out = BufReader::new(child.stdout.take().expect("stdout"));
    let (mut n, mut nhvm, mut hvm_cf, mut bad) = (0usize, 0usize, 0usize, Vec::new());
    for line in out.lines() {
        let line = line.expect("corpus line");
        let (backend, s) = line.split_once('\t').expect("<backend>\\t<schedule>");
        n += 1;
        nhvm += usize::from(backend == "hvm");
        match Schedule::parse(s) {
            Ok(p) => {
                let back = p.to_string();
                if back != s {
                    bad.push(format!("printed differently:\n  in:  {s}\n  out: {back}"));
                } else {
                    let (h, g) = s.split_once('|').expect("a '|'");
                    if p.host_text() != h.trim_end() || p.guest_text() != g.trim_start() {
                        bad.push(format!("host/guest split differs: {s}"));
                    }
                }
            }
            Err(e)
                if backend == "hvm"
                    && s.starts_with("disk=none ")
                    && e.message.starts_with("cf= needs disk=cut") =>
            {
                hvm_cf += 1
            }
            Err(e) => bad.push(format!("refused ({backend}): {s}\n  {e}")),
        }
    }
    let st = child.wait().expect("corpus.py");
    assert!(st.success(), "corpus.py failed: {st}");
    eprintln!(
        "corpus: {n} schedule strings ({nhvm} for hvm), {} round-tripped byte for byte in {:.1} s \
         (generation included); {hvm_cf} hvm strings with `disk=none ... cf=` refused as hvx does",
        n - hvm_cf - bad.len(),
        t0.elapsed().as_secs_f64()
    );
    assert!(n > 10_000, "the corpus is too small ({n})");
    assert!(
        bad.is_empty(),
        "{} of {n} failed; first ones:\n{}",
        bad.len(),
        bad.iter().take(30).cloned().collect::<Vec<_>>().join("\n")
    );
}

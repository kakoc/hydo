//! Report first, shrink later (Milestone 387, docs/design/time-to-first-bug.md, "Report first,
//! shrink later").
//!
//! The explorer writes `exploration.json`, `report.md` and `stats.json` once its first runs and
//! the re-runs of each failure are done, before any shrink ends. Each failure then carries its
//! problem's shrink state ([`hydo_results::shrink_state`]), and a problem without its minimal
//! schedule shows its failing run's schedule and the command that replays it. The shrinks run
//! after that in the pool's last queue, and the report is written again as each one ends.
//!
//! `UNSHRUNK` keeps what a shrink starts from (the failing run's record, its chain number and
//! its fault kinds) for every shrink that has not ended. `hydo explore --shrink-later` leaves
//! every one there, and so does an exploration that was stopped during its shrinks.
//! `hydo shrink <explore dir> --problem N` ([`shrink_main`]) shrinks one problem from it later,
//! with the explorer's own shrinker, and puts the minimal schedule into the same folder:
//! `exploration.json`, `report.md` and `events.jsonl` (the shrink's runs and its record before
//! the stream's end record). The shrink of every problem gives the documents of an exploration
//! that shrank as it went.

use super::*;

/// The file of the shrinks that have not ended, in an exploration's folder.
pub const UNSHRUNK: &str = "unshrunk.json";

/// A failure chosen to shrink: its chain number (the tags `shrink<k+1>-…`, `min<k+1>-a`), its
/// class and signature, its seed, its fault kinds with `--shrink-causes`, and its first run's
/// record, which the shrink starts from.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Chosen {
    pub(super) k: usize,
    pub(super) key: (String, String),
    pub(super) seed: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cause: Option<String>,
    pub(super) rec: Rec,
}

/// A shrink that ended: a failure's (by chain number) or an error kind's (by its place in the
/// list of kinds shrunk).
pub(super) enum Ended {
    Chain(usize, Box<Shrunk>),
    Error(usize, Box<ErrDone>),
}

/// The group a failure's shrink belongs to: its class's shrink group and its signature.
fn group_of(class: &str, sig: &str) -> Group {
    (
        crate::advice::shrink_group(class).to_string(),
        sig.to_string(),
    )
}

/// Each failure's shrink state: `done` when every shrink of its group ended, `shrinking` (or
/// `pending` with `--shrink-later`) while one has not, and `skipped` when none was chosen.
pub(super) fn states<T>(
    failures: &[Failure],
    chosen: &[Chosen],
    done: &[Option<T>],
    later: bool,
) -> Vec<&'static str> {
    use hydo_results::shrink_state as st;
    failures
        .iter()
        .map(|f| {
            let g = group_of(f.run.class.as_deref().unwrap_or(""), &f.signature);
            let mine: Vec<&Chosen> = chosen
                .iter()
                .filter(|c| group_of(&c.key.0, &c.key.1) == g)
                .collect();
            if mine.is_empty() {
                st::SKIPPED
            } else if mine
                .iter()
                .all(|c| done.get(c.k).is_some_and(|d| d.is_some()))
            {
                st::DONE
            } else if later {
                st::PENDING
            } else {
                st::SHRINKING
            }
        })
        .collect()
}

/// A new error kind chosen to shrink: its place in the list of kinds shrunk (the order of their
/// records in the stream), its signature, and the first run in which it was new.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct ErrChosen {
    pub(super) i: usize,
    pub(super) sig: String,
    pub(super) seed: i64,
    pub(super) schedule: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct UnshrunkFile {
    /// What the file is, for a person who opens it.
    #[serde(default)]
    note: String,
    #[serde(default)]
    shrinks: Vec<Chosen>,
    /// The new error kinds whose shrink has not ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    errors: Vec<ErrChosen>,
}

/// Write the shrinks that have not ended to `UNSHRUNK` (replaced at once), or remove the file
/// when every one ended.
pub(super) fn write_unshrunk<T, E>(
    out: &Path,
    chosen: &[Chosen],
    done: &[Option<T>],
    echosen: &[ErrChosen],
    edone: &[Option<E>],
) {
    let open = |d: Option<bool>| d != Some(true);
    let shrinks: Vec<Chosen> = chosen
        .iter()
        .filter(|c| open(done.get(c.k).map(|d| d.is_some())))
        .cloned()
        .collect();
    let errors: Vec<ErrChosen> = echosen
        .iter()
        .filter(|e| open(edone.get(e.i).map(|d| d.is_some())))
        .cloned()
        .collect();
    write_left(
        out,
        UnshrunkFile {
            shrinks,
            errors,
            ..UnshrunkFile::default()
        },
    );
}

fn write_left(out: &Path, mut f: UnshrunkFile) {
    let p = out.join(UNSHRUNK);
    if f.shrinks.is_empty() && f.errors.is_empty() {
        let _ = std::fs::remove_file(&p);
        return;
    }
    f.note = "the shrinks that have not ended; `hydo shrink <this folder> --problem N` shrinks \
              one problem, `--all` every one (Milestone 387)"
        .into();
    let tmp = out.join(format!("{UNSHRUNK}.tmp"));
    if let Ok(t) = serde_json::to_string_pretty(&f) {
        if std::fs::write(&tmp, t + "\n").is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// The shrinks of an exploration that have not ended (empty without the file).
fn unshrunk(dir: &Path) -> Result<UnshrunkFile, String> {
    let p = dir.join(UNSHRUNK);
    match std::fs::read_to_string(&p) {
        Ok(t) => {
            serde_json::from_str::<UnshrunkFile>(&t).map_err(|e| format!("{}: {e}", p.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UnshrunkFile::default()),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// A problem's group: its class's shrink group and its signature.
pub type Group = (String, String);

/// The groups whose shrinks have not ended, with their seeds.
pub fn unshrunk_groups(dir: &Path) -> Result<Vec<(Group, i64)>, String> {
    Ok(unshrunk(dir)?
        .shrinks
        .into_iter()
        .map(|c| (group_of(&c.key.0, &c.key.1), c.seed))
        .collect())
}

/// The new error kinds whose shrink has not ended, by signature.
pub fn unshrunk_errors(dir: &Path) -> Result<Vec<String>, String> {
    Ok(unshrunk(dir)?.errors.into_iter().map(|e| e.sig).collect())
}

/// What `hydo shrink` shrinks: the failures of these first runs, and the new error kinds.
#[derive(Clone, Debug, Default)]
pub struct ShrinkWhat {
    pub seeds: Vec<i64>,
    pub errors: bool,
}

/// `hydo shrink`'s answer: each shrink that ran, as (from seed, minimal schedule, shrink runs),
/// and each error kind's, as (signature, minimal schedule, shrink runs).
pub struct ShrinkOut {
    pub shrunk: Vec<(i64, String, u64)>,
    pub errors: Vec<(String, String, u64)>,
}

/// Shrink what `what` names of the exploration in `dir` (Milestone 387, `hydo shrink`), with
/// the explorer's own shrinker and the engine and options of the exploration's command line.
/// The minimal schedules go into the same folder.
pub fn shrink_main(dir: &Path, what: &ShrinkWhat, env: &Env) -> Result<ShrinkOut, String> {
    let doc_path = dir.join(hydo_results::EXPLORATION_DOC);
    let text = std::fs::read_to_string(&doc_path).map_err(|e| {
        format!(
            "{}: {e} (an exploration's folder has it)",
            doc_path.display()
        )
    })?;
    let mut doc: Exploration =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", doc_path.display()))?;
    if !doc.shards.is_empty() {
        return Err(format!(
            "{}: an exploration merged from parts; shrink the problem in the folder of the part \
             that found it, then merge the parts again",
            dir.display()
        ));
    }
    let all = unshrunk(dir)?;
    let todo: Vec<Chosen> = all
        .shrinks
        .iter()
        .filter(|c| what.seeds.contains(&c.seed))
        .cloned()
        .collect();
    let etodo: Vec<ErrChosen> = if what.errors {
        all.errors.clone()
    } else {
        Vec::new()
    };
    if todo.is_empty() && etodo.is_empty() {
        return Err(format!(
            "{}: nothing of it waits for its shrink ({UNSHRUNK} lists what does)",
            dir.display()
        ));
    }
    // the exploration's own command line, with this folder as its output
    let mut argv = doc.config.argv.clone();
    let out = dir.display().to_string();
    match argv.iter().position(|x| x == "--out") {
        Some(i) if i + 1 < argv.len() => argv[i + 1] = out,
        _ => {
            argv.retain(|x| !x.starts_with("--out="));
            argv.push("--out".into());
            argv.push(out);
        }
    }
    let a = args::parse(&argv).map_err(|e| format!("the exploration's command line: {e}"))?;
    crate::JSON_END.store(!env.plain_end, std::sync::atomic::Ordering::Relaxed);
    let mut cx = setup(a, env);
    cx.argv = argv;
    let pool = make_pool(&cx);
    let (done, edone): (Vec<Shrunk>, Vec<ErrDone>) = std::thread::scope(|sc| {
        let pool = &pool;
        let cx = &cx;
        let hs: Vec<_> = todo
            .iter()
            .map(|c| {
                let c = c.clone();
                sc.spawn(move || shrink_violation(cx, pool, c.k, c.key, &c.rec, c.cause))
            })
            .collect();
        let es: Vec<_> = etodo
            .iter()
            .map(|e| {
                let e = e.clone();
                sc.spawn(move || shrink_errkind(cx, pool, &e))
            })
            .collect();
        (
            hs.into_iter()
                .map(|h| h.join().expect("a shrink"))
                .collect(),
            es.into_iter()
                .map(|h| h.join().expect("a shrink"))
                .collect(),
        )
    });
    pool.close();
    // the stream: the shrinks' runs, the error kinds' runs, then the shrinks' records, before
    // the end record, as one exploration writes them
    let models: Vec<hydo_results::Shrink> = done.iter().map(|s| cx.shrink_model(s)).collect();
    let mut add = String::new();
    for s in &done {
        for r in &s.log {
            add.push_str(&hydo_results::to_line(r));
        }
    }
    for (_, _, log) in &edone {
        for r in log {
            add.push_str(&hydo_results::to_line(r));
        }
    }
    for m in &models {
        add.push_str(&hydo_results::to_line(m));
    }
    let ev = dir.join(hydo_results::EVENTS);
    let old = std::fs::read_to_string(&ev).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().collect();
    let end = lines
        .last()
        .filter(|l| l.contains("\"type\":\"end\""))
        .map(|l| l.to_string());
    if end.is_some() {
        lines.pop();
    }
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    text.push_str(&add);
    if let Some(e) = end {
        text.push_str(&e);
        text.push('\n');
    }
    let tmp = dir.join(format!("{}.tmp", hydo_results::EVENTS));
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &ev).map_err(|e| format!("{}: {e}", ev.display()))?;
    // the document: the shrinks in the order of their first runs, as one exploration has them
    doc.shrinks.extend(models);
    let order = |seed: i64| {
        doc.failures
            .iter()
            .position(|f| f.run.seed == Some(seed))
            .unwrap_or(usize::MAX)
    };
    let mut shrinks = std::mem::take(&mut doc.shrinks);
    shrinks.sort_by_key(|s| order(s.from_seed));
    doc.shrinks = shrinks;
    if let Some(ek) = doc.error_kinds.as_mut() {
        for (sig, es, _) in &edone {
            if let Some(k) = ek.kinds.iter_mut().find(|k| &k.signature == sig) {
                k.shrink = Some(es.clone());
            }
        }
    }
    let left = UnshrunkFile {
        shrinks: all
            .shrinks
            .into_iter()
            .filter(|c| !what.seeds.contains(&c.seed))
            .collect(),
        errors: if what.errors { Vec::new() } else { all.errors },
        ..UnshrunkFile::default()
    };
    use hydo_results::shrink_state as st;
    for f in doc.failures.iter_mut() {
        let g = group_of(f.run.class.as_deref().unwrap_or(""), &f.signature);
        if left
            .shrinks
            .iter()
            .any(|c| group_of(&c.key.0, &c.key.1) == g)
        {
            f.shrink_state = Some(st::PENDING.into());
        } else if doc
            .shrinks
            .iter()
            .any(|s| group_of(&s.class, &s.signature) == g)
        {
            f.shrink_state = Some(st::DONE.into());
            f.repro_command = None;
        }
    }
    hydo_results::write_document(&doc_path, &doc)
        .map_err(|e| format!("{}: {e}", doc_path.display()))?;
    let p = dir.join("report.md");
    let tmp = dir.join("report.md.tmp");
    std::fs::write(&tmp, md::report(&doc)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    write_left(dir, left);
    Ok(ShrinkOut {
        shrunk: done
            .iter()
            .map(|s| {
                (
                    s.rec["from_seed"].as_i64().unwrap_or(0),
                    py_str(&s.rec["minimal"]),
                    s.rec["tries"].as_u64().unwrap_or(0),
                )
            })
            .collect(),
        errors: edone
            .iter()
            .map(|(sig, es, _)| (sig.clone(), es.minimal.clone(), es.tries))
            .collect(),
    })
}

//! The lenient reader: what every consumer of hydo's results does, and what `hydo report`,
//! `hydo repro`, `hydo save` and `hydo errors` do.
//!
//! The rule (tests/reader.rs is its specification):
//!
//! 1. A record or document is a JSON object with an integer `v` and a string `type`. Anything else
//!    is not a results record, and reading it is an error.
//! 2. Fields a reader does not know are ignored.
//! 3. A stream record whose `type` the reader does not know, or whose `v` is a version it does
//!    not read, is skipped (and listed in [`Stream::skipped`]); the records around it are read.
//! 4. A document of a version the reader does not read is an error that names the version.
//! 5. A known record with a known field of the wrong JSON type is an error (a broken writer).
//! 6. A stream's last line without its newline (a stream still being written, or cut off) is
//!    ignored; any other line that is not JSON is an error.

use crate::v1::{
    CiFindingEvent, CiLedger, CiRun, DiagnosisDoc, Diff, End, Exploration, Inspect, InspectError,
    Run, Shrink, Start, Timeline, VersionsDoc,
};
use crate::{EVENTS, EXPLORATION_DOC, RUN_DOC};
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};

/// The versions this reader reads.
pub const READS: &[u64] = &[1];

/// One record this reader knows.
#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Start(Box<Start>),
    Run(Box<Run>),
    Shrink(Box<Shrink>),
    End(Box<End>),
    Exploration(Box<Exploration>),
    /// `diff.json` (`hydo diff`, added in M165).
    Diff(Box<Diff>),
    /// `hydo inspect diagnose` (added in M178).
    Diagnosis(Box<DiagnosisDoc>),
    /// `hydo inspect events` (added in M178).
    Timeline(Box<Timeline>),
    /// `hydo inspect`'s answers (added in M178).
    Inspect(Box<Inspect>),
    InspectError(Box<InspectError>),
    /// `ci-run.json` (`hydo ci run`, added after 0.2.0).
    CiRun(Box<CiRun>),
    /// `versions.json` (`hydo versions`, added in Milestone 221).
    Versions(Box<VersionsDoc>),
    /// The history store: `findings.jsonl` and `state.json` (`hydo ci record`; added in stage 2).
    CiFindingEvent(Box<CiFindingEvent>),
    CiLedger(Box<CiLedger>),
}

/// A record the reader skipped (rule 3).
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    /// The 1-based line of a stream (0 for a document).
    pub line: usize,
    pub v: u64,
    pub ty: String,
}

/// A stream as read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stream {
    pub records: Vec<Record>,
    pub skipped: Vec<Skipped>,
    /// The last line had no newline and was ignored (rule 6).
    pub incomplete_tail: bool,
}

impl Stream {
    pub fn start(&self) -> Option<&Start> {
        self.records.iter().find_map(|r| match r {
            Record::Start(s) => Some(&**s),
            _ => None,
        })
    }
    pub fn runs(&self) -> impl Iterator<Item = &Run> {
        self.records.iter().filter_map(|r| match r {
            Record::Run(x) => Some(&**x),
            _ => None,
        })
    }
    pub fn shrinks(&self) -> impl Iterator<Item = &Shrink> {
        self.records.iter().filter_map(|r| match r {
            Record::Shrink(x) => Some(&**x),
            _ => None,
        })
    }
    pub fn end(&self) -> Option<&End> {
        self.records.iter().find_map(|r| match r {
            Record::End(s) => Some(&**s),
            _ => None,
        })
    }
}

/// Why a read failed.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadError(pub String);

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReadError {}

fn err(s: String) -> ReadError {
    ReadError(s)
}

/// What a JSON value says about itself: (v, type), or why it is not a record (rule 1).
fn header(v: &Value) -> Result<(u64, String), String> {
    let o = v.as_object().ok_or("not a JSON object")?;
    let ver = o
        .get("v")
        .ok_or("no \"v\" (not a hydo results record)")?
        .as_u64()
        .ok_or("\"v\" is not a whole number")?;
    let ty = o
        .get("type")
        .ok_or("no \"type\" (not a hydo results record)")?
        .as_str()
        .ok_or("\"type\" is not a string")?;
    Ok((ver, ty.to_string()))
}

/// One record: Ok(None) when it is of a type or version this reader does not read.
pub fn record(v: Value) -> Result<Option<Record>, String> {
    let (ver, ty) = header(&v)?;
    if !READS.contains(&ver) {
        return Ok(None);
    }
    fn de<T: serde::de::DeserializeOwned>(v: Value, ty: &str) -> Result<Box<T>, String> {
        serde_json::from_value(v)
            .map(Box::new)
            .map_err(|e| format!("a \"{ty}\" record: {e}"))
    }
    Ok(Some(match ty.as_str() {
        "start" => Record::Start(de(v, &ty)?),
        "run" => Record::Run(de(v, &ty)?),
        "shrink" => Record::Shrink(de(v, &ty)?),
        "end" => Record::End(de(v, &ty)?),
        "exploration" => Record::Exploration(de(v, &ty)?),
        "diff" => Record::Diff(de(v, &ty)?),
        "diagnosis" => Record::Diagnosis(de(v, &ty)?),
        "timeline" => Record::Timeline(de(v, &ty)?),
        "inspect" => Record::Inspect(de(v, &ty)?),
        "inspect-error" => Record::InspectError(de(v, &ty)?),
        "ci-run" => Record::CiRun(de(v, &ty)?),
        "versions" => Record::Versions(de(v, &ty)?),
        "finding-event" => Record::CiFindingEvent(de(v, &ty)?),
        "ci-ledger" => Record::CiLedger(de(v, &ty)?),
        _ => return Ok(None),
    }))
}

/// A stream's text (rules 3 and 6).
pub fn stream(text: &str) -> Result<Stream, ReadError> {
    let mut s = Stream::default();
    let mut lines = text.split_inclusive('\n').peekable();
    let mut n = 0;
    while let Some(line) = lines.next() {
        n += 1;
        if !line.ends_with('\n') && lines.peek().is_none() {
            s.incomplete_tail = !line.trim().is_empty();
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).map_err(|e| err(format!("line {n}: not JSON ({e})")))?;
        let (ver, ty) = header(&v).map_err(|e| err(format!("line {n}: {e}")))?;
        match record(v).map_err(|e| err(format!("line {n}: {e}")))? {
            Some(r) => s.records.push(r),
            None => s.skipped.push(Skipped {
                line: n,
                v: ver,
                ty,
            }),
        }
    }
    Ok(s)
}

/// A document's text (rule 4): its record, or an error for a version or a type this reader does
/// not read.
pub fn document(text: &str) -> Result<Record, ReadError> {
    let v: Value = serde_json::from_str(text).map_err(|e| err(format!("not JSON ({e})")))?;
    let (ver, ty) = header(&v).map_err(err)?;
    if !READS.contains(&ver) {
        return Err(err(format!(
            "results version {ver} (this hydo reads version {}); a newer hydo wrote it",
            READS
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    record(v).map_err(err)?.ok_or_else(|| {
        err(format!(
            "a document of type \"{ty}\", which this hydo does not read"
        ))
    })
}

fn read(p: &Path) -> Result<String, ReadError> {
    std::fs::read_to_string(p).map_err(|e| err(format!("{}: {e}", p.display())))
}

/// `result.json`, or any file holding one run document.
pub fn read_run(p: &Path) -> Result<Run, ReadError> {
    match document(&read(p)?).map_err(|e| err(format!("{}: {e}", p.display())))? {
        Record::Run(r) => Ok(*r),
        _ => Err(err(format!("{}: not a run document", p.display()))),
    }
}

/// `exploration.json`.
pub fn read_exploration(p: &Path) -> Result<Exploration, ReadError> {
    match document(&read(p)?).map_err(|e| err(format!("{}: {e}", p.display())))? {
        Record::Exploration(r) => Ok(*r),
        _ => Err(err(format!("{}: not an exploration document", p.display()))),
    }
}

/// `diff.json` (added in M165).
pub fn read_diff(p: &Path) -> Result<Diff, ReadError> {
    match document(&read(p)?).map_err(|e| err(format!("{}: {e}", p.display())))? {
        Record::Diff(r) => Ok(*r),
        _ => Err(err(format!("{}: not a diff document", p.display()))),
    }
}

/// `events.jsonl`.
pub fn read_events(p: &Path) -> Result<Stream, ReadError> {
    stream(&read(p)?).map_err(|e| err(format!("{}: {e}", p.display())))
}

/// What a results directory holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Results {
    /// A run directory (`result.json`).
    Run { path: PathBuf, run: Box<Run> },
    /// An exploration directory: `exploration.json` (absent while it still runs) and its stream.
    Exploration {
        path: PathBuf,
        doc: Option<Box<Exploration>>,
        events: Option<Stream>,
    },
}

/// A run or exploration directory. Err names the directory when it holds neither.
pub fn read_dir(d: &Path) -> Result<Results, ReadError> {
    let rd = d.join(RUN_DOC);
    if rd.is_file() {
        return Ok(Results::Run {
            run: Box::new(read_run(&rd)?),
            path: rd,
        });
    }
    let ed = d.join(EXPLORATION_DOC);
    let ev = d.join(EVENTS);
    if ed.is_file() || ev.is_file() {
        let doc = if ed.is_file() {
            Some(Box::new(read_exploration(&ed)?))
        } else {
            None
        };
        let events = if ev.is_file() {
            Some(read_events(&ev)?)
        } else {
            None
        };
        return Ok(Results::Exploration {
            path: if ed.is_file() { ed } else { ev },
            doc,
            events,
        });
    }
    Err(err(format!(
        "{} has no {RUN_DOC} (hydo run) and no {EXPLORATION_DOC} or {EVENTS} (hydo explore)",
        d.display()
    )))
}

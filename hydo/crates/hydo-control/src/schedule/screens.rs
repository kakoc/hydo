//! Screenshots out of the guest (Milestone 254, `browser:`; docs/design/web-apps.md).
//!
//! The watcher of `browser:` (hydo-manifest's `browser/hook.cjs`) saves a screenshot of a page in
//! the guest folder [`crate::Plan::screens`] and adds one line to its `index.jsonl` there:
//! `{"file":"<name>.png","test":..,"url":..,"why":..}`. After the verdict the controller reads
//! the index and each file, and sends each screenshot to the host as one note:
//! `HYDO-SCREEN {"file":..,"test":..,"url":..,"why":..,"bytes":n,"hex":"<the file in hex>"}`.
//! [`crate::agent::write_run_files`] writes the files into the run directory (`screens/`) and
//! keeps only a short line in the console and the transcript.
//!
//! A note is for people: no control transcript's hash and no fingerprint covers it. The reads
//! come after the verdict, so they change nothing the run decided. Without `browser:` nothing
//! here runs, and a run sends the same commands as before.
//!
//! A run copies at most [`MAX_BYTES`] (Milestone 264). Every byte crosses the agent's channel
//! twice, as hex, and the engines keep that channel's lines in a buffer of their own: 1 MiB of
//! the agent's lines per boot on hvx before Milestone 264 (16 MiB since), 4 MiB of console on
//! KVM. A copy that filled the buffer lost its replies, and the run waited for them until the
//! engine's limit, `TIMEOUT check`: the "about 386 reads" of Milestone 258. The bound is a number
//! of bytes, not a time, so that the same run copies the same files and keeps its fingerprint.
//! A file that does not fit what is left stays in the guest, and a note names it.
//!
//! Milestone 334: the filmstrip of a walk. An image whose walker keeps one ([`crate::Plan::film`])
//! also has `film.jsonl` in the folder, one line per action with the file of a small screenshot,
//! and in its index a Playwright trace of every step, marked `only: failed`. The controller then
//! copies, per boot, at most [`BOOT_BYTES`] and each file up to [`FILE_MAX`]: first the index's
//! other files, then `film.jsonl` and every screenshot it names, then, when the verdict is not OK,
//! the traces, the step that the verdict names first and the latest steps after it. A power cut
//! ends the first boot without a verdict, and the guest's `/tmp` is gone in the recovery boot, so
//! the controller copies the filmstrip and the trace of the last step at the cut, with the
//! services frozen, before the engine cuts the power ([`At::Cut`]); the recovery boot copies its
//! own files after its verdict. The engines' channels hold it: hvx keeps 16 MiB of the agent's
//! lines per boot, and the Mac and KVM engines 32 MiB of console since Milestone 334 (4 MiB
//! before). An image built before keeps the copy of Milestone 264, with the same commands.

use super::text::{read_hex_files, read_hex_sized, read_hex_window};
use super::{read_all, Ctx, F};
use crate::flow::Step;
use serde_json::Value;

/// The note that carries one screenshot.
pub const NOTE: &str = "HYDO-SCREEN";
/// The most screenshots a run copies out.
pub const MAX_FILES: usize = 64;
/// The most bytes of screenshots, recordings and tests a run copies out (Milestone 264: 16 MiB
/// before, more than the engines' channels hold).
pub const MAX_BYTES: usize = 1024 * 1024;
/// Milestone 334: the most bytes a boot copies out of an image with a filmstrip
/// ([`crate::Plan::film`]). As hex with its notes it is about 10.3 MiB of the channel.
pub const BOOT_BYTES: usize = 5 * 1024 * 1024;
/// Milestone 334: the largest one file of an image with a filmstrip that a boot copies out.
pub const FILE_MAX: usize = 3 * 1024 * 1024;
/// Milestone 334: the most frames of a filmstrip a boot copies out.
pub const FILM_FILES: usize = 600;
/// The walker's filmstrip in the screenshots' folder (Milestone 334).
pub const FILM: &str = "film.jsonl";

/// When the controller copies the files out.
pub(super) enum At<'a> {
    /// After the verdict of a boot (which decides whether the traces are copied).
    Verdict(&'a str),
    /// At a power cut, before the engine cuts the power: the filmstrip and the last step's trace.
    Cut,
}

/// A file name the index may give: a plain name, no folder.
pub fn plain_name(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 160
        && !f.starts_with('.')
        && f.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

/// After the verdict (and, for an image with a filmstrip, at a power cut): every screenshot of
/// the index as a `HYDO-SCREEN` note.
pub(super) fn collect(fx: &mut F<'_>, ctx: &Ctx, at: At<'_>) -> Step<()> {
    let Some(dir) = ctx.plan.screens.clone() else {
        return Ok(());
    };
    if !ctx.plan.film {
        // an image built before Milestone 334: its copy after the verdict, as it was
        if let At::Cut = at {
            return Ok(());
        }
        return copy_index(fx, &dir);
    }
    let mut c = Copier {
        dir: dir.clone(),
        max: BOOT_BYTES,
        total: 0,
        seen: std::collections::BTreeSet::new(),
    };
    let index = read_all(fx, &format!("{dir}/index.jsonl"))?;
    let entries: Vec<serde_json::Map<String, Value>> = index
        .lines()
        .take(MAX_FILES * 2)
        .filter_map(|l| match serde_json::from_str::<Value>(l) {
            Ok(Value::Object(e)) => Some(e),
            _ => None,
        })
        .collect();
    let later = |e: &serde_json::Map<String, Value>| {
        e.get("only").and_then(Value::as_str) == Some("failed")
    };
    let mut n = 0;
    for e in entries.iter().filter(|e| !later(e)) {
        n += 1;
        if n > MAX_FILES {
            break;
        }
        c.copy(fx, e.clone())?;
    }
    // the filmstrip: its lines, then the frame of each
    let film = read_all(fx, &format!("{dir}/{FILM}"))?;
    if !film.is_empty() {
        let mut e = serde_json::Map::new();
        e.insert("file".into(), Value::String(FILM.into()));
        e.insert(
            "why".into(),
            Value::String("the walk's actions, one line each, with a small screenshot".into()),
        );
        c.copy(fx, e)?;
        // the frames whose sizes the walker wrote are read many at a time; the others one by one
        let mut known: Vec<(String, u64)> = Vec::new();
        let mut frames = 0;
        for line in film.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(f) = v.get("file").and_then(Value::as_str) else {
                continue;
            };
            frames += 1;
            if frames > FILM_FILES {
                break;
            }
            match v.get("bytes").and_then(Value::as_u64) {
                Some(n) if plain_name(f) && !c.seen.contains(f) => {
                    if c.total + n as usize > c.max || n as usize > FILE_MAX {
                        continue;
                    }
                    c.seen.insert(f.to_string());
                    c.total += n as usize;
                    known.push((format!("{dir}/{f}"), n));
                }
                Some(_) => {}
                None => {
                    let mut e = serde_json::Map::new();
                    e.insert("file".into(), Value::String(f.into()));
                    e.insert("film".into(), Value::Bool(true));
                    c.copy(fx, e)?;
                }
            }
        }
        for part in known.chunks(32) {
            let hexes = read_hex_files(fx, part)?;
            for ((path, _), hex) in part.iter().zip(hexes) {
                if hex.is_empty() {
                    continue;
                }
                let file = path.rsplit('/').next().unwrap_or(path);
                let mut e = serde_json::Map::new();
                e.insert("file".into(), Value::String(file.into()));
                e.insert("film".into(), Value::Bool(true));
                e.insert("bytes".into(), Value::from(hex.len() / 2));
                e.insert("hex".into(), Value::String(hex));
                fx.note(format!("{NOTE} {}", Value::Object(e)));
            }
        }
    }
    // the traces of a failed run: the step that the verdict names first, then the latest
    let failed = match at {
        At::Verdict(v) => !v.starts_with("HYDO-ORACLE: OK"),
        At::Cut => true,
    };
    if failed {
        let step_of = |e: &serde_json::Map<String, Value>| e.get("step").and_then(Value::as_i64);
        let mut traces: Vec<&serde_json::Map<String, Value>> =
            entries.iter().filter(|e| later(e)).collect();
        traces.sort_by_key(|e| std::cmp::Reverse(step_of(e)));
        if let At::Verdict(v) = at {
            if let Some(i) = traces
                .iter()
                .position(|e| step_of(e).is_some_and(|s| v.contains(&format!("walk step {s}"))))
            {
                let named = traces.remove(i);
                traces.insert(0, named);
            }
        }
        let keep = match at {
            At::Verdict(_) => traces.len(),
            At::Cut => 1,
        };
        for e in traces.into_iter().take(keep) {
            c.copy(fx, e.clone())?;
        }
    }
    Ok(())
}

/// The copy of one boot with a filmstrip: what it copied, and its budget.
struct Copier {
    dir: String,
    max: usize,
    total: usize,
    seen: std::collections::BTreeSet<String>,
}

impl Copier {
    /// One file of the folder as a note (a name it copied before, or an unsafe one, is left out).
    fn copy(&mut self, fx: &mut F<'_>, mut e: serde_json::Map<String, Value>) -> Step<()> {
        let Some(file) = e.get("file").and_then(Value::as_str).map(str::to_string) else {
            return Ok(());
        };
        if !plain_name(&file) || !self.seen.insert(file.clone()) {
            return Ok(());
        }
        let room = (self.max - self.total).min(FILE_MAX);
        let (bytes, size) = read_hex_window(fx, &format!("{}/{file}", self.dir), room)?;
        if bytes.is_empty() {
            if size as usize > room {
                let why = if room == FILE_MAX {
                    format!("a run copies at most {FILE_MAX} bytes of one file out of the guest")
                } else {
                    format!(
                        "a boot copies at most {} bytes of files out of the guest; {room} were left",
                        self.max
                    )
                };
                fx.note(format!(
                    "{NOTE}-LEFT {{\"file\":{},\"bytes\":{size},\"why\":{}}}",
                    Value::String(file.clone()),
                    Value::String(why)
                ));
            }
            return Ok(());
        }
        self.total += bytes.len() / 2;
        e.insert("bytes".into(), Value::from(bytes.len() / 2));
        e.insert("hex".into(), Value::String(bytes));
        fx.note(format!("{NOTE} {}", Value::Object(e)));
        Ok(())
    }
}

/// The copy of Milestone 264, for an image without a filmstrip: the index's files, at most
/// [`MAX_BYTES`] in all.
fn copy_index(fx: &mut F<'_>, dir: &str) -> Step<()> {
    let index = read_all(fx, &format!("{dir}/index.jsonl"))?;
    let mut total = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for line in index.lines().take(MAX_FILES * 2) {
        let Ok(Value::Object(mut e)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(file) = e.get("file").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if !plain_name(&file) || !seen.insert(file.clone()) || seen.len() > MAX_FILES {
            continue;
        }
        let path = format!("{dir}/{file}");
        let (bytes, size) = read_hex_sized(fx, &path, MAX_BYTES - total)?;
        if bytes.is_empty() {
            if size as usize > MAX_BYTES - total {
                fx.note(format!(
                    "{NOTE}-LEFT {{\"file\":{},\"bytes\":{size},\"why\":\"a run copies at most {MAX_BYTES} bytes of files out of the guest; {} were left\"}}",
                    Value::String(file.clone()),
                    MAX_BYTES - total
                ));
            }
            continue;
        }
        total += bytes.len() / 2;
        e.insert("bytes".into(), Value::from(bytes.len() / 2));
        e.insert("hex".into(), Value::String(bytes));
        fx.note(format!("{NOTE} {}", Value::Object(e)));
        if total >= MAX_BYTES {
            break;
        }
    }
    Ok(())
}

/// A screenshot note's file name and bytes (None for another line, or a note whose bytes do not
/// decode).
pub fn of_note(line: &str) -> Option<(String, Vec<u8>, Value)> {
    let rest = line.strip_prefix(NOTE)?.strip_prefix(' ')?;
    let mut v: Value = serde_json::from_str(rest).ok()?;
    let file = v.get("file")?.as_str()?.to_string();
    if !plain_name(&file) {
        return None;
    }
    let hex = v.as_object_mut()?.remove("hex")?;
    let hex = hex.as_str()?;
    if hex.len() % 2 != 0 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect();
    Some((file, bytes, v))
}

/// The short line a screenshot note becomes in the console and the transcript file.
pub fn short(line: &str) -> Option<String> {
    let (_, _, v) = of_note(line)?;
    Some(format!("{NOTE} {v}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_gives_its_file_and_a_short_line() {
        let l = r#"HYDO-SCREEN {"file":"a-1.png","test":"t","bytes":3,"hex":"89504e"}"#;
        let (f, b, v) = of_note(l).unwrap();
        assert_eq!(f, "a-1.png");
        assert_eq!(b, vec![0x89, 0x50, 0x4e]);
        assert!(v.get("hex").is_none());
        assert_eq!(
            short(l).unwrap(),
            r#"HYDO-SCREEN {"file":"a-1.png","test":"t","bytes":3}"#
        );
        assert!(of_note("HYDO-SCREEN {\"file\":\"../x.png\",\"hex\":\"00\"}").is_none());
        assert!(of_note("HYDO-SCREEN {\"file\":\"x.png\",\"hex\":\"0\"}").is_none());
        assert!(of_note("hydo-web: test").is_none());
    }

    #[test]
    fn only_plain_names_are_copied() {
        assert!(plain_name("checkout-spec-ts-places-an-order-1.png"));
        assert!(!plain_name("a/b.png"));
        assert!(!plain_name(".hidden"));
        assert!(!plain_name(""));
    }
}

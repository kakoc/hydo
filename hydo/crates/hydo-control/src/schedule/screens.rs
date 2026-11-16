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

use super::text::read_hex_sized;
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

/// A file name the index may give: a plain name, no folder.
pub fn plain_name(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 160
        && !f.starts_with('.')
        && f.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

/// After the verdict: every screenshot of the index as a `HYDO-SCREEN` note.
pub(super) fn collect(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(dir) = ctx.plan.screens.clone() else {
        return Ok(());
    };
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

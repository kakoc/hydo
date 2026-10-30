//! hydo's project format as types (a placeholder until the port of hydo-build's parser lands).
//!
//! API contract other crates rely on (keep these names and signatures):
//! - `parse_text(text, name) -> Result<serde_json::Value, ManifestError>`: hydo-build's reading of a
//!   manifest file: JSON when `name` ends in `.json` (or, with `by_content`, when the text starts
//!   with `{`), else hydo-build's YAML subset (`parse_yaml`), as the same JSON data.
//! - `Guest::from_value(&Value)`: the `guest:` section with hydo-build's defaults
//!   (`ram_mib` 1024, `cpu` "v2").
//! - `ManifestError`: `Display` gives hydo-build's message (the CLI prints `hydo-build: <msg>`).

use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    pub message: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

/// Read a manifest file's text as data. JSON when the name ends in `.json`, or when `by_content`
/// is set and the text starts with `{` (an image keeps every plan as /etc/hydo/hydo.yaml).
pub fn parse_text(text: &str, name: &str, by_content: bool) -> Result<Value, ManifestError> {
    if name.ends_with(".json") || (by_content && text.trim_start().starts_with('{')) {
        return serde_json::from_str(text).map_err(|e| ManifestError {
            message: format!("{name}: {e}"),
        });
    }
    Err(ManifestError {
        message: format!("{name}: the YAML reader is not ported yet"),
    })
}

/// The `guest:` section of a manifest, with hydo-build's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guest {
    pub ram_mib: u32,
    pub cpu: String,
}

impl Guest {
    pub fn from_value(m: &Value) -> Guest {
        let g = m.get("guest");
        Guest {
            ram_mib: g
                .and_then(|g| g.get("ram_mib"))
                .and_then(Value::as_u64)
                .unwrap_or(1024) as u32,
            cpu: g
                .and_then(|g| g.get("cpu"))
                .and_then(Value::as_str)
                .unwrap_or("v2")
                .to_string(),
        }
    }
}

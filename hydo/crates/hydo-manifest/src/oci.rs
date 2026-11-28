//! The manifest-side checks of `services[].image` (hydo/onboard/hydo_oci.py `prepare` and
//! `_image_path`) that need no image: OCI keys without `image:`, the shape of `image:` and whether
//! the image file exists. Reading an existing image (its config: entrypoint, env, user) is not
//! ported; such services are reported in `Loaded::oci_unchecked` and keep their image keys.

use serde_json::Value;

use crate::py::{self, str_of};
use crate::validate::{err, iter_of, R};

const OCI_KEYS: &[&str] = &[
    "image",
    "image_ref",
    "entrypoint",
    "workdir",
    "user",
    "env",
    "env_file",
    "mounts",
    "writable",
];
const ARCHES: &[&str] = &["x86_64", "arm64"];

fn expanduser(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{}{}", h.trim_end_matches('/'), &p[1..]);
        }
    }
    p.to_string()
}

/// `services[].env_file` as a path: relative to the manifest's folder, `~` expanded.
pub fn env_file_path(f: &str, mdir: &str) -> String {
    let f = expanduser(f);
    if f.starts_with('/') {
        f
    } else {
        format!("{}/{f}", mdir.trim_end_matches('/'))
    }
}

/// The KEY=VALUE lines of an env file, in order: `#` comments and empty lines are skipped, an
/// `export ` prefix is dropped, and one pair of matching quotes around a value is removed. A line
/// without `=` or with a key that is not a variable name is an error.
pub fn read_env_file(path: &str) -> Result<Vec<(String, String)>, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("env_file {path}: {e}"))?;
    let mut out = Vec::new();
    for (i, ln) in t.lines().enumerate() {
        let l = ln.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let l = l.strip_prefix("export ").unwrap_or(l).trim_start();
        let Some((k, v)) = l.split_once('=') else {
            return Err(format!("env_file {path}:{}: not KEY=VALUE", i + 1));
        };
        let k = k.trim();
        let ok = k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!(
                "env_file {path}:{}: {k:?} is not a variable name",
                i + 1
            ));
        }
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

fn image_path(v: &Value, arch: &str, mdir: &str, wh: &str) -> R<String> {
    let v = match v {
        Value::Object(o) => {
            let mut bad: Vec<String> = o
                .keys()
                .filter(|k| !ARCHES.contains(&k.as_str()))
                .cloned()
                .collect();
            bad.sort();
            if !bad.is_empty() {
                return Err(err(format!(
                    "{}: image: keys are architectures ({}), not {}",
                    wh,
                    ARCHES.join(", "),
                    bad.join(", ")
                )));
            }
            match o.get(arch) {
                Some(x) => x.clone(),
                None => return Err(err(format!("{wh}: image: has no entry for {arch}"))),
            }
        }
        other => other.clone(),
    };
    let s = match &v {
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => {
            return Err(err(format!(
                "{wh}: image: a path to an OCI layout directory or a docker save tar (or a mapping x86_64: / arm64: of such paths)"
            )))
        }
    };
    let mut p = expanduser(&s);
    if !p.starts_with('/') {
        p = if mdir.ends_with('/') {
            format!("{mdir}{p}")
        } else {
            format!("{mdir}/{p}")
        };
    }
    if !std::path::Path::new(&p).exists() {
        return Err(err(format!(
            "{wh}: image {p} does not exist. hydo-build does not pull images; produce the file with `docker save -o <file>.tar <image>` or `skopeo copy docker://<image>@sha256:<digest> oci:<dir>:<tag>` (MANIFEST.md)"
        )));
    }
    Ok(p)
}

/// `_oci_prepare(m, path, arch)` as far as it goes without reading an image: returns the
/// (service, image path) pairs whose image exists and was not read.
pub fn prepare(m: &Value, manifest_path: &str, arch: &str) -> R<Vec<(String, String)>> {
    let svcs = match m.get("services") {
        Some(s) if py::truthy(s) => iter_of(s)?,
        _ => Vec::new(),
    };
    if !svcs
        .iter()
        .any(|s| s.as_object().is_some_and(|o| o.contains_key("image")))
    {
        return Ok(Vec::new());
    }
    let mdir = py::dirname(&py::abspath(manifest_path));
    let mut out = Vec::new();
    for (i, s) in svcs.iter().enumerate() {
        let o = s.as_object().cloned().unwrap_or_default();
        let wh = format!(
            "services[{}] ({})",
            i,
            str_of(o.get("name").unwrap_or(&Value::Null))
        );
        let Some(img) = o.get("image") else {
            let extra: Vec<&str> = OCI_KEYS
                .iter()
                .copied()
                .filter(|k| o.contains_key(*k))
                .collect();
            if !extra.is_empty() {
                return Err(err(format!(
                    "{}: {} only with image:",
                    wh,
                    extra.join(", ")
                )));
            }
            continue;
        };
        let p = image_path(img, arch, &mdir, &wh)?;
        // Milestone 392: each version's own image file
        if let Some(Value::Array(vs)) = o.get("versions") {
            for (j, v) in vs.iter().enumerate() {
                if let Some(vi) = v.get("image") {
                    image_path(vi, arch, &mdir, &format!("{wh}.versions[{j}]"))?;
                }
            }
        }
        if let Some(f) = o.get("env_file").and_then(Value::as_str) {
            let fp = env_file_path(f, &mdir);
            if !std::path::Path::new(&fp).is_file() {
                return Err(err(format!(
                    "{wh}: env_file {f} does not exist ({fp}); a file that `hydo k8s` writes for Secrets stays out of version control: run the import again"
                )));
            }
        }
        out.push((str_of(o.get("name").unwrap_or(&Value::Null)), p));
        // hydo_oci reads the image here (its config), then checks this service's env:, workdir:,
        // user: and mounts: against it; not ported, so those checks of an image service are not
        // made (the image is assumed readable) and the loop goes on with the next service.
    }
    Ok(out)
}

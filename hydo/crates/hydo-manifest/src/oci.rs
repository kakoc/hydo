//! The manifest-side checks of `services[].image` (hydo/onboard/hydo_oci.py `prepare` and
//! `_image_path`) that need no image: OCI keys without `image:`, the shape of `image:` and whether
//! the image file exists. Reading an existing image (its config: entrypoint, env, user) is not
//! ported; such services are reported in `Loaded::oci_unchecked` and keep their image keys.

use serde_json::Value;

use crate::py::{self, str_of};
use crate::validate::{err, iter_of, R};

const OCI_KEYS: &[&str] = &["image", "image_ref", "entrypoint", "workdir", "user", "env", "mounts"];
const ARCHES: &[&str] = &["x86_64", "arm64"];

fn expanduser(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{}{}", h.trim_end_matches('/'), &p[1..]);
        }
    }
    p.to_string()
}

fn image_path(v: &Value, arch: &str, mdir: &str, wh: &str) -> R<String> {
    let v = match v {
        Value::Object(o) => {
            let mut bad: Vec<String> = o.keys().filter(|k| !ARCHES.contains(&k.as_str())).cloned().collect();
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
        p = if mdir.ends_with('/') { format!("{mdir}{p}") } else { format!("{mdir}/{p}") };
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
    if !svcs.iter().any(|s| s.as_object().is_some_and(|o| o.contains_key("image"))) {
        return Ok(Vec::new());
    }
    let mdir = py::dirname(&py::abspath(manifest_path));
    let mut out = Vec::new();
    for (i, s) in svcs.iter().enumerate() {
        let o = s.as_object().cloned().unwrap_or_default();
        let wh = format!("services[{}] ({})", i, str_of(o.get("name").unwrap_or(&Value::Null)));
        let Some(img) = o.get("image") else {
            let extra: Vec<&str> = OCI_KEYS.iter().copied().filter(|k| o.contains_key(*k)).collect();
            if !extra.is_empty() {
                return Err(err(format!("{}: {} only with image:", wh, extra.join(", "))));
            }
            continue;
        };
        let p = image_path(img, arch, &mdir, &wh)?;
        out.push((str_of(o.get("name").unwrap_or(&Value::Null)), p));
        // hydo_oci reads the image here (its config, then the env:, workdir:, user: and mounts:
        // checks of this service); the rest of the loop depends on that, so it stops here.
        break;
    }
    Ok(out)
}

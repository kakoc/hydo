//! The image config of an image file already fetched (hydo_compose.py's `config_from_file`, which
//! read it with hydo_oci.Image): a `docker save` archive (manifest.json) or an OCI layout
//! directory (index.json), for one architecture.

use crate::pyval::{from_json, Y};
use crate::registry::{sha256_hex, INDEX_TYPES};
use crate::ComposeError;
use std::io::Read;
use std::path::Path;

type R<T> = Result<T, ComposeError>;

/// A tar member: its normalised path, its bytes (small files only) and a symbolic link's target.
type Member = (String, Vec<u8>, Option<String>);

fn e<T>(m: String) -> R<T> {
    Err(ComposeError(m))
}

/// The files of a tar archive (plain or gzip), by normalised path; symbolic links followed.
fn tar_files(path: &str) -> R<Vec<Member>> {
    let raw = std::fs::read(path).map_err(|x| ComposeError::os(x, path))?;
    let data = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut d = Vec::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_end(&mut d)
            .map_err(|x| ComposeError(format!("{path}: not a readable tar archive ({x})")))?;
        d
    } else {
        raw
    };
    let mut a = tar::Archive::new(&data[..]);
    let mut out = Vec::new();
    let entries = a.entries().map_err(|x| {
        ComposeError(format!(
            "{path}: not an OCI layout directory or a readable tar archive ({x})"
        ))
    })?;
    for en in entries {
        let mut en = en.map_err(|x| ComposeError(format!("{path}: {x}")))?;
        let name = en
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = hydo_manifest::py::normpath(name.trim_start_matches('/'));
        let link = if en.header().entry_type().is_symlink() {
            en.link_name()
                .ok()
                .flatten()
                .map(|l| l.to_string_lossy().into_owned())
        } else {
            None
        };
        let mut b = Vec::new();
        if en.header().entry_type().is_file() {
            // only small files are needed (manifest.json and the config); the archives of
            // Docker 25 and later keep the config at blobs/sha256/<hex>, without .json (M214)
            if name == "manifest.json"
                || name.ends_with(".json")
                || !name.contains('/') && en.size() < (16 << 20)
                || name.starts_with("blobs/") && en.size() < (4 << 20)
            {
                en.read_to_end(&mut b)
                    .map_err(|x| ComposeError(format!("{path}: {x}")))?;
            }
        }
        out.push((name, b, link));
    }
    Ok(out)
}

struct Src {
    path: String,
    tar: Option<Vec<Member>>,
}

impl Src {
    fn read(&self, name: &str, digest: Option<&str>) -> R<Vec<u8>> {
        let b = match &self.tar {
            None => {
                let p = Path::new(&self.path).join(name);
                if !p.is_file() {
                    return e(format!("{}: {name} missing", self.path));
                }
                std::fs::read(&p).map_err(|x| ComposeError::os(x, &p.display().to_string()))?
            }
            Some(files) => {
                let mut n = hydo_manifest::py::normpath(name.trim_start_matches('/'));
                let mut found = None;
                for _ in 0..16 {
                    match files.iter().find(|(x, _, _)| *x == n) {
                        Some((_, _, Some(l))) => {
                            n = hydo_manifest::py::normpath(&format!(
                                "{}/{l}",
                                hydo_manifest::py::dirname(&n)
                            ));
                        }
                        Some((_, b, None)) => {
                            found = Some(b.clone());
                            break;
                        }
                        None => break,
                    }
                }
                found.ok_or_else(|| {
                    ComposeError(format!("{}: {name} missing from the archive", self.path))
                })?
            }
        };
        if let Some(d) = digest {
            let got = format!("sha256:{}", sha256_hex(&b));
            if got != d {
                return e(format!(
                    "{}: {name} does not match its digest {d} (got {got})",
                    self.path
                ));
            }
        }
        Ok(b)
    }

    fn exists(&self, name: &str) -> bool {
        match &self.tar {
            None => Path::new(&self.path).join(name).is_file(),
            Some(f) => f.iter().any(|(x, _, _)| x == name),
        }
    }
}

fn blob(d: &str) -> String {
    let (a, h) = d.split_once(':').unwrap_or((d, ""));
    format!("blobs/{a}/{h}")
}

fn json(b: &[u8], what: &str) -> R<serde_json::Value> {
    serde_json::from_slice(b).map_err(|x| ComposeError(format!("{what}: {x}")))
}

/// {digest, variant, variants: [], config: {Entrypoint, Cmd, Env, WorkingDir, User, ExposedPorts,
/// Healthcheck, Volumes}} of the image file `path` for `arch` (amd64 or arm64).
pub fn config_from_file(path: &str, arch: &str) -> R<Y> {
    let src = Src {
        path: path.to_string(),
        tar: if Path::new(path).is_dir() {
            None
        } else {
            Some(tar_files(path)?)
        },
    };
    let (mdig, cdig, cfg, nlayers);
    if src.tar.is_some() && src.exists("manifest.json") {
        let entries = json(&src.read("manifest.json", None)?, path)?;
        let list = entries.as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            return e(format!("{path}: manifest.json lists no images"));
        }
        if list.len() != 1 {
            return e(format!("{path}: more than one image"));
        }
        let en = &list[0];
        let cname = en["Config"].as_str().unwrap_or("").to_string();
        let hexd = hydo_manifest::py::basename(&cname).replace(".json", "");
        let is_hex = hexd.len() == 64
            && hexd
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        let cd = is_hex.then(|| format!("sha256:{hexd}"));
        let cb = src.read(&cname, cd.as_deref())?;
        cdig = cd.unwrap_or_else(|| format!("sha256:{}", sha256_hex(&cb)));
        cfg = json(&cb, &cname)?;
        mdig = None;
        nlayers = en["Layers"].as_array().map(|a| a.len()).unwrap_or(0);
    } else if src.exists("index.json") {
        let idx = json(&src.read("index.json", None)?, path)?;
        let descs = idx["manifests"].as_array().cloned().unwrap_or_default();
        if descs.len() != 1 {
            return e(format!(
                "{path}: {}; index.json has {} manifests",
                if descs.is_empty() {
                    "no manifest"
                } else {
                    "more than one image"
                },
                descs.len()
            ));
        }
        let mut d = descs[0].clone();
        let mut done = None;
        for _ in 0..4 {
            let dg = d["digest"].as_str().unwrap_or("").to_string();
            let doc = json(&src.read(&blob(&dg), Some(&dg))?, &dg)?;
            let mt = d["mediaType"]
                .as_str()
                .or(doc["mediaType"].as_str())
                .unwrap_or("")
                .to_string();
            if INDEX_TYPES.contains(&mt.as_str()) || doc.get("manifests").is_some() {
                let cand: Vec<serde_json::Value> = doc["manifests"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|x| {
                        x["platform"]["os"] == "linux" && x["platform"]["architecture"] == arch
                    })
                    .collect();
                let Some(c) = cand.first() else {
                    return e(format!(
                        "{path}: the image index has no linux/{arch} manifest"
                    ));
                };
                d = c.clone();
                continue;
            }
            let c = &doc["config"];
            let cd = c["digest"].as_str().unwrap_or("").to_string();
            let cb = src.read(&blob(&cd), Some(&cd))?;
            done = Some((
                dg,
                cd,
                json(&cb, path)?,
                doc["layers"].as_array().map(|a| a.len()).unwrap_or(0),
            ));
            break;
        }
        let Some((dg, cd, c, n)) = done else {
            return e(format!("{path}: image index nested too deeply"));
        };
        mdig = Some(dg);
        cdig = cd;
        cfg = c;
        nlayers = n;
    } else {
        return e(format!(
            "{path}: neither a docker save archive (manifest.json) nor an OCI layout (index.json)"
        ));
    }
    let os = cfg["os"].as_str().unwrap_or("linux");
    let got = cfg["architecture"].as_str().unwrap_or("");
    if os != "linux" || got != arch {
        let want = if arch == "amd64" { "x86_64" } else { "arm64" };
        return e(format!(
            "{path}: the image is {}/{got}, but this build is for {want} ({arch}); use an image of that architecture (e.g. skopeo copy --override-arch {arch} ...)",
            cfg["os"].as_str().unwrap_or("None")
        ));
    }
    let diff = cfg["rootfs"]["diff_ids"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    if diff != nlayers {
        return e(format!(
            "{path}: {nlayers} layers but {diff} rootfs.diff_ids in the config"
        ));
    }
    let c = &cfg["config"];
    let mut conf = Y::Map(Vec::new());
    for k in [
        "Entrypoint",
        "Cmd",
        "Env",
        "WorkingDir",
        "User",
        "ExposedPorts",
        "Healthcheck",
        "Volumes",
    ] {
        conf.set(Y::s(k), c.get(k).map(from_json).unwrap_or(Y::Null));
    }
    let variant = cfg["variant"].as_str().unwrap_or("").to_string();
    Ok(Y::Map(vec![
        (Y::s("digest"), Y::Str(mdig.unwrap_or(cdig))),
        (Y::s("variant"), Y::Str(variant)),
        (Y::s("variants"), Y::Seq(Vec::new())),
        (Y::s("config"), conf),
    ]))
}

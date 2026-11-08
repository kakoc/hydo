//! Services whose files come from a Docker/OCI image (`services[].image`): port of
//! hydo/onboard/hydo_oci.py. No network access: the image is a file on disk, an OCI image layout
//! directory or a tar archive (`docker save`, skopeo's docker-archive:/oci-archive:, optionally
//! gzip-compressed). Each image service gets its own root /oci/<service> in the guest and runs
//! chrooted there; the layers are applied in order with whiteouts; owners and modes come from the
//! layer tars and go into the ext4 root with debugfs (x86_64) or into the cpio headers (arm64);
//! every blob is checked against its digest and every layer against the config's diff_ids.
//! The launchers, `hydo-oci` and image.json are written byte for byte as hydo_oci.py wrote them.

use crate::cpio::{Meta, S_IFDIR, S_IFLNK, S_IFREG};
use crate::fsx::{self, io_fail, is_link, lexists};
use crate::shell::shq;
use crate::tarball::{self, Member};
use crate::{errln, outln, pyjson, Fail, Io};
use hydo_manifest::py;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const OCI_KEYS: &[&str] = &[
    "image",
    "image_ref",
    "entrypoint",
    "workdir",
    "user",
    "env",
    "mounts",
    "writable",
];
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const BINDS: &[&str] = &["proc", "sys", "dev", "tmp", "data"];
const PASS_ENV: &[&str] = &[
    "HYDO_FROM",
    "HYDO_TO",
    "HYDO_STEPS",
    "HYDO_ACKED",
    "HYDO_OUTCOMES",
    "HYDO_FAILED",
    "HYDO_LOG",
    "HYDO_SVC",
    "HYDO_VALUE",
];
const HOSTS: &str = "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n";
const INDEX_TYPES: &[&str] = &[
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// An OciError: its text becomes the manifest refusal.
type R<T> = Result<T, String>;

fn arch_name(arch: &str) -> &'static str {
    if arch == "arm64" {
        "arm64"
    } else {
        "amd64"
    }
}

fn sha_hex(b: &[u8]) -> String {
    fsx::hex(&Sha256::digest(b))
}

// ------------------------------------------------------------------ reading an image file
enum Source {
    Dir(PathBuf),
    Tar {
        file: PathBuf,
        _tmp: Option<TempFile>,
        members: HashMap<String, Member>,
        all: Vec<Member>,
    },
}

/// A decompressed archive, removed when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn open_tar(path: &Path) -> R<(PathBuf, Option<TempFile>, Vec<Member>)> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut head = [0u8; 6];
    let n = f.read(&mut head).map_err(|e| e.to_string())?;
    let (file, tmp) = if n >= 2 && head[..2] == [0x1f, 0x8b] {
        let t = std::env::temp_dir().join(format!(
            "hydo-oci-archive-{}-{}.tar",
            std::process::id(),
            crate::unique()
        ));
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut d = flate2::read::MultiGzDecoder::new(std::io::BufReader::new(f));
        let mut o = File::create(&t).map_err(|e| e.to_string())?;
        let tf = TempFile(t.clone());
        std::io::copy(&mut d, &mut o).map_err(|e| format!("gzip: {e}"))?;
        (t, Some(tf))
    } else if n >= 3 && &head[..3] == b"BZh" {
        return Err(
            "bzip2-compressed archives are not read; use a plain or gzip-compressed tar".into(),
        );
    } else if n >= 6 && head == [0xfd, b'7', b'z', b'X', b'Z', 0] {
        return Err(
            "xz-compressed archives are not read; use a plain or gzip-compressed tar".into(),
        );
    } else {
        (path.to_path_buf(), None)
    };
    let mut fh = File::open(&file).map_err(|e| e.to_string())?;
    let mems = tarball::members(&mut fh)?;
    Ok((file, tmp, mems))
}

impl Source {
    fn new(path: &str) -> R<Source> {
        let p = Path::new(path);
        if p.is_dir() {
            return Ok(Source::Dir(p.to_path_buf()));
        }
        let (file, tmp, all) = open_tar(p).map_err(|e| {
            format!("{path}: not an OCI layout directory or a readable tar archive ({e})")
        })?;
        let mut members = HashMap::new();
        for m in &all {
            members.insert(py::normpath(m.name.trim_start_matches('/')), m.clone());
        }
        Ok(Source::Tar {
            file,
            _tmp: tmp,
            members,
            all,
        })
    }

    fn is_tar(&self) -> bool {
        matches!(self, Source::Tar { .. })
    }

    fn member(&self, name: &str) -> Option<Member> {
        let Source::Tar { members, .. } = self else {
            return None;
        };
        let mut name = py::normpath(name.trim_start_matches('/'));
        for _ in 0..16 {
            let mi = members.get(&name)?;
            if mi.issym() {
                name = py::normpath(&join_rel(&py::dirname(&name), &mi.linkname));
                continue;
            }
            return Some(mi.clone());
        }
        None
    }

    fn exists(&self, name: &str) -> bool {
        match self {
            Source::Dir(d) => d.join(name).is_file(),
            Source::Tar { .. } => self.member(name).is_some(),
        }
    }

    /// The bytes of a member (a hard link reads its target's).
    fn open(&self, src_path: &str, name: &str) -> R<Box<dyn Read>> {
        match self {
            Source::Dir(d) => {
                let p = d.join(name);
                if !p.is_file() {
                    return Err(format!("{src_path}: {name} missing"));
                }
                Ok(Box::new(File::open(&p).map_err(|e| e.to_string())?))
            }
            Source::Tar { file, all, .. } => {
                let mut mi = self
                    .member(name)
                    .ok_or_else(|| format!("{src_path}: {name} missing from the archive"))?;
                if mi.islnk() {
                    let target = py::normpath(&mi.linkname);
                    mi = all
                        .iter()
                        .rev()
                        .find(|m| py::normpath(&m.name) == target && !m.islnk())
                        .cloned()
                        .ok_or_else(|| format!("{src_path}: {name}: link target missing"))?;
                }
                if !mi.has_data() {
                    return Err(format!("{src_path}: {name} is not a file in the archive"));
                }
                let mut f = File::open(file).map_err(|e| e.to_string())?;
                f.seek(SeekFrom::Start(mi.offset_data))
                    .map_err(|e| e.to_string())?;
                Ok(Box::new(f.take(mi.size)))
            }
        }
    }

    fn read(&self, src_path: &str, name: &str, digest: Option<&str>) -> R<Vec<u8>> {
        let mut b = Vec::new();
        self.open(src_path, name)?
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        if let Some(d) = digest {
            check_digest(&b, d, name)?;
        }
        Ok(b)
    }
}

fn join_rel(dir: &str, t: &str) -> String {
    if t.starts_with('/') || dir.is_empty() {
        t.to_string()
    } else {
        format!("{dir}/{t}")
    }
}

fn check_digest(b: &[u8], digest: &str, what: &str) -> R<()> {
    let (algo, hexd) = digest.split_once(':').unwrap_or((digest, ""));
    if algo != "sha256" {
        return Err(format!("{what}: digest {digest}: only sha256 is supported"));
    }
    let got = sha_hex(b);
    if got != hexd {
        return Err(format!(
            "{what}: content sha256:{got} does not match its digest {digest}"
        ));
    }
    Ok(())
}

fn blob(digest: &str) -> String {
    let (algo, hexd) = digest.split_once(':').unwrap_or((digest, ""));
    format!("blobs/{algo}/{hexd}")
}

fn norm_ref(r: &str) -> String {
    let r = r.strip_prefix("docker.io/").unwrap_or(r);
    r.strip_prefix("library/").unwrap_or(r).to_string()
}

/// `_ref_matches(ref, names)`.
fn ref_matches(r: &str, names: &[Option<String>]) -> bool {
    for n in names.iter().flatten() {
        if n.is_empty() {
            continue;
        }
        let tail = r.rsplit(':').next().unwrap_or(r);
        if n == r || norm_ref(n) == norm_ref(r) || (!n.contains(':') && tail == n) {
            return true;
        }
    }
    false
}

fn loads(b: &[u8], what: &str) -> R<Value> {
    serde_json::from_slice(b).map_err(|e| format!("{what}: not valid JSON ({e})"))
}

fn sget<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.as_object()
        .and_then(|o| o.get(k))
        .filter(|x| !x.is_null())
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(x) => Some(py::str_of(x)),
    }
}

/// The selected image: config, layers, digests.
pub struct Image {
    pub path: String,
    src: Source,
    pub manifest_digest: Option<String>,
    pub config_digest: String,
    pub config: Value,
    /// (blob name, compressed digest or None for docker save archives)
    pub layers: Vec<(String, Option<String>)>,
    pub ref_names: Vec<String>,
    pub diff_ids: Vec<Value>,
}

impl Image {
    pub fn new(path: &str, rf: Option<&str>, arch: &str) -> R<Image> {
        let src = Source::new(path)?;
        let mut img = Image {
            path: path.to_string(),
            src,
            manifest_digest: None,
            config_digest: String::new(),
            config: Value::Null,
            layers: Vec::new(),
            ref_names: Vec::new(),
            diff_ids: Vec::new(),
        };
        if img.src.is_tar() && img.src.exists("manifest.json") {
            img.docker(rf)?;
        } else if img.src.exists("index.json") {
            img.oci(rf, arch)?;
        } else {
            return Err(format!(
                "{path}: neither a docker save archive (manifest.json) nor an OCI layout (index.json)"
            ));
        }
        let cfg = &img.config;
        if !cfg.is_object() {
            return Err(format!("{path}: the image config is not a JSON object"));
        }
        let want = arch_name(arch);
        let got = cfg.get("architecture").cloned().unwrap_or(Value::Null);
        let os_ = cfg
            .get("os")
            .cloned()
            .unwrap_or(Value::String("linux".into()));
        if os_.as_str() != Some("linux") || got.as_str() != Some(want) {
            let os_shown = cfg.get("os").cloned().unwrap_or(Value::Null);
            return Err(format!(
                "{path}: the image is {}/{}, but this build is for {arch} ({want}); use an image of that architecture (e.g. skopeo copy --override-arch {want} ...)",
                py::str_of(&os_shown),
                py::str_of(&got)
            ));
        }
        img.diff_ids = match sget(cfg, "rootfs").and_then(|r| sget(r, "diff_ids")) {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        if img.diff_ids.len() != img.layers.len() {
            return Err(format!(
                "{path}: {} layers but {} rootfs.diff_ids in the config",
                img.layers.len(),
                img.diff_ids.len()
            ));
        }
        Ok(img)
    }

    fn docker(&mut self, rf: Option<&str>) -> R<()> {
        let p = self.path.clone();
        let entries = loads(&self.src.read(&p, "manifest.json", None)?, "manifest.json")?;
        let entries = match entries {
            Value::Array(a) if !a.is_empty() => a,
            _ => return Err(format!("{p}: manifest.json lists no images")),
        };
        let cfg_hex = |e: &Value| -> String {
            py::basename(&py::str_of(e.get("Config").unwrap_or(&Value::Null))).replace(".json", "")
        };
        let tags = |e: &Value| -> Vec<String> {
            match sget(e, "RepoTags") {
                Some(Value::Array(a)) if !a.is_empty() => a.iter().map(py::str_of).collect(),
                _ => Vec::new(),
            }
        };
        let pick: Vec<&Value> = match rf {
            Some(r) if r.starts_with("sha256:") => {
                entries.iter().filter(|e| cfg_hex(e) == r[7..]).collect()
            }
            Some(r) if !r.is_empty() => entries
                .iter()
                .filter(|e| ref_matches(r, &tags(e).into_iter().map(Some).collect::<Vec<_>>()))
                .collect(),
            _ => entries.iter().collect(),
        };
        if pick.len() != 1 {
            let what = if pick.is_empty() {
                format!(
                    "no image matches image_ref {}",
                    py::repr_str(rf.unwrap_or(""))
                )
            } else {
                "more than one image".to_string()
            };
            let has: Vec<String> = entries
                .iter()
                .map(|e| {
                    let t = tags(e);
                    if t.is_empty() {
                        "<untagged>".to_string()
                    } else {
                        t.join(",")
                    }
                })
                .collect();
            return Err(format!(
                "{p}: {what}; the archive has: {} (set image_ref: to a tag or the image id)",
                has.join(", ")
            ));
        }
        let e = pick[0];
        let cname = e
            .get("Config")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{p}: manifest.json: an entry without Config"))?
            .to_string();
        let hexd = cfg_hex(e);
        let cdig = if hexd.len() == 64
            && hexd
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            Some(format!("sha256:{hexd}"))
        } else {
            None
        };
        let cb = self.src.read(&p, &cname, cdig.as_deref())?;
        self.config_digest = cdig.unwrap_or_else(|| format!("sha256:{}", sha_hex(&cb)));
        self.config = loads(&cb, &cname)?;
        self.layers = match e.get("Layers") {
            Some(Value::Array(a)) => a.iter().map(|x| (py::str_of(x), None)).collect(),
            _ => return Err(format!("{p}: manifest.json: an entry without Layers")),
        };
        self.ref_names = tags(e);
        Ok(())
    }

    fn oci(&mut self, rf: Option<&str>, arch: &str) -> R<()> {
        let p = self.path.clone();
        let idx = loads(&self.src.read(&p, "index.json", None)?, "index.json")?;
        let descs: Vec<Value> = match sget(&idx, "manifests") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let ann = |d: &Value, k: &str| -> Option<String> {
            sget(d, "annotations").and_then(|a| opt_str(sget(a, k)))
        };
        let pick: Vec<&Value> = match rf {
            Some(r) if r.starts_with("sha256:") => descs
                .iter()
                .filter(|d| d.get("digest").and_then(Value::as_str) == Some(r))
                .collect(),
            Some(r) if !r.is_empty() => descs
                .iter()
                .filter(|d| {
                    ref_matches(
                        r,
                        &[
                            ann(d, "org.opencontainers.image.ref.name"),
                            ann(d, "io.containerd.image.name"),
                        ],
                    )
                })
                .collect(),
            _ => descs.iter().collect(),
        };
        if pick.len() != 1 {
            let names: Vec<String> = descs
                .iter()
                .map(|d| {
                    ann(d, "org.opencontainers.image.ref.name")
                        .filter(|x| !x.is_empty())
                        .unwrap_or_else(|| py::str_of(d.get("digest").unwrap_or(&Value::Null)))
                })
                .collect();
            let what = if pick.is_empty() {
                format!(
                    "no manifest matches image_ref {}",
                    py::repr_str(rf.unwrap_or(""))
                )
            } else {
                "more than one image".to_string()
            };
            return Err(format!(
                "{p}: {what}; index.json has: {} (set image_ref: to a tag or a digest)",
                names.join(", ")
            ));
        }
        let mut d = pick[0].clone();
        self.ref_names = ann(&d, "org.opencontainers.image.ref.name")
            .filter(|x| !x.is_empty())
            .into_iter()
            .collect();
        for _ in 0..4 {
            let dig = d
                .get("digest")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{p}: a descriptor without a digest"))?
                .to_string();
            let body = self.src.read(&p, &blob(&dig), Some(&dig))?;
            let doc = loads(&body, &blob(&dig))?;
            let mt = opt_str(sget(&d, "mediaType").filter(|x| py::truthy(x)))
                .or_else(|| opt_str(sget(&doc, "mediaType")));
            let is_index = mt.as_deref().is_some_and(|m| INDEX_TYPES.contains(&m))
                || doc.as_object().is_some_and(|o| o.contains_key("manifests"));
            if is_index {
                let want = arch_name(arch);
                let ms: Vec<Value> = match sget(&doc, "manifests") {
                    Some(Value::Array(a)) => a.clone(),
                    _ => Vec::new(),
                };
                let plat = |x: &Value, k: &str| -> Value {
                    sget(x, "platform")
                        .and_then(|pl| pl.get(k).cloned())
                        .unwrap_or(Value::Null)
                };
                let cand: Vec<&Value> = ms
                    .iter()
                    .filter(|x| {
                        plat(x, "os").as_str() == Some("linux")
                            && plat(x, "architecture").as_str() == Some(want)
                    })
                    .collect();
                if cand.is_empty() {
                    let mut plats: Vec<String> = ms
                        .iter()
                        .map(|x| {
                            format!(
                                "{}/{}",
                                py::str_of(&plat(x, "os")),
                                py::str_of(&plat(x, "architecture"))
                            )
                        })
                        .collect();
                    plats.sort();
                    plats.dedup();
                    return Err(format!(
                        "{p}: the image index has no linux/{want} manifest (has {})",
                        plats.join(", ")
                    ));
                }
                d = cand[0].clone();
                continue;
            }
            self.manifest_digest = Some(dig);
            let c = doc
                .get("config")
                .ok_or_else(|| format!("{p}: a manifest without a config"))?;
            let cdig = c
                .get("digest")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{p}: a config without a digest"))?
                .to_string();
            let cb = self.src.read(&p, &blob(&cdig), Some(&cdig))?;
            self.config_digest = cdig;
            self.config = loads(&cb, "config")?;
            self.layers = match sget(&doc, "layers") {
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|l| {
                        let dg = py::str_of(l.get("digest").unwrap_or(&Value::Null));
                        (blob(&dg), Some(dg))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            return Ok(());
        }
        Err(format!("{p}: image index nested too deeply"))
    }

    fn config_section(&self) -> Value {
        sget(&self.config, "config")
            .filter(|c| py::truthy(c))
            .cloned()
            .unwrap_or(Value::Object(Map::new()))
    }
}

/// A reader that hashes what passes through it.
struct HashReader<Rd: Read> {
    inner: Rd,
    h: Sha256,
}

impl<Rd: Read> Read for HashReader<Rd> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.h.update(&buf[..n]);
        Ok(n)
    }
}

/// `_layer_to_file`: the layer, uncompressed, in `out`; checks the blob digest and the diff_id.
fn layer_to_file(
    img: &Image,
    name: &str,
    digest: Option<&str>,
    diff_id: Option<&str>,
    out: &Path,
) -> R<()> {
    let raw = img.src.open(&img.path, name)?;
    let mut hr = HashReader {
        inner: raw,
        h: Sha256::new(),
    };
    let mut head = Vec::new();
    (&mut hr)
        .take(4)
        .read_to_end(&mut head)
        .map_err(|e| e.to_string())?;
    if head == [0x28, 0xb5, 0x2f, 0xfd] {
        return Err(format!(
            "{}: layer {name} is zstd-compressed; this build cannot decompress zstd. Re-export it gzip-compressed (skopeo copy --dest-compress-format gzip ...)",
            img.path
        ));
    }
    let uh = {
        let mut fo = File::create(out).map_err(|e| e.to_string())?;
        let mut uh = Sha256::new();
        let mut copy = |r: &mut dyn Read| -> R<()> {
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = r
                    .read(&mut buf)
                    .map_err(|e| format!("{}: layer {name}: {e}", img.path))?;
                if n == 0 {
                    return Ok(());
                }
                uh.update(&buf[..n]);
                fo.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            }
        };
        let cat = std::io::Cursor::new(head.clone()).chain(&mut hr);
        if head.len() >= 2 && head[..2] == [0x1f, 0x8b] {
            let mut d = flate2::read::MultiGzDecoder::new(cat);
            copy(&mut d)?;
        } else {
            let mut c = cat;
            copy(&mut c)?;
        }
        uh
    };
    std::io::copy(&mut hr, &mut std::io::sink()).map_err(|e| e.to_string())?;
    if let Some(d) = digest.filter(|d| !d.is_empty()) {
        if format!("sha256:{}", fsx::hex(&hr.h.finalize())) != d {
            return Err(format!(
                "{}: layer {name}: content does not match its digest {d}",
                img.path
            ));
        }
    }
    if let Some(d) = diff_id.filter(|d| !d.is_empty()) {
        if format!("sha256:{}", fsx::hex(&uh.finalize())) != d {
            return Err(format!(
                "{}: layer {name}: uncompressed content does not match rootfs.diff_id {d}",
                img.path
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ flattening the layers
/// `(S_IFMT, mode, uid, gid)` of an image file as the layer tar gives it.
pub type MetaEntry = (u32, i64, i64, i64);

/// An image root being assembled on the build host (`_Tree`).
struct Tree {
    root: PathBuf,
    meta: HashMap<String, MetaEntry>,
    skipped: u64,
}

fn rjoin(parts: &[String]) -> String {
    parts.join("/")
}

impl Tree {
    fn new(root: PathBuf) -> R<Tree> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let mut meta = HashMap::new();
        meta.insert(String::new(), (S_IFDIR, 0o755, 0, 0));
        Ok(Tree {
            root,
            meta,
            skipped: 0,
        })
    }

    fn abs(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    fn parts(name: &str) -> R<Vec<String>> {
        let p: Vec<String> = name
            .split('/')
            .filter(|x| !x.is_empty() && *x != ".")
            .map(str::to_string)
            .collect();
        if p.iter().any(|x| x == "..") {
            return Err(format!("unsafe path in a layer: {}", py::repr_str(name)));
        }
        if p.iter()
            .any(|x| x.contains('"') || x.contains('\n') || x.contains('\\'))
        {
            return Err(format!(
                "unsupported character in a layer path: {}",
                py::repr_str(name)
            ));
        }
        Ok(p)
    }

    fn resolve_dir(&mut self, comps: &[String], create: bool) -> R<Option<Vec<String>>> {
        let mut out: Vec<String> = Vec::new();
        let mut todo: std::collections::VecDeque<String> = comps.iter().cloned().collect();
        let mut hops = 0;
        while let Some(c) = todo.pop_front() {
            if c.is_empty() || c == "." {
                continue;
            }
            if c == ".." {
                out.pop();
                continue;
            }
            let mut cand = out.clone();
            cand.push(c.clone());
            let p = self.abs(&rjoin(&cand));
            if is_link(&p) {
                hops += 1;
                if hops > 40 {
                    return Err(format!("symlink loop under {}", rjoin(&cand)));
                }
                let t = fs::read_link(&p).map_err(|e| e.to_string())?;
                let t = t.to_string_lossy().into_owned();
                if t.starts_with('/') {
                    out.clear();
                }
                for x in t.split('/').rev() {
                    todo.push_front(x.to_string());
                }
                continue;
            }
            if !lexists(&p) {
                if !create {
                    return Ok(None);
                }
                mkdir755(&p)?;
                self.meta.insert(rjoin(&cand), (S_IFDIR, 0o755, 0, 0));
            } else if !p.is_dir() {
                return Err(format!(
                    "a layer puts a file below {}, which is not a directory",
                    rjoin(&cand)
                ));
            }
            out.push(c);
        }
        Ok(Some(out))
    }

    fn remove(&mut self, rel: &str) -> R<()> {
        let p = self.abs(rel);
        if p.is_dir() && !is_link(&p) {
            fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
        } else if lexists(&p) {
            fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
        let pre = format!("{rel}/");
        self.meta.retain(|k, _| !(k == rel || k.starts_with(&pre)));
        Ok(())
    }

    fn whiteout(&mut self, name: &str) -> R<()> {
        let p = Tree::parts(name)?;
        let Some(base) = p.last().cloned() else {
            return Ok(());
        };
        let Some(d) = self.resolve_dir(&p[..p.len() - 1], false)? else {
            return Ok(());
        };
        if base == ".wh..wh..opq" {
            let dp = self.abs(&rjoin(&d));
            let mut names: Vec<String> = fs::read_dir(&dp)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for n in names {
                let mut r = d.clone();
                r.push(n);
                self.remove(&rjoin(&r))?;
            }
        } else if let Some(t) = base.strip_prefix(".wh.") {
            let mut r = d.clone();
            r.push(t.to_string());
            self.remove(&rjoin(&r))?;
        }
        Ok(())
    }

    fn add(&mut self, tarf: &mut File, mi: &Member) -> R<()> {
        let p = Tree::parts(&mi.name)?;
        if p.is_empty() {
            self.meta
                .insert(String::new(), (S_IFDIR, mi.mode & 0o7777, mi.uid, mi.gid));
            return Ok(());
        }
        let d = self
            .resolve_dir(&p[..p.len() - 1], true)?
            .unwrap_or_default();
        let mut rv = d.clone();
        rv.push(p[p.len() - 1].clone());
        let rel = rjoin(&rv);
        let dst = self.abs(&rel);
        if mi.isdir() {
            if !dst.is_dir() || is_link(&dst) {
                if lexists(&dst) {
                    self.remove(&rel)?;
                }
                mkdir755(&dst)?;
            }
            self.meta
                .insert(rel, (S_IFDIR, mi.mode & 0o7777, mi.uid, mi.gid));
            return Ok(());
        }
        if !(mi.isreg() || mi.issym() || mi.islnk()) {
            self.skipped += 1;
            return Ok(());
        }
        if lexists(&dst) {
            self.remove(&rel)?;
        }
        if mi.issym() {
            std::os::unix::fs::symlink(&mi.linkname, &dst).map_err(|e| e.to_string())?;
            self.meta.insert(rel, (S_IFLNK, 0o777, mi.uid, mi.gid));
        } else if mi.islnk() {
            let tp = Tree::parts(&mi.linkname)?;
            let (td, trel) = if tp.is_empty() {
                (Some(Vec::new()), String::new())
            } else {
                let td = self.resolve_dir(&tp[..tp.len() - 1], false)?;
                let mut t = td.clone().unwrap_or_default();
                t.push(tp[tp.len() - 1].clone());
                (td, rjoin(&t))
            };
            let ta = self.abs(&trel);
            if td.is_none() || !lexists(&ta) || (ta.is_dir() && !is_link(&ta)) {
                return Err(format!(
                    "hard link {} -> {}: target missing",
                    mi.name, mi.linkname
                ));
            }
            fs::hard_link(&ta, &dst).map_err(|e| e.to_string())?;
            let m = self.meta.get(&trel).copied().unwrap_or((
                S_IFREG,
                mi.mode & 0o7777,
                mi.uid,
                mi.gid,
            ));
            self.meta.insert(rel, m);
        } else {
            tarf.seek(SeekFrom::Start(mi.offset_data))
                .map_err(|e| e.to_string())?;
            let fo = File::create(&dst).map_err(|e| e.to_string())?;
            let mut w = std::io::BufWriter::with_capacity(1 << 20, fo);
            let mut r = std::io::BufReader::with_capacity(1 << 20, (&mut *tarf).take(mi.size));
            std::io::copy(&mut r, &mut w).map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            drop(w);
            let mode = 0o644 | if mi.mode & 0o111 != 0 { 0o111 } else { 0 };
            fsx::chmod(&dst, mode).map_err(|e| e.message())?;
            self.meta
                .insert(rel, (S_IFREG, mi.mode & 0o7777, mi.uid, mi.gid));
        }
        Ok(())
    }

    fn apply_layer(&mut self, path: &Path) -> R<()> {
        let mut f = File::open(path).map_err(|e| e.to_string())?;
        let mems = tarball::members(&mut f)?;
        let base = |n: &str| -> String { py::basename(n.trim_end_matches('/')) };
        for mi in &mems {
            if base(&mi.name).starts_with(".wh.") {
                self.whiteout(mi.name.trim_end_matches('/'))?;
            }
        }
        for mi in &mems {
            if !base(&mi.name).starts_with(".wh.") {
                self.add(&mut f, mi)?;
            }
        }
        Ok(())
    }

    fn put_file(&mut self, rel: &str, content: &[u8], mode: i64) -> R<()> {
        let comps: Vec<String> = rel.split('/').map(str::to_string).collect();
        let d = self
            .resolve_dir(&comps[..comps.len() - 1], true)?
            .unwrap_or_default();
        let mut rv = d;
        rv.push(comps[comps.len() - 1].clone());
        let r = rjoin(&rv);
        if lexists(&self.abs(&r)) {
            self.remove(&r)?;
        }
        fs::write(self.abs(&r), content).map_err(|e| e.to_string())?;
        let m = 0o644 | if mode & 0o111 != 0 { 0o111 } else { 0 };
        fsx::chmod(&self.abs(&r), m).map_err(|e| e.message())?;
        self.meta.insert(r, (S_IFREG, mode, 0, 0));
        Ok(())
    }

    fn ensure_dir(&mut self, rel: &str, mode: i64) -> R<()> {
        let p = self.abs(rel);
        if is_link(&p) || (lexists(&p) && !p.is_dir()) {
            return Err(format!(
                "the image has /{rel}, but not as a directory; hydo bind-mounts the guest's /{rel} there"
            ));
        }
        if !p.is_dir() {
            mkdir755(&p)?;
            self.meta.insert(rel.to_string(), (S_IFDIR, mode, 0, 0));
        }
        Ok(())
    }

    /// The home directory of user (name or uid) from the image's /etc/passwd ("/" if unknown).
    fn home_of(&self, user: &str) -> String {
        if user.is_empty() {
            return "/root".into();
        }
        let name = user.split(':').next().unwrap_or("");
        if let Ok(b) = fs::read(self.abs("etc/passwd")) {
            let t = String::from_utf8_lossy(&b);
            for line in crate::splitlines(&t) {
                let x: Vec<&str> = line.split(':').collect();
                if x.len() >= 6 && (x[0] == name || x[2] == name) {
                    return if x[5].is_empty() {
                        "/".into()
                    } else {
                        x[5].to_string()
                    };
                }
            }
        }
        "/".into()
    }
}

fn mkdir755(p: &Path) -> R<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o755)
        .create(p)
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ manifest side
/// One image service, as `prepare` leaves it for `stage`.
pub struct OciService {
    pub svc: String,
    pub path: String,
    pub img: Image,
    pub final_cmd: String,
    pub env: Vec<(String, String)>,
    pub workdir: String,
    pub user: String,
    pub mounts: Vec<Mount>,
    /// `writable: true`: the service's root is a writable copy of the image (its layer) on the
    /// data disk, bound over /oci/<service> at the first start of every boot.
    pub writable: bool,
}

/// Where the writable layer of an image service lives on the data disk (the guest path; on the
/// data partition `.hydo/layers/<service>`), and the file that says the copy is whole.
pub fn layer_dir(svc: &str) -> String {
    format!("/data/.hydo/layers/{svc}")
}

/// What `tls:` adds to every image service's root (src/tls.rs): the CA and the host names.
pub struct TlsTrust {
    pub ca_pem: String,
    pub host_lines: String,
}

#[derive(Clone)]
pub struct Mount {
    pub dst: String,
    pub src: Option<String>,
    pub tmpfs: bool,
    pub copy_up: bool,
    pub size: Option<String>,
}

fn words(argv: &[Value]) -> String {
    argv.iter()
        .map(|a| shq(&py::str_of(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `_words(cfg.get(k) or [])` of an image config field (a string iterates as its characters).
fn words_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(a)) => words(a),
        Some(Value::String(s)) => s
            .chars()
            .map(|c| shq(&c.to_string()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

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
            let mut bad: Vec<String> = o
                .keys()
                .filter(|k| *k != "x86_64" && *k != "arm64")
                .cloned()
                .collect();
            bad.sort();
            if !bad.is_empty() {
                return Err(format!(
                    "{wh}: image: keys are architectures (x86_64, arm64), not {}",
                    bad.join(", ")
                ));
            }
            o.get(arch)
                .cloned()
                .ok_or_else(|| format!("{wh}: image: has no entry for {arch}"))?
        }
        other => other.clone(),
    };
    let s = match &v {
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => {
            return Err(format!(
                "{wh}: image: a path to an OCI layout directory or a docker save tar (or a mapping x86_64: / arm64: of such paths)"
            ))
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
    if !Path::new(&p).exists() {
        return Err(format!(
            "{wh}: image {p} does not exist. hydo-build does not pull images; produce the file with `docker save -o <file>.tar <image>` or `skopeo copy docker://<image>@sha256:<digest> oci:<dir>:<tag>` (MANIFEST.md)"
        ));
    }
    Ok(p)
}

fn mount_path_ok(p: &str) -> bool {
    let p = p.strip_suffix('\n').unwrap_or(p);
    p.starts_with('/')
        && p[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._@+=,:/-".contains(c))
}

/// `_mounts(v, where)`.
fn mounts_of(v: Option<Value>, wh: &str) -> Result<Vec<Mount>, Fail> {
    let m = |s: String| Fail::Manifest(s);
    let v = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(m(format!("{wh}: mounts: must be a list"))),
    };
    let mut out = Vec::new();
    for (i, e) in v.iter().enumerate() {
        let w = format!("{wh}: mounts[{i}]");
        let Some(o) = e.as_object() else {
            return Err(m(format!(
                "{w}: must be a mapping {{src|tmpfs, dst, copy_up, size}}"
            )));
        };
        let mut bad: Vec<&String> = o
            .keys()
            .filter(|k| !["src", "dst", "tmpfs", "copy_up", "size"].contains(&k.as_str()))
            .collect();
        bad.sort();
        if !bad.is_empty() {
            return Err(m(format!(
                "{w}: unknown field(s) {} (known: copy_up, dst, size, src, tmpfs)",
                bad.iter()
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let dst = o.get("dst").cloned().unwrap_or(Value::Null);
        // A directory of the data disk may stand on /data (a Compose volume on /data, one per
        // service: the nodes of a cluster each with their own store); nothing else may.
        let data_dir_src = matches!(o.get("src"), Some(Value::String(s))
            if py::normpath(s).starts_with("/data/") && !format!("{s}/").contains("/../"));
        let dst_ok = match &dst {
            Value::String(d) => {
                let nd = py::normpath(d);
                mount_path_ok(d)
                    && !format!("{d}/").contains("/../")
                    && !["/", "/proc", "/sys", "/dev", "/tmp", "/data", "/.hydo"]
                        .contains(&nd.as_str())
                    || nd == "/data" && mount_path_ok(d) && data_dir_src
            }
            _ => false,
        };
        if !dst_ok {
            return Err(m(format!(
                "{w}: dst {}: an absolute path other than / /proc /sys /dev /tmp /data (letters, digits and ._@+=,:/- only)",
                py::repr(&dst)
            )));
        }
        let dst = dst.as_str().unwrap_or_default().to_string();
        let tm = o.get("tmpfs").is_some_and(py::truthy);
        let src = o.get("src").filter(|x| !x.is_null()).cloned();
        if tm == src.is_some() {
            return Err(m(format!(
                "{w}: give exactly one of src: (a guest path) and tmpfs: true"
            )));
        }
        if let Some(sv) = &src {
            let ok = matches!(sv, Value::String(x) if mount_path_ok(x) && !format!("{x}/").contains("/../"));
            if !ok {
                return Err(m(format!(
                    "{w}: src {}: an absolute guest path (letters, digits and ._@+=,:/- only)",
                    py::repr(sv)
                )));
            }
        }
        let size = o.get("size").filter(|x| !x.is_null()).cloned();
        if let Some(sz) = &size {
            let t = py::str_of(sz);
            let digits = t.trim_end_matches(['k', 'm', 'g']);
            let ok = tm
                && !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
                && t.len() - digits.len() <= 1;
            if !ok {
                return Err(m(format!("{w}: size: only with tmpfs: true, e.g. 64m")));
            }
        }
        let copy_up = o.get("copy_up").is_some_and(py::truthy);
        if py::normpath(&dst) == "/data" && copy_up {
            return Err(m(format!(
                "{w}: copy_up: not on dst '/data' (the image's own /data is never visible: /data is the data disk)"
            )));
        }
        out.push(Mount {
            dst: py::normpath(&dst),
            src: src.map(|x| py::str_of(&x)),
            tmpfs: tm,
            copy_up,
            size: size.map(|x| py::str_of(&x)),
        });
    }
    Ok(out)
}

/// `prepare(m, manifest_path, arch)`: for every service with image:, read the image's config,
/// compute the effective command, env, workdir and user, point the service's cmd at the
/// generated launcher and drop the image keys from the service.
pub fn prepare(
    m: &mut Value,
    manifest_path: &str,
    arch: &str,
    io: &mut Io,
) -> Result<Vec<OciService>, Fail> {
    let any = m
        .get("services")
        .and_then(Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .any(|s| s.as_object().is_some_and(|o| o.contains_key("image")))
        });
    if !any {
        return Ok(Vec::new());
    }
    let mdir = py::dirname(&py::abspath(manifest_path));
    let mut out = Vec::new();
    let n = m["services"].as_array().map(|a| a.len()).unwrap_or(0);
    for i in 0..n {
        let s = m["services"][i]
            .as_object_mut()
            .ok_or_else(|| Fail::Crash(format!("services[{i}] is not a mapping")))?;
        let wh = format!(
            "services[{}] ({})",
            i,
            py::str_of(s.get("name").unwrap_or(&Value::Null))
        );
        if !s.contains_key("image") {
            let extra: Vec<&str> = OCI_KEYS
                .iter()
                .copied()
                .filter(|k| s.contains_key(*k))
                .collect();
            if !extra.is_empty() {
                return Err(Fail::Manifest(format!(
                    "{wh}: {} only with image:",
                    extra.join(", ")
                )));
            }
            continue;
        }
        let img_v = s.shift_remove("image").unwrap_or(Value::Null);
        let path = image_path(&img_v, arch, &mdir, &wh).map_err(Fail::Manifest)?;
        let rf = s.shift_remove("image_ref");
        let rf = match rf {
            None | Some(Value::Null) => None,
            Some(x) => Some(py::str_of(&x)),
        };
        let img = Image::new(&path, rf.as_deref(), arch).map_err(Fail::Manifest)?;
        let cfg = img.config_section();
        let (entry, dflt_cmd) = if s.contains_key("entrypoint") {
            let v = s.shift_remove("entrypoint").unwrap_or(Value::Null);
            let e = match &v {
                Value::Array(a) => words(a),
                Value::Null => String::new(),
                other => py::str_of(other),
            };
            (e, String::new())
        } else {
            (
                words_of(sget(&cfg, "Entrypoint")),
                words_of(sget(&cfg, "Cmd")),
            )
        };
        let cmd = match s.get("cmd") {
            Some(Value::Array(a)) => words(a),
            Some(v) => py::str_of(v),
            None => dflt_cmd,
        };
        let final_cmd = py::strip(&format!("{entry} {cmd}")).to_string();
        if final_cmd.is_empty() {
            return Err(Fail::Manifest(format!(
                "{wh}: the image has no Entrypoint/Cmd; give cmd:"
            )));
        }
        let mut env: Vec<(String, String)> = Vec::new();
        let mut keys: HashMap<String, usize> = HashMap::new();
        if let Some(Value::Array(a)) = sget(&cfg, "Env") {
            for kvs in a {
                let t = kvs.as_str().ok_or_else(|| {
                    Fail::Crash(format!(
                        "{path}: the image config's Env has a non-string entry"
                    ))
                })?;
                let (k, v) = t.split_once('=').unwrap_or((t, ""));
                keys.insert(k.to_string(), env.len());
                env.push((k.to_string(), v.to_string()));
            }
        }
        let menv = s.shift_remove("env").filter(py::truthy);
        match menv {
            None => {}
            Some(Value::Object(o)) => {
                for (k, v) in o {
                    if !crate::fullmatch_ident(&k) {
                        return Err(Fail::Manifest(format!(
                            "{wh}: env: {} is not a valid variable name",
                            py::repr_str(&k)
                        )));
                    }
                    let v = match v {
                        Value::Null => String::new(),
                        Value::Bool(b) => if b { "true" } else { "false" }.to_string(),
                        other => py::str_of(&other),
                    };
                    if let Some(&j) = keys.get(&k) {
                        env[j].1 = v;
                    } else {
                        keys.insert(k.clone(), env.len());
                        env.push((k, v));
                    }
                }
            }
            Some(_) => return Err(Fail::Manifest(format!("{wh}: env: must be a mapping"))),
        }
        if !keys.contains_key("PATH") {
            env.insert(0, ("PATH".into(), DEFAULT_PATH.into()));
        }
        let wd = s.shift_remove("workdir").filter(py::truthy);
        let workdir = match wd {
            Some(v) => py::str_of(&v),
            None => sget(&cfg, "WorkingDir")
                .filter(|x| py::truthy(x))
                .map(py::str_of)
                .unwrap_or_else(|| "/".into()),
        };
        if !workdir.starts_with('/') {
            return Err(Fail::Manifest(format!(
                "{wh}: workdir {} must be absolute",
                py::repr_str(&workdir)
            )));
        }
        let user = match s.shift_remove("user") {
            None | Some(Value::Null) => sget(&cfg, "User")
                .filter(|x| py::truthy(x))
                .map(py::str_of)
                .unwrap_or_default(),
            Some(u) => py::str_of(&u),
        };
        let user = if ["root", "0", "0:0", "root:root"].contains(&user.as_str()) {
            String::new()
        } else {
            user
        };
        let mounts = mounts_of(s.shift_remove("mounts"), &wh)?;
        let writable = match s.shift_remove("writable") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(Fail::Manifest(format!("{wh}: writable: true or false"))),
        };
        let name = py::str_of(s.get("name").unwrap_or(&Value::Null));
        s.insert(
            "cmd".into(),
            Value::String(format!("/etc/hydo/oci/{name}/run")),
        );
        let tags = if img.ref_names.is_empty() {
            "untagged".to_string()
        } else {
            img.ref_names.join(",")
        };
        let man = img
            .manifest_digest
            .as_ref()
            .map(|d| format!(", manifest {}", head(d, 19)))
            .unwrap_or_default();
        errln(
            io,
            &format!(
                "hydo-build: service {name} from image {path} ({tags}, config {}{man})",
                head(&img.config_digest, 19)
            ),
        );
        out.push(OciService {
            svc: name,
            path,
            img,
            final_cmd,
            env,
            workdir,
            user,
            mounts,
            writable,
        });
    }
    Ok(out)
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn under_binds(dst: &str) -> bool {
    let first = dst.trim_start_matches('/').split('/').next().unwrap_or("");
    BINDS.contains(&first)
}

/// `_mount_lines(o)`: the launcher head's mounts, once per boot, under a flock.
fn mount_lines(o: &OciService) -> String {
    mount_lines_of(&o.svc, &o.mounts)
}

fn mount_lines_of(svc: &str, mounts: &[Mount]) -> String {
    let mut l = vec![
        format!("if [ ! -f /tmp/hydo/oci-mnt.{svc}.done ]; then"),
        format!("  $BB mkdir -p /tmp/hydo; exec 9> /tmp/hydo/oci-mnt.{svc}.lock; $BB flock 9"),
        "  hmnt() { $BB grep -q \" $1 \" /proc/mounts; }".to_string(),
        format!("  if [ ! -f /tmp/hydo/oci-mnt.{svc}.done ]; then"),
        "    for l in fd:/proc/self/fd stdin:/proc/self/fd/0 stdout:/proc/self/fd/1 stderr:/proc/self/fd/2; do".to_string(),
        "      [ -e /dev/${l%%:*} ] || $BB ln -s ${l#*:} /dev/${l%%:*}".to_string(),
        "    done".to_string(),
    ];
    for (k, mt) in mounts.iter().enumerate() {
        let d = format!("$R{}", mt.dst);
        if under_binds(&mt.dst) {
            l.push(format!("    [ -e {d} ] || $BB mkdir -p {d}"));
        }
        if mt.tmpfs {
            let t = format!("/tmp/hydo/oci-tmpfs/{svc}/{k}");
            let opt = mt
                .size
                .as_ref()
                .map(|s| format!("-o size={s} "))
                .unwrap_or_default();
            l.push(format!("    if ! hmnt {d}; then"));
            l.push(format!(
                "      $BB mkdir -p {t} && $BB mount -t tmpfs {opt}none {t} || exit 125"
            ));
            if mt.copy_up {
                l.push(format!(
                    "      [ -d {d} ] && $BB cp -a {d}/. {t}/ 2>/dev/null"
                ));
                l.push(format!(
                    "      [ -d {d} ] && $BB chown $($BB stat -c %u:%g {d}) {t} && $BB chmod $($BB stat -c %a {d}) {t}"
                ));
            } else {
                l.push(format!("      $BB chmod 1777 {t}"));
            }
            l.push(format!("      $BB mount --bind {t} {d} || exit 125"));
            l.push("    fi".into());
        } else if mt.dst == "/data" {
            // a data-disk directory over the data disk's own bind (made by the head above, so
            // `hmnt` would always say yes): once per boot, guarded by the done file
            let s = mt.src.clone().unwrap_or_default();
            l.push(format!(
                "    $BB mkdir -p {s} && $BB mount --bind {s} {d} || exit 125"
            ));
        } else {
            let s = mt.src.clone().unwrap_or_default();
            l.push(format!("    if ! hmnt {d}; then"));
            if mt.copy_up {
                l.push(format!("      $BB mkdir -p {s}"));
                l.push(format!(
                    "      if [ -d {d} ] && [ -z \"$($BB ls -A {s})\" ]; then $BB cp -a {d}/. {s}/ 2>/dev/null; $BB chown $($BB stat -c %u:%g {d}) {s}; $BB chmod $($BB stat -c %a {d}) {s}; fi"
                ));
            }
            l.push(format!("      $BB mount --bind {s} {d} || exit 125"));
            l.push("    fi".into());
        }
    }
    l.push(format!("    : > /tmp/hydo/oci-mnt.{svc}.done"));
    l.push("  fi".into());
    l.push("  exec 9>&-".into());
    l.push("fi".into());
    l.join("\n") + "\n"
}

/// `writable: true`: bind the service's layer over its root, once per boot, under a lock (the
/// launcher and `hydo-oci` may start together). An x86_64 image carries the layer on its data
/// partition already (hydo-build put the image's tree there); an arm64 image copies its root
/// (in the initramfs) to the data disk at the first start and syncs it before saying so.
fn layer_lines(svc: &str) -> String {
    let l = layer_dir(svc);
    [
        "if ! $BB mountpoint -q $R; then".to_string(),
        format!("  $BB mkdir -p /tmp/hydo; exec 8> /tmp/hydo/oci-layer.{svc}.lock; $BB flock 8"),
        "  if ! $BB mountpoint -q $R; then".to_string(),
        format!("    L={l}"),
        "    if [ ! -f $L.done ]; then { $BB rm -rf $L && $BB mkdir -p $L && $BB cp -a $R/. $L/ && $BB sync && : > $L.done && $BB sync; } || exit 125; fi".to_string(),
        "    $BB mount --bind $L $R || exit 125".to_string(),
        "  fi".to_string(),
        "  exec 8>&-".to_string(),
        "fi".to_string(),
    ]
    .join("\n")
        + "\n"
}

/// `_scripts(o, home)`: the launcher (run) and `hydo-oci`'s helper (exec). `shares`: the service
/// whose tree this one uses (`root.share_images`): it is bind-mounted on this service's root first.
fn scripts(o: &OciService, home: &str, shares: Option<&str>) -> (String, String) {
    let svc = &o.svc;
    let r = format!("/oci/{svc}");
    let mut env: Vec<String> = o
        .env
        .iter()
        .map(|(k, v)| shq(&format!("{k}={v}")))
        .collect();
    if !o.env.iter().any(|(k, _)| k == "HOME") {
        env.push(shq(&format!("HOME={home}")));
    }
    for k in PASS_ENV {
        env.push(format!("{k}=\"${k}\""));
    }
    let layer = if o.writable {
        layer_lines(svc)
    } else {
        String::new()
    };
    let mut head = format!(
        "#!/bin/busybox sh\n# generated by hvx/onboard/hydo_oci.py -- service {svc} from {}\nBB=/bin/busybox R={r}\n{layer}for d in {}; do $BB mountpoint -q $R/$d || $BB mount --bind /$d $R/$d || exit 125; done\n",
        o.img.config_digest,
        BINDS.join(" ")
    );
    if let Some(owner) = shares {
        // the shared tree first, then the binds and mounts on this service's own mount of it.
        // Once per boot: /proc/mounts decides, because busybox `mountpoint` compares the device
        // with the parent's, and a bind of the same root file system has the same device (a
        // restart stacked a second bind over the first and hid the service's own mounts).
        head = head.replacen(
            "for d in",
            &format!(
                "$BB grep -q \" $R \" /proc/mounts || $BB mount --bind /oci/{owner} $R || exit 125\nfor d in"
            ),
            1,
        );
    }
    if !o.mounts.is_empty() {
        head.push_str(&mount_lines(o));
    }
    let chroot = format!(
        "exec $BB chroot $R /.hydo/busybox env -i {} /.hydo/busybox sh -c",
        env.join(" ")
    );
    let user = if o.user.is_empty() {
        String::new()
    } else {
        format!("/.hydo/busybox chpst -u {} ", shq(&o.user))
    };
    let run = format!(
        "{head}{chroot} {}\n",
        shq(&format!(
            "cd {} || exit 127; exec {user}{}",
            shq(&o.workdir),
            o.final_cmd
        ))
    );
    let ex = head.replacen(
        "for d in",
        &format!(
            "[ $# -gt 0 ] || {{ echo \"usage: hydo-oci {svc} <command> [args]\" >&2; exit 2; }}\nfor d in"
        ),
        1,
    ) + &format!(
        "{chroot} {} {} \"$@\"\n",
        shq("cd \"$0\" || exit 127; exec \"$@\""),
        shq(&o.workdir)
    );
    (run, ex)
}

const HYDO_OCI: &str = "#!/bin/busybox sh\n\
# hydo-oci <service> <command> [args...]: run a command inside an image service's root, with the\n\
# image's env and working directory (as root). Generated by hvx/onboard/hydo_oci.py.\n\
s=$1\n\
[ -n \"$s\" ] && [ -x /etc/hydo/oci/$s/exec ] || { echo \"hydo-oci: no image service '$s'\" >&2; exit 127; }\n\
shift\n\
exec /etc/hydo/oci/$s/exec \"$@\"\n";

fn hydo_oci_netns() -> String {
    HYDO_OCI.replace(
        "exec /etc/hydo/oci/$s/exec \"$@\"\n",
        "[ -e /tmp/hydo/ns/$s ] || { echo \"hydo-oci: no network namespace for $s\" >&2; exit 125; }\n\
exec /bin/busybox nsenter -n/tmp/hydo/ns/$s /etc/hydo/oci/$s/exec \"$@\"\n",
    )
}

fn mounts_json(ms: &[Mount]) -> Value {
    Value::Array(
        ms.iter()
            .map(|m| {
                let mut o = Map::new();
                o.insert("dst".into(), Value::String(m.dst.clone()));
                o.insert(
                    "src".into(),
                    m.src.clone().map(Value::String).unwrap_or(Value::Null),
                );
                o.insert("tmpfs".into(), Value::Bool(m.tmpfs));
                o.insert("copy_up".into(), Value::Bool(m.copy_up));
                o.insert(
                    "size".into(),
                    m.size.clone().map(Value::String).unwrap_or(Value::Null),
                );
                Value::Object(o)
            })
            .collect(),
    )
}

/// Unpack a service's image into its tree: the layers in order, the bind mount points, hydo's
/// busybox and /etc/hosts (`hosts`).
fn unpack(o: &OciService, t: &mut Tree, tmpdir: &Path, busybox: &Path, hosts: &str) -> R<()> {
    for (k, ((name, dig), diff)) in o.img.layers.iter().zip(o.img.diff_ids.iter()).enumerate() {
        let lt = tmpdir.join(format!("layer-{k:03}.tar"));
        let diff = diff
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| py::str_of(diff));
        layer_to_file(&o.img, name, dig.as_deref(), Some(&diff), &lt)?;
        t.apply_layer(&lt)?;
        fs::remove_file(&lt).map_err(|e| e.to_string())?;
    }
    for d in BINDS {
        t.ensure_dir(d, if *d == "tmp" { 0o1777 } else { 0o755 })?;
    }
    t.ensure_dir(".hydo", 0o755)?;
    t.meta.insert(".hydo".into(), (S_IFDIR, 0o755, 0, 0));
    let bb = fs::read(busybox).map_err(|e| e.to_string())?;
    t.put_file(".hydo/busybox", &bb, 0o755)?;
    t.put_file("etc/hosts", hosts.as_bytes(), 0o644)?;
    Ok(())
}

/// Readiness commands that run a program the image does not have (no silent passes,
/// 2026-11-07): `hydo compose` turns a Compose healthcheck into `ready: ... hydo-oci <svc> <prog>
/// ...`, and an image without `<prog>` (the libSQL server's image has no `curl`) fails the
/// readiness check of every run, after its whole time limit. Called after [`stage`] or
/// [`stage_with`] with the same `share` and `layers`, so that each service's tree is looked at
/// where it was unpacked (its own R/oci/<svc>, the tree it shares with `root.share_images`, or
/// its `writable: true` layer on the staged data partition); one warning per such command.
pub fn ready_warnings(
    oci: &[OciService],
    root: &Path,
    m: &Value,
    share: bool,
    layers: Option<&Path>,
) -> Vec<String> {
    let paths: Vec<(String, String, PathBuf)> = oci
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let p = o
                .env
                .iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v.clone());
            let tree = match layers {
                Some(l) if o.writable => l.join(".hydo/layers").join(&o.svc),
                _ => {
                    // the first earlier service of the same image that is not writable
                    let owner = oci[..i]
                        .iter()
                        .find(|x| {
                            share
                                && !o.writable
                                && !x.writable
                                && x.img.config_digest == o.img.config_digest
                        })
                        .map_or(o.svc.as_str(), |x| x.svc.as_str());
                    root.join("oci").join(owner)
                }
            };
            (
                o.svc.clone(),
                p.unwrap_or_else(|| DEFAULT_PATH.to_string()),
                tree,
            )
        })
        .collect();
    ready_warnings_in(&paths, m)
}

/// [`ready_warnings`] for image services given as (service, the image's PATH, its tree).
fn ready_warnings_in(paths: &[(String, String, PathBuf)], m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let services = m.get("services").and_then(Value::as_array);
    for s in services.into_iter().flatten() {
        let name = s.get("name").and_then(Value::as_str).unwrap_or("");
        let Some(ready) = s.get("ready").and_then(Value::as_str) else {
            continue;
        };
        let w: Vec<&str> = ready.split_whitespace().collect();
        for (i, tok) in w.iter().enumerate() {
            if *tok != "hydo-oci" && !tok.ends_with("/hydo-oci") {
                continue;
            }
            let (Some(svc), Some(prog)) = (w.get(i + 1), w.get(i + 2)) else {
                continue;
            };
            // a word the shell expands or a quoted one cannot be looked up here
            if prog.starts_with('-') || prog.contains(['$', '"', '\'', '`', '(', ';', '|', '&']) {
                continue;
            }
            let Some((_, path, base)) = paths.iter().find(|(s, _, _)| s.as_str() == *svc) else {
                continue;
            };
            let found = if prog.contains('/') {
                fsx::lexists(&base.join(prog.trim_start_matches('/')))
            } else {
                path.split(':')
                    .filter(|d| !d.is_empty())
                    .any(|d| fsx::lexists(&base.join(d.trim_start_matches('/')).join(prog)))
            };
            if !found {
                out.push(format!(
                    "services: {name}: ready: runs {prog} inside the image of {svc} (hydo-oci), \
                     and that image has no {prog} (PATH {path}): the readiness check would fail \
                     in every run; use a program the image has, or the guest's busybox outside \
                     hydo-oci (for example wget -q -O /dev/null http://<addr>:<port>/<path>)"
                ));
            }
        }
    }
    out
}

/// `stage(oci, R, busybox, tmpdir, netns, share)`: flatten every image service's image into
/// R/oci/<svc>, add the launchers. Returns the owners and modes of everything under R/oci.
///
/// `share` (`root.share_images: true`, opt-in): a service whose image (by config digest) an
/// earlier service already unpacked gets no tree of its own: its R/oci/<svc> is an empty
/// directory, and its launcher bind-mounts the first service's tree there before anything else
/// (a plain bind, so the first service's own mounts are not seen through it: each service keeps
/// its own /tmp, /data, volumes and tmpfs mounts on its own mount point). The mount points of
/// every sharing service's `mounts:` are made in the one tree. Opt-in because it changes the
/// bytes of every image whose services share an image (libSQL's primary and replica: one tree
/// instead of two copies of a 576 MiB root).
pub fn stage(
    oci: &mut [OciService],
    root: &Path,
    busybox: &Path,
    tmpdir: &Path,
    netns: bool,
    share: bool,
    io: &mut Io,
) -> Result<HashMap<String, MetaEntry>, Fail> {
    stage_with(oci, root, busybox, tmpdir, netns, share, None, None, io).map(|(m, _)| m)
}

/// Owners and modes by path.
pub type Metas = HashMap<String, MetaEntry>;

/// `stage` with `writable:` layers and `tls:` trust. `layers`: the staged data partition, where
/// an x86_64 build puts the tree of every `writable: true` service (at .hydo/layers/<svc>, with
/// its .done file; the root keeps an empty /oci/<svc> to bind it on); None (arm64) keeps every
/// tree in the root, and the launcher copies it to the data disk. Returns the owners and modes
/// under R/oci and under the data partition.
#[allow(clippy::too_many_arguments)]
pub fn stage_with(
    oci: &mut [OciService],
    root: &Path,
    busybox: &Path,
    tmpdir: &Path,
    netns: bool,
    share: bool,
    layers: Option<&Path>,
    tls: Option<&TlsTrust>,
    io: &mut Io,
) -> Result<(Metas, Metas), Fail> {
    let mut lmeta: HashMap<String, MetaEntry> = HashMap::new();
    if oci.is_empty() {
        return Ok((HashMap::new(), lmeta));
    }
    let mut meta: HashMap<String, MetaEntry> = HashMap::new();
    meta.insert("oci".into(), (S_IFDIR, 0o755, 0, 0));
    fsx::makedirs(&root.join("oci"))?;
    if tmpdir.is_dir() {
        fs::remove_dir_all(tmpdir).map_err(|e| io_fail("cannot remove", tmpdir, e))?;
    }
    fsx::makedirs(tmpdir)?;
    // the trees unpacked so far: (config digest, when other services may share the tree; the
    // service that owns it; the tree; the bundle `tls:` trusts in it)
    let mut trees: Vec<(Option<String>, String, Tree, Option<String>)> = Vec::new();
    for o in oci.iter_mut() {
        let svc = o.svc.clone();
        let wrap = |e: String| Fail::Manifest(format!("service {svc}, image {}: {e}", o.path));
        // a writable service's tree is its own layer: it neither shares a tree nor is shared
        let shared = if share && !o.writable {
            trees
                .iter()
                .position(|x| x.0.as_deref() == Some(o.img.config_digest.as_str()))
        } else {
            None
        };
        let in_layer = o.writable && layers.is_some();
        let ti = match shared {
            Some(i) => {
                let d = root.join("oci").join(&o.svc);
                fsx::makedirs(&d)?;
                meta.insert(format!("oci/{}", o.svc), (S_IFDIR, 0o755, 0, 0));
                i
            }
            None => {
                let tree_root = match layers {
                    Some(l) if o.writable => {
                        // the root keeps an empty directory for the bind
                        fsx::makedirs(&root.join("oci").join(&o.svc))?;
                        l.join(".hydo/layers").join(&o.svc)
                    }
                    _ => root.join("oci").join(&o.svc),
                };
                trees.push((
                    (!o.writable).then(|| o.img.config_digest.clone()),
                    o.svc.clone(),
                    Tree::new(tree_root).map_err(wrap)?,
                    None,
                ));
                trees.len() - 1
            }
        };
        let owner = trees[ti].1.clone();
        let (_, _, t, bundle) = &mut trees[ti];
        let hosts = match tls {
            None => HOSTS.to_string(),
            Some(tt) => format!("{HOSTS}{}", tt.host_lines),
        };
        let res = (|| -> R<String> {
            if shared.is_none() {
                unpack(o, t, tmpdir, busybox, &hosts)?;
                if let Some(tt) = tls {
                    *bundle = Some(trust_image(t, tt)?);
                }
            }
            if let Some(b) = bundle.as_deref() {
                trust_env(b, &mut o.env);
            }
            for mt in o.mounts.iter_mut() {
                if under_binds(&mt.dst) {
                    continue;
                }
                let comps = Tree::parts(&mt.dst)?;
                let d = t
                    .resolve_dir(&comps[..comps.len() - 1], true)?
                    .unwrap_or_default();
                let mut rv = d.clone();
                rv.push(comps[comps.len() - 1].clone());
                let mut rel = rjoin(&rv);
                let is_file = mt
                    .src
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .is_some_and(|s| root.join(s.trim_start_matches('/')).is_file());
                if is_link(&t.abs(&rel)) {
                    if is_file {
                        return Err(format!(
                            "mounts: dst {} is a symlink in the image; give the path it points to",
                            mt.dst
                        ));
                    }
                    rel = rjoin(&t.resolve_dir(&rv, true)?.unwrap_or_default());
                } else if !lexists(&t.abs(&rel)) {
                    if is_file {
                        t.put_file(&rel, b"", 0o644)?;
                    } else {
                        t.ensure_dir(&rel, 0o755)?;
                    }
                }
                mt.dst = format!("/{rel}");
            }
            Ok(t.home_of(&o.user))
        })();
        let home =
            res.map_err(|e| Fail::Manifest(format!("service {}, image {}: {e}", o.svc, o.path)))?;
        if in_layer {
            for (k, v) in &t.meta {
                let key = if k.is_empty() {
                    format!(".hydo/layers/{}", o.svc)
                } else {
                    format!(".hydo/layers/{}/{k}", o.svc)
                };
                lmeta.insert(key, *v);
            }
            lmeta.insert(".hydo".into(), (S_IFDIR, 0o755, 0, 0));
            lmeta.insert(".hydo/layers".into(), (S_IFDIR, 0o755, 0, 0));
            let done = format!(".hydo/layers/{}.done", o.svc);
            if let Some(l) = layers {
                fsx::write_text(&l.join(&done), "")?;
            }
            lmeta.insert(done, (S_IFREG, 0o644, 0, 0));
            meta.insert(format!("oci/{}", o.svc), (S_IFDIR, 0o755, 0, 0));
        } else {
            // the tree's entries under its owner's path (a sharing service adds its mount points)
            for (k, v) in &t.meta {
                let key = if k.is_empty() {
                    format!("oci/{owner}")
                } else {
                    format!("oci/{owner}/{k}")
                };
                meta.insert(key, *v);
            }
        }
        let (run, ex) = scripts(o, &home, shared.map(|_| owner.as_str()));
        let d = root.join("etc/hydo/oci").join(&o.svc);
        fsx::makedirs(&d)?;
        for (n, c) in [("run", &run), ("exec", &ex)] {
            fsx::write_text(&d.join(n), c)?;
            fsx::chmod(&d.join(n), 0o755)?;
        }
        let cfg = o.img.config_section();
        let mut ic = Map::new();
        for k in ["Entrypoint", "Cmd", "Env", "WorkingDir", "User"] {
            ic.insert(k.into(), cfg.get(k).cloned().unwrap_or(Value::Null));
        }
        let mut eff = Map::new();
        eff.insert("command".into(), Value::String(o.final_cmd.clone()));
        eff.insert(
            "env".into(),
            Value::Array(
                o.env
                    .iter()
                    .map(|(k, v)| {
                        Value::Array(vec![Value::String(k.clone()), Value::String(v.clone())])
                    })
                    .collect(),
            ),
        );
        eff.insert("workdir".into(), Value::String(o.workdir.clone()));
        eff.insert("user".into(), Value::String(o.user.clone()));
        if !o.mounts.is_empty() {
            eff.insert("mounts".into(), mounts_json(&o.mounts));
        }
        let mut rec = Map::new();
        rec.insert("service".into(), Value::String(o.svc.clone()));
        rec.insert(
            "source".into(),
            Value::String(py::basename(o.path.trim_end_matches('/'))),
        );
        rec.insert(
            "ref_names".into(),
            Value::Array(o.img.ref_names.iter().cloned().map(Value::String).collect()),
        );
        rec.insert(
            "manifest_digest".into(),
            o.img
                .manifest_digest
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        rec.insert(
            "config_digest".into(),
            Value::String(o.img.config_digest.clone()),
        );
        rec.insert(
            "architecture".into(),
            o.img
                .config
                .get("architecture")
                .cloned()
                .unwrap_or(Value::Null),
        );
        rec.insert("layers".into(), Value::Array(o.img.diff_ids.clone()));
        rec.insert("image_config".into(), Value::Object(ic));
        rec.insert("effective".into(), Value::Object(eff));
        rec.insert("skipped_special_files".into(), Value::from(t.skipped));
        if shared.is_some() {
            rec.insert("shares_tree_of".into(), Value::String(owner.clone()));
        }
        fsx::write_text(
            &d.join("image.json"),
            &(pyjson::dumps_indent1_sorted(&Value::Object(rec)) + "\n"),
        )?;
        if o.writable {
            outln(
                io,
                &format!(
                    "hydo-build: service {} is writable: its layer is {} on the data disk{}",
                    o.svc,
                    layer_dir(&o.svc),
                    if in_layer {
                        " (the image's tree is put there)"
                    } else {
                        " (copied there at the first start)"
                    }
                ),
            );
        }
        if shared.is_some() {
            outln(
                io,
                &format!(
                    "hydo-build: image {} -> /oci/{} (the tree of /oci/{owner}, root.share_images)",
                    head(&o.img.config_digest, 19),
                    o.svc
                ),
            );
            continue;
        }
        let skipped = if t.skipped > 0 {
            format!(", {} device/FIFO entries skipped", t.skipped)
        } else {
            String::new()
        };
        outln(
            io,
            &format!(
                "hydo-build: image {} -> /oci/{} ({} layers, {} entries{skipped})",
                head(&o.img.config_digest, 19),
                o.svc,
                o.img.layers.len(),
                t.meta.len()
            ),
        );
    }
    let hb = root.join("bin/hydo-oci");
    fsx::write_text(
        &hb,
        &if netns {
            hydo_oci_netns()
        } else {
            HYDO_OCI.to_string()
        },
    )?;
    fsx::chmod(&hb, 0o755)?;
    fs::remove_dir_all(tmpdir).map_err(|e| io_fail("cannot remove", tmpdir, e))?;
    Ok((meta, lmeta))
}

/// A file of the image by its path, symlinks followed inside the image ("" when missing).
fn image_file(t: &mut Tree, rel: &str) -> R<Option<String>> {
    let mut rel = rel.to_string();
    for _ in 0..40 {
        let comps: Vec<String> = rel
            .split('/')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect();
        let Some((last, dirs)) = comps.split_last() else {
            return Ok(None);
        };
        let Some(mut d) = t.resolve_dir(dirs, false)? else {
            return Ok(None);
        };
        if last == ".." || last == "." {
            return Ok(None);
        }
        d.push(last.clone());
        let r = rjoin(&d);
        let p = t.abs(&r);
        if is_link(&p) {
            let tg = fs::read_link(&p).map_err(|e| e.to_string())?;
            let tg = tg.to_string_lossy().into_owned();
            d.pop();
            rel = if tg.starts_with('/') {
                tg
            } else {
                format!("{}/{tg}", rjoin(&d))
            };
            continue;
        }
        return Ok(p.is_file().then_some(r));
    }
    Err(format!("symlink loop at {rel}"))
}

/// `tls:`: the CA into the image's system bundles (each real file once; made at the Debian path
/// if the image has none) and its own copy at /.hydo/tls-ca.pem. Returns the bundle the trust
/// variables name ([`trust_env`]).
fn trust_image(t: &mut Tree, tt: &TlsTrust) -> R<String> {
    t.put_file(
        crate::tls::IMAGE_CA.trim_start_matches('/'),
        tt.ca_pem.as_bytes(),
        0o644,
    )?;
    let mut done: Vec<String> = Vec::new();
    for b in crate::tls::BUNDLES {
        if let Some(r) = image_file(t, b)? {
            if !done.contains(&r) {
                crate::tls::append_pem(&t.abs(&r), &tt.ca_pem).map_err(|e| e.message())?;
                done.push(r);
            }
        }
    }
    // where the distributions' own tools look when an entrypoint rebuilds the bundle
    // (update-ca-certificates on Debian and Alpine, update-ca-trust on RHEL), so a rebuild keeps it
    for (dir, file) in crate::tls::ANCHORS {
        if t.resolve_dir(&Tree::parts(dir)?, false)?.is_some() {
            t.put_file(&format!("{dir}/{file}"), tt.ca_pem.as_bytes(), 0o644)?;
        }
    }
    Ok(match done.first() {
        Some(r) => format!("/{r}"),
        None => {
            let r = crate::tls::BUNDLE.trim_start_matches('/');
            t.put_file(r, tt.ca_pem.as_bytes(), 0o644)?;
            crate::tls::BUNDLE.to_string()
        }
    })
}

/// `tls:`: the trust variables in a service's environment unless the image or the manifest sets
/// them (`bundle`: what [`trust_image`] returned for the service's tree).
fn trust_env(bundle: &str, env: &mut Vec<(String, String)>) {
    for (k, v) in crate::tls::env(crate::tls::IMAGE_CA, bundle) {
        if !env.iter().any(|(n, _)| n == k) {
            env.push((k.to_string(), v));
        }
    }
}

/// The cpio writer's view of the meta (arm64).
pub fn cpio_meta(meta: &HashMap<String, MetaEntry>) -> Meta {
    meta.iter()
        .map(|(k, (kind, mode, uid, gid))| {
            (k.clone(), (*kind, *mode as u32, *uid as u32, *gid as u32))
        })
        .collect()
}

/// `fix_ext4(meta, T, img, FAKE_TS)`: the image files' real owners and modes into the ext4 root
/// with debugfs (the building user cannot chown), under the frozen clock.
pub fn fix_ext4(
    meta: &HashMap<String, MetaEntry>,
    debugfs: &hydo_config::build::Input,
    mke2fs: &str,
    faketime: &str,
    img: &Path,
    io: &mut Io,
) -> Result<(), Fail> {
    if meta.is_empty() {
        return Ok(());
    }
    let dbg = debugfs.nonempty().map(PathBuf::from).unwrap_or_else(|| {
        Path::new(mke2fs)
            .parent()
            .unwrap_or(Path::new(""))
            .join("debugfs")
    });
    if !dbg.is_file() {
        return Err(Fail::Exit(format!(
            "hydo-build: debugfs not found at {} (set HYDO_DEBUGFS)",
            dbg.display()
        )));
    }
    let mut keys: Vec<&String> = meta.keys().collect();
    keys.sort();
    let mut lines = Vec::new();
    for rel in keys {
        let (kind, mode, uid, gid) = meta[rel];
        let p = format!("\"/{rel}\"");
        if kind != S_IFLNK {
            lines.push(format!("sif {p} mode 0{:o}", kind as i64 | mode));
        }
        lines.push(format!("sif {p} uid {uid}"));
        lines.push(format!("sif {p} gid {gid}"));
    }
    let cmdf = PathBuf::from(format!("{}.debugfs", img.display()));
    fsx::write_text(&cmdf, &(lines.join("\n") + "\n"))?;
    let r = std::process::Command::new(faketime)
        .arg(crate::image::FAKE_TS)
        .arg(&dbg)
        .arg("-w")
        .arg("-f")
        .arg(&cmdf)
        .arg(img)
        .env("TZ", crate::image::FAKE_TZ)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    let _ = fs::remove_file(&cmdf);
    let r = r.map_err(|e| {
        Fail::Exit(format!(
            "hydo-build: debugfs owner/mode pass failed:\n  {e}"
        ))
    })?;
    let errs: Vec<String> = String::from_utf8_lossy(&r.stderr)
        .lines()
        .filter(|x| !x.trim().is_empty() && !x.starts_with("debugfs "))
        .map(str::to_string)
        .collect();
    if !r.status.success() || !errs.is_empty() {
        return Err(Fail::Exit(format!(
            "hydo-build: debugfs owner/mode pass failed:\n  {}",
            errs.iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }
    outln(
        io,
        &format!(
            "hydo-build: image file owners and modes written ({} entries)",
            meta.len()
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A readiness command that runs, through hydo-oci, a program its image lacks is named;
    /// one whose program is there (on the image's PATH, or by its path) is not.
    #[test]
    fn ready_commands_with_a_program_the_image_lacks_are_named() {
        let root = std::env::temp_dir().join(format!("hydo-ready-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("oci/primary/bin")).unwrap();
        fs::write(root.join("oci/primary/bin/wget"), b"").unwrap();
        fs::create_dir_all(root.join("oci/primary/opt/x")).unwrap();
        fs::write(root.join("oci/primary/opt/x/probe"), b"").unwrap();
        let m = json!({"services": [
            {"name": "primary", "ready": "timeout 2 hydo-oci primary curl -fsS http://localhost:8080/health"},
            {"name": "a", "ready": "timeout 2 hydo-oci primary wget -q -O- http://localhost:8080/"},
            {"name": "b", "ready": "hydo-oci primary /opt/x/probe"},
            {"name": "c", "ready": "hydo-oci primary \"$X\""},
            {"name": "d", "ready": "redis-cli ping"},
            {"name": "e", "ready": "hydo-oci other curl x"}
        ]});
        let paths = vec![(
            "primary".to_string(),
            DEFAULT_PATH.to_string(),
            root.join("oci").join("primary"),
        )];
        let w = ready_warnings_in(&paths, &m);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].starts_with(
                "services: primary: ready: runs curl inside the image of primary (hydo-oci), and \
                 that image has no curl"
            ),
            "{}",
            w[0]
        );
    }

    /// The refusals of hydo_oci._mounts that test_hydo_compose.py's test_validation checked
    /// (deleted with the Python in M107): its eight cases with hydo_oci's messages.
    #[test]
    fn mounts_refusals() {
        assert!(mounts_of(None, "s").unwrap().is_empty());
        let w = "s: mounts[0]";
        let rule = "(letters, digits and ._@+=,:/- only)";
        let not = "an absolute path other than / /proc /sys /dev /tmp /data";
        let cases = [
            (
                json!([{"dst": "/x"}]),
                format!("{w}: give exactly one of src: (a guest path) and tmpfs: true"),
            ),
            (
                json!([{"src": "/a", "tmpfs": true, "dst": "/x"}]),
                format!("{w}: give exactly one of src: (a guest path) and tmpfs: true"),
            ),
            (
                json!([{"src": "/a", "dst": "/"}]),
                format!("{w}: dst '/': {not} {rule}"),
            ),
            (
                json!([{"src": "/a", "dst": "/data"}]),
                format!("{w}: dst '/data': {not} {rule}"),
            ),
            (
                json!([{"src": "/a", "dst": "rel"}]),
                format!("{w}: dst 'rel': {not} {rule}"),
            ),
            (
                json!([{"src": "/a b", "dst": "/x"}]),
                format!("{w}: src '/a b': an absolute guest path {rule}"),
            ),
            (
                json!([{"src": "/a", "dst": "/x", "size": "1m"}]),
                format!("{w}: size: only with tmpfs: true, e.g. 64m"),
            ),
            (
                json!([{"src": "/a", "dst": "/x", "bogus": 1}]),
                format!("{w}: unknown field(s) bogus (known: copy_up, dst, size, src, tmpfs)"),
            ),
        ];
        for (v, want) in cases {
            match mounts_of(Some(v.clone()), "s") {
                Err(Fail::Manifest(got)) => assert_eq!(got, want, "{v}"),
                Err(e) => panic!("{v}: not a manifest refusal: {}", e.message()),
                Ok(_) => panic!("{v}: accepted"),
            }
        }
    }

    /// A data-disk directory on /data (each node of a cluster with its own Compose volume on
    /// /data): accepted without copy_up, refused for any other source, mounted once per boot
    /// over the data disk's own bind without the `hmnt` test (which that bind always passes).
    #[test]
    fn a_data_disk_directory_on_data() {
        let w = "s: mounts[0]";
        let ok = mounts_of(
            Some(json!([{"src": "/data/vol/n1-data", "dst": "/data"}])),
            "s",
        )
        .unwrap_or_else(|e| panic!("refused: {}", e.message()));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].dst, "/data");
        let refused = [
            (
                json!([{"src": "/data/vol/n1-data", "dst": "/data", "copy_up": true}]),
                format!("{w}: copy_up: not on dst '/data' (the image's own /data is never visible: /data is the data disk)"),
            ),
            (
                json!([{"src": "/data", "dst": "/data"}]),
                format!("{w}: dst '/data': an absolute path other than / /proc /sys /dev /tmp /data (letters, digits and ._@+=,:/- only)"),
            ),
            (
                json!([{"src": "/data/../etc", "dst": "/data"}]),
                format!("{w}: dst '/data': an absolute path other than / /proc /sys /dev /tmp /data (letters, digits and ._@+=,:/- only)"),
            ),
            (
                json!([{"tmpfs": true, "dst": "/data"}]),
                format!("{w}: dst '/data': an absolute path other than / /proc /sys /dev /tmp /data (letters, digits and ._@+=,:/- only)"),
            ),
        ];
        for (v, want) in refused {
            match mounts_of(Some(v.clone()), "s") {
                Err(Fail::Manifest(got)) => assert_eq!(got, want, "{v}"),
                Err(e) => panic!("{v}: not a manifest refusal: {}", e.message()),
                Ok(_) => panic!("{v}: accepted"),
            }
        }
        let lines = mount_lines_of("n1", &ok);
        assert!(
            lines.contains(
                "    $BB mkdir -p /data/vol/n1-data && $BB mount --bind /data/vol/n1-data $R/data || exit 125\n"
            ),
            "{lines}"
        );
        assert!(!lines.contains("hmnt $R/data"), "{lines}");
    }
}

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
        let dst_ok = match &dst {
            Value::String(d) => {
                mount_path_ok(d)
                    && !format!("{d}/").contains("/../")
                    && !["/", "/proc", "/sys", "/dev", "/tmp", "/data", "/.hydo"]
                        .contains(&py::normpath(d).as_str())
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
        out.push(Mount {
            dst: py::normpath(&dst),
            src: src.map(|x| py::str_of(&x)),
            tmpfs: tm,
            copy_up: o.get("copy_up").is_some_and(py::truthy),
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
    let svc = &o.svc;
    let mut l = vec![
        format!("if [ ! -f /tmp/hydo/oci-mnt.{svc}.done ]; then"),
        format!("  $BB mkdir -p /tmp/hydo; exec 9> /tmp/hydo/oci-mnt.{svc}.lock; $BB flock 9"),
        "  hmnt() { $BB grep -q \" $1 \" /proc/mounts; }".to_string(),
        format!("  if [ ! -f /tmp/hydo/oci-mnt.{svc}.done ]; then"),
        "    for l in fd:/proc/self/fd stdin:/proc/self/fd/0 stdout:/proc/self/fd/1 stderr:/proc/self/fd/2; do".to_string(),
        "      [ -e /dev/${l%%:*} ] || $BB ln -s ${l#*:} /dev/${l%%:*}".to_string(),
        "    done".to_string(),
    ];
    for (k, mt) in o.mounts.iter().enumerate() {
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

/// `_scripts(o, home)`: the launcher (run) and `hydo-oci`'s helper (exec).
fn scripts(o: &OciService, home: &str) -> (String, String) {
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
    let mut head = format!(
        "#!/bin/busybox sh\n# generated by hvx/onboard/hydo_oci.py -- service {svc} from {}\nBB=/bin/busybox R={r}\nfor d in {}; do $BB mountpoint -q $R/$d || $BB mount --bind /$d $R/$d || exit 125; done\n",
        o.img.config_digest,
        BINDS.join(" ")
    );
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

/// `stage(oci, R, busybox, tmpdir, netns)`: flatten every image service's image into
/// R/oci/<svc>, add the launchers. Returns the owners and modes of everything under R/oci.
pub fn stage(
    oci: &mut [OciService],
    root: &Path,
    busybox: &Path,
    tmpdir: &Path,
    netns: bool,
    io: &mut Io,
) -> Result<HashMap<String, MetaEntry>, Fail> {
    if oci.is_empty() {
        return Ok(HashMap::new());
    }
    let mut meta: HashMap<String, MetaEntry> = HashMap::new();
    meta.insert("oci".into(), (S_IFDIR, 0o755, 0, 0));
    fsx::makedirs(&root.join("oci"))?;
    if tmpdir.is_dir() {
        fs::remove_dir_all(tmpdir).map_err(|e| io_fail("cannot remove", tmpdir, e))?;
    }
    fsx::makedirs(tmpdir)?;
    for o in oci.iter_mut() {
        let svc = o.svc.clone();
        let wrap = |e: String| Fail::Manifest(format!("service {svc}, image {}: {e}", o.path));
        let mut t = Tree::new(root.join("oci").join(&o.svc)).map_err(wrap)?;
        let res = (|| -> R<String> {
            for (k, ((name, dig), diff)) in
                o.img.layers.iter().zip(o.img.diff_ids.iter()).enumerate()
            {
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
            t.put_file("etc/hosts", HOSTS.as_bytes(), 0o644)?;
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
        for (k, v) in &t.meta {
            let key = if k.is_empty() {
                format!("oci/{}", o.svc)
            } else {
                format!("oci/{}/{k}", o.svc)
            };
            meta.insert(key, *v);
        }
        let (run, ex) = scripts(o, &home);
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
        fsx::write_text(
            &d.join("image.json"),
            &(pyjson::dumps_indent1_sorted(&Value::Object(rec)) + "\n"),
        )?;
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
    Ok(meta)
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
    mke2fs: &str,
    faketime: &str,
    img: &Path,
    io: &mut Io,
) -> Result<(), Fail> {
    if meta.is_empty() {
        return Ok(());
    }
    let dbg = std::env::var("HYDO_DEBUGFS")
        .ok()
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
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
}

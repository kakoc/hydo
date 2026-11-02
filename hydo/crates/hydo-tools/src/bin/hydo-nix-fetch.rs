//! hydo-nix-fetch -- copy store paths out of the public Nix binary cache without Nix.
//!
//!     hydo-nix-fetch [--closure] <out root> <store path or hash-name>...
//!
//! For each path: fetches https://cache.nixos.org/<hash>.narinfo (`$HYDO_NIX_CACHE` replaces the
//! cache), downloads the archive it names, checks the compressed file against the narinfo's
//! FileHash and the unpacked archive against its NarHash (both sha256, Nix base-32), and unpacks
//! the whole tree (directories, files with their executable bit, symlinks) at
//! <out root>/nix/store/<hash-name>. With --closure it follows the narinfo's References, so the
//! result is the path's whole runtime closure. A path already present under <out root> is not
//! fetched again. Prints one line per path: its NarHash and its name.
//!
//! The cache serves the same bytes for a store path forever (the path's hash fixes its inputs, and
//! NarHash fixes its contents), so a path name plus its NarHash is a pin. Used since Milestone 99
//! to take the aarch64 iproute2 (`ip`) and iptables of nixpkgs onto the Mac, which has no Nix, for
//! the arm64 guests with service networks. The port of hydo/dist/linux/nix_cache_fetch.py (M108):
//! the same command line, output and tree.

use hydo_manifest::py::{basename, dirname};
use hydo_tools::nar::{lexists, nix32, unpack, Hashing, Nar, Xz};
use hydo_tools::{die, pyjoin};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::Path;

const USAGE: &str =
    "nix_cache_fetch -- copy store paths out of the public Nix binary cache without Nix.

    hydo-nix-fetch [--closure] <out root> <store path or hash-name>...
";

struct Fetcher {
    cache: String,
    agent: ureq::Agent,
    skipped: Vec<String>,
}

impl Fetcher {
    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let r = self
            .agent
            .get(url)
            .call()
            .map_err(|e| format!("{url}: {e}"))?;
        if r.status() != 200 {
            return Err(format!("{url}: HTTP {}", r.status()));
        }
        Ok(r)
    }

    fn narinfo(&self, h: &str) -> Result<HashMap<String, String>, String> {
        let url = format!("{}/{}.narinfo", self.cache, h);
        let mut r = self.get(&url)?;
        let text = r
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{url}: {e}"))?;
        let mut info = HashMap::new();
        for line in text.lines() {
            let (k, v) = match line.split_once(": ") {
                Some((k, v)) => (k, v),
                None => (line, ""),
            };
            info.insert(k.to_string(), v.to_string());
        }
        Ok(info)
    }

    /// base = <hash>-<name>; the narinfo's References (hash-names).
    fn fetch(&mut self, base: &str, out: &str) -> Result<Vec<String>, String> {
        let h = base.split('-').next().unwrap_or("");
        let info = self.narinfo(h)?;
        let field = |k: &str| info.get(k).cloned();
        if field("StorePath").as_deref() != Some(&format!("/nix/store/{base}")) {
            return Err(format!(
                "nix_cache_fetch: {base}: the cache answers for {}",
                field("StorePath").unwrap_or_else(|| "None".into())
            ));
        }
        let dst = pyjoin(&pyjoin(&pyjoin(out, "nix"), "store"), base);
        let refs: Vec<String> = field("References")
            .unwrap_or_default()
            .split_whitespace()
            .filter(|r| *r != base)
            .map(str::to_string)
            .collect();
        let need = |k: &str| {
            field(k).ok_or_else(|| format!("nix_cache_fetch: {base}: the narinfo has no {k}"))
        };
        if lexists(&dst) {
            println!("{}  {} (present)", need("NarHash")?, base);
            return Ok(refs);
        }
        if field("Compression").as_deref() != Some("xz") {
            return Err(format!(
                "nix_cache_fetch: {base}: compression {} not supported",
                field("Compression").unwrap_or_else(|| "None".into())
            ));
        }
        let tmp = format!("{dst}.nar.xz.part");
        let d = dirname(&dst);
        if !d.is_empty() {
            std::fs::create_dir_all(&d).map_err(|e| format!("{d}: {e}"))?;
        }
        let url = format!("{}/{}", self.cache, need("URL")?);
        let mut r = self.get(&url)?;
        let mut body = r.body_mut().with_config().limit(u64::MAX).reader();
        let mut fh = Sha256::new();
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = body.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
                if n == 0 {
                    break;
                }
                fh.update(&buf[..n]);
                std::io::Write::write_all(&mut f, &buf[..n]).map_err(|e| format!("{tmp}: {e}"))?;
            }
        }
        if format!("sha256:{}", nix32(&fh.finalize())) != need("FileHash")? {
            return Err(format!("nix_cache_fetch: {base}: FileHash mismatch"));
        }
        let mut n = Nar::new(Hashing::new(Xz::open(Path::new(&tmp))?));
        n.expect("nix-archive-1")?;
        unpack(&mut n, Some(dst.clone()), &mut self.skipped)?;
        let mut one = [0u8; 1];
        if n.f
            .read(&mut one)
            .map_err(|e| format!("nar_extract: {e}"))?
            > 0
        {
            return Err(format!(
                "nix_cache_fetch: {base}: trailing data after the archive"
            ));
        }
        std::fs::remove_file(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
        let nh = format!("sha256:{}", nix32(&n.f.h.finalize()));
        if nh != need("NarHash")? {
            return Err(format!(
                "nix_cache_fetch: {base}: NarHash mismatch (unpacked tree left at {dst})"
            ));
        }
        println!("{}  {}", need("NarHash")?, base);
        Ok(refs)
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let closure = argv.iter().any(|a| a == "--closure");
    let args: Vec<&String> = argv.iter().filter(|a| *a != "--closure").collect();
    if args.len() < 2 {
        die(USAGE);
    }
    let out = args[0].as_str();
    let mut todo: VecDeque<String> = args[1..]
        .iter()
        .map(|p| basename(p.trim_end_matches('/')))
        .collect();
    let cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build();
    let mut f = Fetcher {
        cache: hydo_config::Config::process().tools.nix_cache,
        agent: cfg.into(),
        skipped: Vec::new(),
    };
    let mut seen = HashSet::new();
    while let Some(b) = todo.pop_front() {
        if !seen.insert(b.clone()) {
            continue;
        }
        let refs = f.fetch(&b, out).unwrap_or_else(|e| die(&e));
        if closure {
            todo.extend(refs);
        }
    }
    for p in &f.skipped {
        println!("not written (its name differs from an earlier entry's only in case): {p}");
    }
}

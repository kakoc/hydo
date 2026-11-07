//! The component list, `hydo/dist/<platform>/third-party.json` (format 1; the crate documentation
//! describes every field).

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The two releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Mac,
}

impl Platform {
    pub fn parse(s: &str) -> Option<Platform> {
        match s {
            "linux" => Some(Platform::Linux),
            "mac" => Some(Platform::Mac),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::Mac => "mac",
        }
    }
    /// The release's description in THIRD-PARTY.md.
    pub fn title(self) -> &'static str {
        match self {
            Platform::Linux => "Linux on x86_64",
            Platform::Mac => "macOS on Apple silicon",
        }
    }
    /// The component list of this platform, relative to the repository root.
    pub fn list_path(self) -> String {
        format!("hydo/dist/{}/third-party.json", self.name())
    }
    pub fn dist_dir(self) -> String {
        format!("hydo/dist/{}", self.name())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub format: u32,
    pub platform: String,
    #[serde(default)]
    pub about: Vec<String>,
    pub nixpkgs: Nixpkgs,
    pub rust: Vec<RustProgram>,
    #[serde(default)]
    pub rust_overrides: Vec<RustOverride>,
    pub components: Vec<Component>,
}

/// The nixpkgs tree whose recipes built every component with a `nix` entry; its archive goes
/// into the sources archive.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nixpkgs {
    pub rev: String,
    pub store_path: String,
    pub archive: String,
    pub sha256: String,
    pub url: String,
}

/// One cargo build whose outputs the release ships.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustProgram {
    /// The release paths of the programs this build makes.
    pub files: Vec<String>,
    /// The workspace's (or own workspace's) Cargo.toml, relative to the repository root.
    pub manifest: String,
    pub packages: Vec<String>,
    pub target: String,
}

/// What the list says about a crate whose registry source lacks a license field or texts.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustOverride {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub license: Option<String>,
    /// Repository paths of license texts, used when the crate ships none.
    #[serde(default)]
    pub texts: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub files: Vec<String>,
    pub texts: Vec<String>,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub repo: Vec<String>,
    #[serde(default)]
    pub nix: Vec<NixRef>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The file's name in a sources cache and in the archive's `upstream/` (absent: referenced only).
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub url: Vec<String>,
    /// How fetch-sources.sh obtains it again (crate documentation).
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NixRef {
    pub system: String,
    /// The nixpkgs attribute that built it, in the list's pinned nixpkgs or in `nixpkgs`.
    #[serde(default)]
    pub attr: Option<String>,
    pub out: Vec<String>,
    /// The derivation (`/nix/store/….drv`) that built `out`, when it is not from the pinned
    /// nixpkgs.
    #[serde(default)]
    pub drv: Option<String>,
    /// The included source file holding the nixpkgs tree that built it, when that is not the
    /// list's pinned nixpkgs.
    #[serde(default)]
    pub nixpkgs: Option<String>,
    /// The included source file holding the derivation's closure (fetch-sources.sh's
    /// `nix-recipe:`): every .drv file, every non-derivation input and `nix derivation show -r`.
    #[serde(default)]
    pub recipe: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl NixRef {
    /// The included files that hold this store path's build recipe: its nixpkgs tree (the pinned
    /// one unless `nixpkgs` names another) and its derivation closure, if recorded.
    pub fn recipe_files<'a>(&'a self, list: &'a List) -> Vec<&'a str> {
        let mut v = vec![self
            .nixpkgs
            .as_deref()
            .unwrap_or(list.nixpkgs.archive.as_str())];
        if let Some(r) = &self.recipe {
            v.push(r);
        }
        v
    }
}

impl Component {
    /// The version as shown, with `@release` replaced.
    pub fn version_for(&self, release: &str) -> String {
        if self.version == "@release" {
            release.to_string()
        } else {
            self.version.clone()
        }
    }
    pub fn covers(&self, path: &str) -> bool {
        self.files.iter().any(|p| crate::glob::matches(p, path))
    }
    /// The included files that hold the build recipes of its Nix-built store paths, each once.
    pub fn recipe_files<'a>(&'a self, list: &'a List) -> Vec<&'a str> {
        let mut v: Vec<&str> = Vec::new();
        for f in self.nix.iter().flat_map(|n| n.recipe_files(list)) {
            if !v.contains(&f) {
                v.push(f);
            }
        }
        v
    }
    pub fn included_sources(&self) -> impl Iterator<Item = (&str, &str)> {
        self.sources
            .iter()
            .filter_map(|s| Some((s.file.as_deref()?, s.sha256.as_deref().unwrap_or(""))))
    }
}

impl List {
    pub fn load(repo: &Path, p: Platform) -> Result<List, String> {
        let path = repo.join(p.list_path());
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let l: List =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if l.format != 1 {
            return Err(format!(
                "{}: format {} (this tool reads format 1)",
                path.display(),
                l.format
            ));
        }
        if l.platform != p.name() {
            return Err(format!(
                "{}: platform {:?}, expected {:?}",
                path.display(),
                l.platform,
                p.name()
            ));
        }
        Ok(l)
    }
    /// Whether any component or Rust program claims a release path.
    pub fn covers(&self, path: &str) -> bool {
        self.components.iter().any(|c| c.covers(path))
            || self
                .rust
                .iter()
                .any(|r| r.files.iter().any(|p| crate::glob::matches(p, path)))
    }
    /// Every store path some component's `nix` entries name.
    pub fn store_paths(&self) -> std::collections::BTreeSet<&str> {
        self.components
            .iter()
            .flat_map(|c| c.nix.iter().flat_map(|n| n.out.iter().map(|s| s.as_str())))
            .collect()
    }
}

/// A component's texts as files: an entry ending in `/` is a directory of the repository and
/// stands for every file below it (sorted).
pub fn expand_texts(repo: &Path, texts: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for t in texts {
        if let Some(dir) = t.strip_suffix('/') {
            for f in crate::notices::walk(&repo.join(dir)) {
                v.push(format!("{dir}/{f}"));
            }
        } else {
            v.push(t.clone());
        }
    }
    v
}

/// Where the license texts of a component go in the release, for a repository path: the files
/// under `hydo/dist/licenses/` keep their path below it; the repository's `LICENSE` is the
/// release's `LICENSE`; any other repository file goes below `licenses/hydo/`.
pub fn text_release_path(repo_path: &str) -> String {
    if let Some(rest) = repo_path.strip_prefix("hydo/dist/licenses/") {
        format!("licenses/{rest}")
    } else if repo_path == "LICENSE" {
        "LICENSE".to_string()
    } else {
        format!("licenses/hydo/{repo_path}")
    }
}

/// Where a pinned asset (a path of `hydo/dist/<platform>/assets/`) lies in the release tree, as
/// the platform's build-release.sh places it (the same mapping as hydo-cli's
/// `setup::asset_release_path`, which `hydo setup` uses).
pub fn asset_release_path(p: Platform, asset: &str) -> Option<String> {
    let a = asset.strip_prefix("./").unwrap_or(asset);
    match p {
        Platform::Linux => {
            if a.starts_with("guest/") || a.starts_with("tools/") {
                Some(a.to_string())
            } else {
                a.strip_prefix("redis/")
                    .map(|f| format!("examples/redis/files/{f}"))
            }
        }
        Platform::Mac => {
            if let Some(f) = a.strip_prefix("redis/") {
                return Some(format!("examples/redis/files/{f}"));
            }
            if let Some(f) = a.strip_prefix("ledger-spin/") {
                return Some(format!("examples/ledger-spin/bin/{f}"));
            }
            if let Some(f) = a.strip_prefix("tc/").or_else(|| a.strip_prefix("netns/")) {
                return Some(format!("guest/tc/{f}"));
            }
            match a {
                "Image-explore" | "Image-explore-netns" => Some(format!("hv/hvm/explore/out/{a}")),
                "busybox"
                | "mke2fs"
                | "hydo_fc"
                | "busybox-applets.txt"
                | "tc-closure.txt"
                | "netns-closure.txt" => Some(format!("guest/{a}")),
                _ => None,
            }
        }
    }
}

/// The paths of a `sha256sum` file (`<hex>  <path>`).
pub fn sums_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(_, p)| p.trim_start_matches([' ', '*']))
        })
        .map(|p| p.strip_prefix("./").unwrap_or(p).to_string())
        .collect()
}

/// The repository root of this crate's checkout.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

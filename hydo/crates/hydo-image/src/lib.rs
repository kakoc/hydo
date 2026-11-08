#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! hydo's image record (Milestone 155): every image records what shapes its runs but is not
//! inside it, so an image built today still runs, and reproduces, after hydo is upgraded.
//!
//! Two records, both version 1 of one format (`"v": 1`, a `"type"`; additions only within the
//! version, readers ignore fields they do not know; the schema `hydo/schema/image-v1.json` is
//! generated from these types and checked against `hydo/schema/released/image-v1.json` by
//! `tests/schema.rs`):
//!
//! - **Inside the image**, `/etc/hydo/image.json` ([`ImageRecord`], `"type": "image"`, canonical
//!   JSON: sorted keys, no spaces, one newline): the guest kernel the image was built for, by
//!   sha256 (with its file name, Linux version and the kernel command line hydo boots it with,
//!   for people), the x86_64 initramfs the same way, the architecture, and the **fingerprint
//!   epoch** ([`EPOCH`]). Nothing that changes with every hydo release is in it (a version or a
//!   commit there would move every fingerprint at every release), so two hydo versions of one
//!   epoch build the same image bytes.
//! - **Next to the image**, `<image file>.json` ([`BuildRecord`], `"type": "build"`, written by
//!   `hydo build`): the image file's and the data disk's sha256, a copy of the image record, the
//!   hydo version and commit that built it, the manifest's path and sha256, and the service
//!   images' digests. Every run's `result.json` carries the same facts (the results format's
//!   `image` and the running hydo's version, commit and epoch).
//!
//! **The fingerprint epoch.** A fingerprint is a function of the image's bytes and of what the
//! engine and the host controller do with them. [`EPOCH`] numbers the second part: it starts at 1
//! (Milestone 155, which re-baselined every example) and must be increased by one in the same
//! commit as any change of an engine, the host controller, the agent pin or the guest kernels that
//! moves a recorded fingerprint, that is, at every re-baseline (docs/REBASELINE.md gets a section
//! naming the new epoch). Runs of one epoch compare fingerprints exactly; across epochs only the
//! verdicts are comparable, and `hydo repro` says so ([`comparable`]). A run, an exploration or a
//! cache entry written before Milestone 155 has no epoch and counts as epoch 1.
//!
//! **The kernel an image names** ([`kernels`]): the engines boot the kernel whose sha256 the image
//! records, found among the kernels this hydo has (a release's `guest/` kernels, a tree's build
//! outputs); a missing one is refused with a message that names it, its version and the hydo that
//! built the image. [`kernels::obtain`] asks the source a program registers: the `hydo` command's
//! install cache (Milestone 162), which uses this crate's [`store`] (the cache's layout and
//! content store) and [`releases`] (the release index: which release shipped which kernel).
//! An image without the record (built before Milestone 155) boots today's default kernel, with a
//! one-line warning.

pub mod kernels;
pub mod releases;
pub mod store;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The version of both records this crate writes.
pub const VERSION: u64 = 1;

/// The fingerprint epoch of this hydo (see the crate documentation: increase it at every
/// re-baseline).
pub const EPOCH: u64 = 1;

/// The epoch of a run, exploration or cache entry that recorded none (written before M155).
pub const EPOCH_BEFORE_RECORDS: u64 = 1;

/// The image record's path inside the image's root (without the leading slash).
pub const RECORD_PATH: &str = "etc/hydo/image.json";

/// The hydo version this crate is built with (the workspace version, `hydo/VERSION`).
pub fn hydo_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The commit this hydo is built from (build.rs: `hydo/COMMIT`, else git), if known.
pub fn hydo_commit() -> Option<&'static str> {
    Some(env!("BUILT_FROM_COMMIT")).filter(|s| !s.is_empty())
}

/// The version field of both records: always 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V1;

impl Serialize for V1 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(1)
    }
}

impl<'de> Deserialize<'de> for V1 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<V1, D::Error> {
        let n = u64::deserialize(d)?;
        if n == 1 {
            Ok(V1)
        } else {
            Err(serde::de::Error::custom(format!(
                "image record version {n}, this hydo reads version 1 (a newer hydo wrote it)"
            )))
        }
    }
}

impl JsonSchema for V1 {
    fn schema_name() -> Cow<'static, str> {
        "Version1".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({"type": "integer", "const": 1})
    }
}

macro_rules! tag {
    ($name:ident, $lit:literal) => {
        #[doc = concat!("The `type` of this record: always `\"", $lit, "\"`.")]
        #[derive(
            Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
        )]
        pub enum $name {
            #[default]
            #[serde(rename = $lit)]
            Tag,
        }
    };
}

tag!(ImageType, "image");
tag!(BuildType, "build");

/// The guest kernel an image was built for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Kernel {
    /// The kernel's file name as hydo ships it (`bzImage`, `Image-explore`, `Image-explore-netns`).
    pub file: String,
    /// SHA-256 of the kernel file: the identity a run looks the kernel up by.
    pub sha256: String,
    /// The Linux version, for people (`6.6.35`).
    pub version: String,
    /// The kernel command line hydo boots an agent image with, for people (the engine is the
    /// authority; the hvx engine adds `hydo.hc=1` when the module offers the host-call port).
    pub cmdline: String,
}

/// A file by name and sha256.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileRef {
    pub file: String,
    pub sha256: String,
}

/// `/etc/hydo/image.json`: what an image needs from the hydo that runs it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageRecord {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: ImageType,
    /// `x86_64` or `arm64`.
    pub arch: String,
    /// The fingerprint epoch of the hydo that built the image ([`EPOCH`]).
    pub epoch: u64,
    pub kernel: Kernel,
    /// x86_64: the initramfs the kernel starts first (it mounts the image's root and starts the
    /// agent); arm64 images are their own initramfs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initrd: Option<FileRef>,
}

/// The hydo that built an image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hydo {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// The manifest an image was built from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// The path as given to `hydo build`.
    pub path: String,
    /// SHA-256 of that file.
    pub sha256: String,
}

/// A service built from a Docker/OCI image (`services[].image:`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceImage {
    /// The service's name.
    pub service: String,
    /// The image reference as the manifest gives it.
    pub image: String,
    /// The image manifest's digest (`sha256:...`), when the image was read by one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The image configuration's digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_digest: Option<String>,
}

/// `<image file>.json`: how and from what an image was built (written by `hydo build`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildRecord {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: BuildType,
    /// The image file (an x86_64 `*-disk.img`, an arm64 `*.cpio.gz`).
    pub image: FileRef,
    /// arm64: the data disk next to the initramfs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<FileRef>,
    /// The record inside the image.
    pub record: ImageRecord,
    pub hydo: Hydo,
    pub manifest: Manifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceImage>,
}

/// Either record (the schema's root).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AnyV1 {
    Image(Box<ImageRecord>),
    Build(Box<BuildRecord>),
}

/// The JSON Schema of both records (the committed copy is `hydo/schema/image-v1.json`).
pub fn schema() -> Value {
    let s = schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<AnyV1>();
    let mut v = serde_json::to_value(s).expect("a schema serializes");
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "title".into(),
            Value::String("hydo image records, version 1".into()),
        );
    }
    v
}

fn sorted(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut kv: Vec<(String, Value)> = m.into_iter().collect();
            kv.sort_by(|a, b| a.0.cmp(&b.0));
            let mut o = Map::new();
            for (k, x) in kv {
                o.insert(k, sorted(x));
            }
            Value::Object(o)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        x => x,
    }
}

/// Canonical JSON: sorted keys, no spaces, one newline (the bytes of `/etc/hydo/image.json`).
pub fn canonical<T: Serialize>(rec: &T) -> String {
    let v = serde_json::to_value(rec).expect("an image record serializes");
    let mut s = serde_json::to_string(&sorted(v)).expect("a value serializes");
    s.push('\n');
    s
}

/// An indented document and a newline (the bytes of `<image>.json`).
pub fn document<T: Serialize>(rec: &T) -> String {
    let mut s = serde_json::to_string_pretty(rec).expect("a build record serializes");
    s.push('\n');
    s
}

/// Read an image record (unknown fields ignored; another version or type refused).
pub fn parse_record(text: &str) -> Result<ImageRecord, String> {
    serde_json::from_str(text).map_err(|e| format!("/{RECORD_PATH}: {e}"))
}

/// Read a build record.
pub fn parse_build(text: &str) -> Result<BuildRecord, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// The build record's path for an image: the image's file name plus `.json`.
pub fn sidecar_path(image: &Path) -> PathBuf {
    let mut s = image.as_os_str().to_os_string();
    s.push(".json");
    PathBuf::from(s)
}

/// The build record next to an image, if there is a readable one.
pub fn read_sidecar(image: &Path) -> Option<BuildRecord> {
    let t = std::fs::read_to_string(sidecar_path(image)).ok()?;
    parse_build(&t).ok()
}

/// How a recorded run compares with a run of this hydo ([`comparable`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    /// The same epoch: the fingerprints must be equal.
    Fingerprints,
    /// Another epoch: only the verdicts can be compared.
    Verdicts { recorded: u64, now: u64 },
}

/// How a run recorded at `recorded` (None: before M155, [`EPOCH_BEFORE_RECORDS`]) compares with
/// a run of this hydo.
pub fn comparable(recorded: Option<u64>) -> Compare {
    let r = recorded.unwrap_or(EPOCH_BEFORE_RECORDS);
    if r == EPOCH {
        Compare::Fingerprints
    } else {
        Compare::Verdicts {
            recorded: r,
            now: EPOCH,
        }
    }
}

/// The plain sentence `hydo repro` prints when the epochs differ.
pub fn other_epoch_note(recorded: u64, now: u64) -> String {
    format!(
        "the recorded fingerprints are from fingerprint epoch {recorded} and this hydo ({}) is at \
         epoch {now}: fingerprints from another epoch are not comparable, so the verdicts are \
         compared instead",
        hydo_version()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_and_reads_back() {
        let r = kernels::record_for("x86_64", false);
        let t = canonical(&r);
        assert!(t.starts_with("{\"arch\":\"x86_64\",\"epoch\":1,\"initrd\":{"));
        assert!(t.ends_with("}\n") && !t.contains(": "));
        assert_eq!(parse_record(&t).unwrap(), r);
        let mut v: Value = serde_json::from_str(&t).unwrap();
        v["new_field"] = Value::Bool(true);
        assert_eq!(
            parse_record(&v.to_string()).unwrap(),
            r,
            "unknown fields ignored"
        );
        v["v"] = 2.into();
        assert!(parse_record(&v.to_string())
            .unwrap_err()
            .contains("version 2"));
    }

    #[test]
    fn epochs() {
        assert_eq!(comparable(None), Compare::Fingerprints);
        assert_eq!(comparable(Some(EPOCH)), Compare::Fingerprints);
        assert_eq!(
            comparable(Some(EPOCH + 1)),
            Compare::Verdicts {
                recorded: EPOCH + 1,
                now: EPOCH
            }
        );
        assert!(other_epoch_note(1, 2).contains("not comparable"));
    }

    #[test]
    fn sidecar_names() {
        assert_eq!(
            sidecar_path(Path::new("b/explore-ob-redis-disk.img")),
            PathBuf::from("b/explore-ob-redis-disk.img.json")
        );
    }
}

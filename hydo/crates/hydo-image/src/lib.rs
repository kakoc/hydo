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
/// re-baseline). Epoch 2 (Milestone 204): `guest.cpu_cache` and `root.fix_pyc` on when absent,
/// and the KVM engine's full-state cycles and fingerprint without KVM's leftover event fields
/// ([`defaults`]). Epoch 3 (Milestone 234): the Mac and KVM engines' guest kernels with cgroups
/// (`Image-explore-cg`, `Image-explore-netns-cg`, `bzImage-kvm-cg`, `bzImage-kvm-smp-cg`;
/// docs/design/guest-cgroups.md), so a pause of threads runs on every engine. Epoch 4 (Milestone
/// 269): the saved regressions leave the image for `<image>.regressions.json` beside it
/// ([`regressions_path`]), so saving or editing one no longer changes the image's bytes, and
/// (Milestone 283) the host controller mounts cgroup2 on `/sys/fs/cgroup` at every boot, and
/// (Milestone 290) the Mac and KVM engines' fingerprints hash the disks by a tree of block hashes.
/// Epoch 5 (Milestone 317): an image service's launcher mounts its system folders once per boot,
/// under the lock of its `mounts:` ([`EpochDefaults::oci_mounts_once`]), and (Milestone 323) hvx
/// boots an x86_64 image's guest with its TSC as the clock ([`EpochDefaults::exact_tick`]), and
/// (Milestone 331) each image service has a `/tmp` of its own ([`EpochDefaults::oci_own_tmp`]),
/// and (Milestone 338) an arm64 root keeps its hard links ([`EpochDefaults::cpio_hard_links`]),
/// an image service sees the guest's cgroup2 files ([`EpochDefaults::oci_sys_rbind`]), and a
/// timed pause of threads finds the workload as a reaction's does
/// ([`EpochDefaults::pause_largest_session`]). Epoch 6 (Milestone 353): the recovery boot after a
/// power cut starts with the first boot's last outcomes in `$HYDO_OUTCOMES`
/// ([`EpochDefaults::outcomes_after_cut`]), and (Milestone 348) every command that the host
/// controller starts gets a `PATH` ([`EpochDefaults::command_path`]).
pub const EPOCH: u64 = 6;

/// The epoch of a run, exploration or cache entry that recorded none (written before M155).
pub const EPOCH_BEFORE_RECORDS: u64 = 1;

/// What an image's own fingerprint epoch decides at run time (Milestone 204). The defaults that
/// changed with an epoch are taken from the epoch the image was built at, so an image built
/// before keeps running exactly as it did. An image without a record counts as
/// [`EPOCH_BEFORE_RECORDS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochDefaults {
    /// What an absent `guest.cpu_cache` means for an x86_64 image: on from epoch 2.
    pub cpu_cache: bool,
    /// The KVM engine proves loops that store by the whole vCPU state (`HYDO_KVM_SPIN_STATE`,
    /// Milestone 194) unless the variable says otherwise: from epoch 2.
    pub kvm_full_state: bool,
    /// The KVM fingerprint hashes what KVM keeps of an exception or interrupt it has finished
    /// with (the leftovers of `KVM_GET_VCPU_EVENTS`, which depend on the host): before epoch 2.
    pub kvm_stale_events: bool,
    /// The Mac and KVM engines boot the image with a guest kernel that has cgroups, so a pause of
    /// threads (`P=workload/<thread>@...`, Milestone 226) runs there: from epoch 3 (Milestone
    /// 234). hvx's guest kernel has always had them.
    pub cgroups: bool,
    /// The image carries the plan's saved regressions: the folder `/hydo/regressions/`, their
    /// entries in `/etc/hydo/compose.json` and their compositions in `/etc/hydo/compose.saved/`.
    /// Before epoch 4. From epoch 4 (Milestone 269) they are in `<image>.regressions.json`
    /// ([`regressions_path`]), which only the host reads.
    pub regressions_in_image: bool,
    /// What an absent `guest.cgroup2` means: the host controller mounts the cgroup2 file system
    /// on `/sys/fs/cgroup` at the start of every boot, as a container runtime does, so a program
    /// that reads `/proc/self/cgroup` at start (Seastar, Dragonfly) finds its `0::/` line. From
    /// epoch 4 (Milestone 283).
    pub cgroup2: bool,
    /// The Mac and KVM engines' fingerprints hash each disk by a tree of block hashes, so a
    /// fingerprint hashes again only the blocks that the run wrote (`hydo_hvm::disk`,
    /// `Disk::tree_hash`), instead of every byte with byte-wise FNV. From epoch 4 (Milestone 290).
    /// hvx's disk is in the kernel module, whose fingerprint does not change.
    pub disk_tree: bool,
    /// The Mac and KVM engines' fingerprints hash guest RAM by a tree of 4 KiB page hashes, on
    /// several threads (`hydo_hvm::ramtree`), instead of one chain of the word-wise mix over
    /// every byte. The KVM engine gives the pages it did not write since its base their known
    /// hashes. From epoch 4 (Milestone 294). hvx's RAM hash is in the kernel module.
    pub ram_tree: bool,
    /// hydo-build writes each image service's launcher (`/etc/hydo/oci/<service>/run`) and its
    /// `hydo-oci` helper (`exec`) so that they bind `/proc`, `/sys`, `/dev`, `/tmp` and `/data`
    /// into the service's root once per boot, under the lock that guards the service's
    /// `mounts:`, deciding by `/proc/mounts`, before the `mounts:`. Before, each bound them on
    /// every start outside the lock when busybox's `mountpoint` said no, so a readiness check
    /// that started with the launcher could bind `/dev` a second time over the service's
    /// `/dev/shm` tmpfs (open item 162). From epoch 5 (Milestone 317). It decides the bytes of a
    /// new image only: an image built before keeps its launcher inside it.
    pub oci_mounts_once: bool,
    /// hydo-build writes each image service's launcher and `hydo-oci` helper so that the
    /// service's `/tmp` is its own, as a container's is: the guest's folder `/tmp/oci/<service>`,
    /// with the guest's `/tmp/hydo` bound at its `/tmp/hydo` (where `$HYDO_ACKED` and the other
    /// run files are) and the guest's `/tmp` at `/.hydo/tmp`. Before, every image service had
    /// the guest's `/tmp` itself, so two services of one image met each other's files there: a
    /// program that locks a file named after its port in `/tmp` (TiKV) found the lock of its
    /// sibling (open item 147, which `root.share_images` was blamed for). From epoch 5
    /// (Milestone 331). It decides the bytes of a new image only.
    pub oci_own_tmp: bool,
    /// What an absent `guest.exact_tick` means for an x86_64 image: hydo-build writes
    /// `/etc/hydo/exact-tick` with `tsc`, and the hvx engine boots the guest kernel with
    /// `tsc=reliable` and a virtual TSC that moves 1 us per step. The guest's clocks are then the
    /// virtual TSC, read in steps of 2 us. Before, the kernel found the TSC unstable at boot and
    /// kept time with refined-jiffies, whose ticks part from the timer by 152 ns each, so a busy
    /// guest's clock nearly stopped late in a long run (Milestone 319). From epoch 5 (Milestone
    /// 323). It decides the bytes of a new image only: an image built before has no such file
    /// and boots as it did. The Mac and KVM engines ignore the file.
    pub exact_tick: bool,
    /// hydo-build writes the arm64 root's newc archive (the initramfs, and the archive that the
    /// root of `guest.root: disk` is unpacked from) with the tree's hard links as links: one
    /// inode with its count of links, the bytes in its first entry only. Before, each path of a
    /// file was a whole copy, and the busybox image's 400 applets took 400 copies of busybox in
    /// the guest's RAM (open item 162). From epoch 5 (Milestone 338). It decides the bytes of a
    /// new image only. An x86_64 root is made by `mke2fs -d`, which has always kept them.
    pub cpio_hard_links: bool,
    /// hydo-build writes each image service's launcher and `hydo-oci` helper so that they bind
    /// the guest's `/sys` into the service's root recursively, so that the service's programs
    /// find the cgroup2 files under `/sys/fs/cgroup` beside their `0::/` line. Before, a plain
    /// bind left the mount of `/sys/fs/cgroup` out (open item 144). From epoch 5 (Milestone 338).
    /// It decides the bytes of a new image only.
    pub oci_sys_rbind: bool,
    /// The host controller's timed pause of threads (`P=workload/<thread>@...`) finds the
    /// workload's session as a reaction's freeze does, as the session with the most processes
    /// among the leaders with `HYDO_FROM`, and leaves out a thread that ended before its move
    /// into the cgroup. Before, it took the first such leader in the order of `/proc` (which can
    /// be a watcher of busybox's `timeout`) and ended the run with an ERROR at a thread that had
    /// ended (open item 80). From epoch 5 (Milestone 338). It changes the controller's commands,
    /// so an image built before keeps its transcripts.
    pub pause_largest_session: bool,
    /// What an absent `guest.path` means: the host controller gives every command it starts (the
    /// workload's, the checks', the services' of the guest's own root) `PATH` =
    /// [`COMMAND_PATH`], so a program that such a command starts finds the guest's programs,
    /// `hydo-oci` among them, by name. Before, the kernel gave init no `PATH`, busybox's shell
    /// exported none, and Go's `exec.Command("hydo-oci", ...)` failed with `executable file
    /// not found in $PATH` (open item 170). The variable is in each command's environment, so
    /// in guest RAM: it moves every fingerprint and control transcript. So it waited for the
    /// next fingerprint epoch (Milestone 348): on from epoch 6 (Milestone 353), and before it
    /// `guest.path: true` turns it on.
    pub command_path: bool,
    /// What an absent `guest.outcomes_after_cut` means: the host controller reads the first
    /// boot's `$HYDO_OUTCOMES` at a power cut and writes the last outcome of every id into the
    /// recovery boot's file, so a check that looks up the ids of `$HYDO_OUTCOMES` there sees what
    /// the client saw. Before, the recovery boot's file started empty, and such a check reported
    /// every acknowledged id lost or passed without checking anything (Milestone 184, open item
    /// 62). The controller reports the outcomes at the cut and sends the recovery boot one more
    /// command, the write of the file, which is then in guest RAM, so every run with a power cut
    /// gets another control transcript and fingerprint. From epoch 6 (Milestone 353).
    pub outcomes_after_cut: bool,
}

/// The `PATH` that `guest.path` gives the project's commands: busybox's own default.
pub const COMMAND_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The defaults of an image built at `epoch` for `arch` (`x86_64` or `arm64`).
pub fn defaults(epoch: u64, arch: &str) -> EpochDefaults {
    let two = epoch >= 2;
    EpochDefaults {
        cpu_cache: two && arch == "x86_64",
        kvm_full_state: two,
        kvm_stale_events: !two,
        cgroups: epoch >= 3,
        regressions_in_image: epoch < 4,
        cgroup2: epoch >= 4,
        disk_tree: epoch >= 4,
        ram_tree: epoch >= 4,
        oci_mounts_once: epoch >= 5,
        oci_own_tmp: epoch >= 5,
        exact_tick: epoch >= 5 && arch == "x86_64",
        cpio_hard_links: epoch >= 5,
        oci_sys_rbind: epoch >= 5,
        pause_largest_session: epoch >= 5,
        command_path: epoch >= 6,
        outcomes_after_cut: epoch >= 6,
    }
}

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
    /// The guest kernel variant the manifest asked for with `guest.kernel` (Milestone 256):
    /// `containers`, a kernel with what containers and Kubernetes need, on the Mac and KVM
    /// engines (`hydo_image::kernels::CONTAINERS`). Absent: the kernel of the image's epoch. On
    /// arm64 `kernel` names the variant's file too; on x86_64 `kernel` names hvx's kernel, which
    /// has those options already, and the KVM engine picks its build of the variant by this
    /// field. An image without the key has no field, so its record keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_variant: Option<String>,
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

/// The file name suffix of an image's saved regressions ([`regressions_path`]).
pub const REGRESSIONS_SUFFIX: &str = ".regressions.json";

/// The `type` of a regressions file.
pub const REGRESSIONS_TYPE: &str = "regressions";

/// The saved regressions of an image built at epoch 4 or later (Milestone 269): the image's file
/// name plus `.regressions.json`. `hydo build` writes it when the plan has saved regressions (the
/// folder `regressions/` or `test.regressions`), and removes a stale one when it has none. The
/// host reads it to replay them (`hydo repro --regressions`, `hydo ci run`) and for the
/// composition that a `W=@<key>` names. Nothing of it reaches the guest, so the image's bytes do
/// not depend on the regressions.
pub fn regressions_path(image: &Path) -> PathBuf {
    let mut s = image.as_os_str().to_os_string();
    s.push(REGRESSIONS_SUFFIX);
    PathBuf::from(s)
}

/// The text of a regressions file, `{"regressions": [...], "type": "regressions", "v": 1}`:
/// indented, sorted keys, one newline. The entries are the plan meta's (hydo-manifest's
/// `expand`), as an image built before epoch 4 carried them in `/etc/hydo/compose.json`.
pub fn regressions_text(regressions: &[Value]) -> String {
    let doc = serde_json::json!({
        "regressions": regressions,
        "type": REGRESSIONS_TYPE,
        "v": VERSION,
    });
    let mut s = serde_json::to_string_pretty(&sorted(doc)).expect("a value serializes");
    s.push('\n');
    s
}

/// The entries of a regressions file (another version or type is refused).
pub fn parse_regressions(text: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v.get("v") != Some(&Value::from(VERSION))
        || v.get("type") != Some(&Value::from(REGRESSIONS_TYPE))
    {
        return Err(format!(
            "not a regressions file of version {VERSION} (\"type\": \"{REGRESSIONS_TYPE}\")"
        ));
    }
    match v.get("regressions") {
        Some(Value::Array(a)) => Ok(a.clone()),
        _ => Err("\"regressions\" must be a list".into()),
    }
}

/// The saved regressions beside `image`; `Ok(None)` when there is no such file.
pub fn read_regressions(image: &Path) -> Result<Option<Vec<Value>>, String> {
    let p = regressions_path(image);
    match std::fs::read_to_string(&p) {
        Ok(t) => parse_regressions(&t)
            .map(Some)
            .map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
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
        assert!(t.starts_with("{\"arch\":\"x86_64\",\"epoch\":6,\"initrd\":{"));
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
        // a run without an epoch is from epoch 1, before this hydo's epoch (4 since M269)
        assert_eq!(
            comparable(None),
            Compare::Verdicts {
                recorded: EPOCH_BEFORE_RECORDS,
                now: EPOCH
            }
        );
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
    fn epoch_defaults() {
        // an image built at epoch 1 keeps every default it was built with (M204)
        let one = defaults(1, "x86_64");
        assert!(!one.cpu_cache && !one.kvm_full_state && one.kvm_stale_events);
        assert!(!one.cgroups);
        let two = defaults(2, "x86_64");
        assert!(two.cpu_cache && two.kvm_full_state && !two.kvm_stale_events && !two.cgroups);
        // arm64 has no CPUID
        assert!(!defaults(2, "arm64").cpu_cache);
        // epoch 3 (M234): the Mac and KVM guest kernels with cgroups; the rest as at epoch 2
        let three = defaults(3, "x86_64");
        assert_eq!(
            three,
            EpochDefaults {
                cgroups: true,
                ..two
            }
        );
        assert!(defaults(3, "arm64").cgroups);
        assert!(one.regressions_in_image && two.regressions_in_image);
        assert!(three.regressions_in_image);
        assert!(!one.cgroup2 && !two.cgroup2 && !three.cgroup2);
        assert!(!one.disk_tree && !two.disk_tree && !three.disk_tree);
        assert!(!one.ram_tree && !two.ram_tree && !three.ram_tree);
        // epoch 4 (M269, M283, M290, M294): the saved regressions beside the image, cgroup2
        // mounted at every boot, and the disks' and RAM's tree hashes; the rest as at epoch 3
        let four = defaults(4, "x86_64");
        assert_eq!(
            four,
            EpochDefaults {
                regressions_in_image: false,
                cgroup2: true,
                disk_tree: true,
                ram_tree: true,
                ..three
            }
        );
        assert!(defaults(4, "arm64").disk_tree);
        assert!(defaults(4, "arm64").ram_tree);
        assert!(defaults(4, "arm64").cgroup2);
        assert!(!defaults(4, "arm64").regressions_in_image);
        assert!(!one.oci_mounts_once && !two.oci_mounts_once && !three.oci_mounts_once);
        assert!(!four.oci_mounts_once);
        assert!(!one.oci_own_tmp && !two.oci_own_tmp && !three.oci_own_tmp && !four.oci_own_tmp);
        // epoch 5 (M317, M323): the image services' launchers mount the system folders once, and
        // hvx's guest keeps time with its TSC; the rest as at epoch 4
        assert!(!one.exact_tick && !two.exact_tick && !three.exact_tick && !four.exact_tick);
        for d in [one, two, three, four] {
            assert!(!d.cpio_hard_links && !d.oci_sys_rbind && !d.pause_largest_session);
        }
        let five = defaults(5, "x86_64");
        assert_eq!(
            five,
            EpochDefaults {
                oci_mounts_once: true,
                oci_own_tmp: true,
                exact_tick: true,
                cpio_hard_links: true,
                oci_sys_rbind: true,
                pause_largest_session: true,
                ..four
            }
        );
        assert!(defaults(5, "arm64").cpio_hard_links);
        assert!(defaults(5, "arm64").oci_mounts_once);
        assert!(defaults(5, "arm64").oci_own_tmp);
        // hvx runs x86_64 images only
        assert!(!defaults(5, "arm64").exact_tick);
        // Milestones 348 and 353: `guest.path` and `guest.outcomes_after_cut` are off at every
        // epoch up to 5, so an image built before epoch 6 keeps its runs
        for e in 1..=5 {
            for arch in ["x86_64", "arm64"] {
                assert!(!defaults(e, arch).command_path, "epoch {e} {arch}");
                assert!(!defaults(e, arch).outcomes_after_cut, "epoch {e} {arch}");
            }
        }
        // epoch 6 (M353): both on; the rest as at epoch 5
        let six = defaults(6, "x86_64");
        assert_eq!(
            six,
            EpochDefaults {
                command_path: true,
                outcomes_after_cut: true,
                ..five
            }
        );
        assert!(defaults(6, "arm64").command_path && defaults(6, "arm64").outcomes_after_cut);
        assert!(!defaults(6, "arm64").exact_tick);
        assert_eq!(defaults(EPOCH, "x86_64"), six);
    }

    #[test]
    fn regressions_files() {
        let img = Path::new("b/explore-ob-redis-disk.img");
        assert_eq!(
            regressions_path(img),
            PathBuf::from("b/explore-ob-redis-disk.img.regressions.json")
        );
        let regs = vec![serde_json::json!({
            "name": "lost", "schedule": "disk=none | run C=3", "expect": "violation"
        })];
        let t = regressions_text(&regs);
        assert!(
            t.starts_with("{\n  \"regressions\": [\n") && t.ends_with("}\n"),
            "{t}"
        );
        assert!(t.contains("\"type\": \"regressions\"") && t.contains("\"v\": 1"));
        assert_eq!(parse_regressions(&t).unwrap(), regs);
        assert!(parse_regressions(&regressions_text(&[]))
            .unwrap()
            .is_empty());
        let newer = "{\"v\": 2, \"type\": \"regressions\", \"regressions\": []}";
        assert!(parse_regressions(newer).unwrap_err().contains("version 1"));
        assert!(parse_regressions("{\"v\": 1, \"type\": \"build\"}").is_err());
    }

    #[test]
    fn sidecar_names() {
        assert_eq!(
            sidecar_path(Path::new("b/explore-ob-redis-disk.img")),
            PathBuf::from("b/explore-ob-redis-disk.img.json")
        );
    }
}

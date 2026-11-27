//! The KVM engine (`hydo/crates/hydo-kvm`, docs/design/kvm-engine.md): x86_64 agent images on
//! stock Linux KVM (`/dev/kvm`), on a thread of this process by default (KVM allows several VMs in
//! one process), or as the program `hydo-kvm` with `HYDO_KVM=process`:
//!
//! ```text
//! hydo-kvm [--ram-mib N] [--cpu v3] [--cpu-cache on] --control boot serve|one <bzImage-kvm> <initramfs> <disk> <outdir> ["<schedule>"]
//! ```
//!
//! It boots the same images as hvx (the image's disk and its recorded initramfs) with its own
//! kernel, the KVM build of the image's recorded kernel ([`boot_files`], `hydo_kvm::kernel`), and
//! speaks the Mac engine's lock-step protocol with the exit count as its clock. Its fingerprints
//! are its own.

use crate::process::{ControlledBoot, ControlledRun};
use crate::{BootSpec, ControlledEngine, EngineError, EngineKind};
use hydo_image::kernels::{locate, Role, X86_INITRD, X86_KERNEL};
use hydo_image::FileRef;
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How to start the KVM engine.
#[derive(Clone, Debug)]
pub struct Kvm {
    /// The engine program ([`program`]), used with `HYDO_KVM=process`.
    pub program: PathBuf,
    /// Environment for the engine (on top of `config` in this process, or of this process's
    /// environment for the program).
    pub env: Vec<(String, String)>,
    pub config: hydo_config::Env,
}

/// The program of a tree or a release at `root`: `$HYDO_KVM_ENGINE`, else the tree's
/// `target/release/hydo-kvm` when it is there, else the release's `bin/hydo-kvm`.
pub fn program(root: &Path, e: &hydo_config::kvm::Engine) -> PathBuf {
    if let Some(p) = &e.program {
        return p.clone();
    }
    let tree = root.join("target").join("release").join("hydo-kvm");
    if crate::hvx::is_executable(&tree) {
        return tree;
    }
    root.join("bin").join("hydo-kvm")
}

/// The files a KVM run boots an image with.
#[derive(Clone, Debug)]
pub struct KvmBoot {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    /// A line for the user (an image without a record, a kernel given by HYDO_KVM_KERNEL).
    pub note: Option<String>,
}

/// The KVM kernel and the initramfs for `image`: the image's record names its kernel and
/// initramfs by sha256 (M155; an image without one: the x86_64 defaults); the initramfs is looked
/// up by sha256 in `dirs`, and the kernel is the KVM build paired with the recorded kernel
/// (`hydo_kvm::kernel::twin_for`) for the image's fingerprint epoch (the build with cgroups from
/// epoch 3 on, M234, and with `/proc/<pid>/io` from epoch 7 on, M376, unless the record says
/// `proc_io: false`) and kernel variant (`guest.kernel: containers`, M256), looked up by its own
/// sha256 in `dirs`, unless `given` names one.
pub fn boot_files(image: &Path, dirs: &[PathBuf], given: Option<&Path>) -> Result<KvmBoot, String> {
    boot_files_n(image, dirs, given, 1)
}

/// [`boot_files`] for a machine of `vcpus` vCPUs: with more than one, the SMP build of the
/// image's kernel (`hydo_kvm::kernel::SMP_TWINS`, docs/design/kvm-vcpus.md).
pub fn boot_files_n(
    image: &Path,
    dirs: &[PathBuf],
    given: Option<&Path>,
    vcpus: u32,
) -> Result<KvmBoot, String> {
    let rec = crate::image::image_record(image)?;
    let mut note = None;
    let (kref, iref) = match &rec {
        Some(r) => (
            r.kernel.sha256.clone(),
            r.initrd.clone().unwrap_or(FileRef {
                file: X86_INITRD.file.into(),
                sha256: X86_INITRD.sha256.into(),
            }),
        ),
        None => {
            note = Some(hydo_image::kernels::no_record_warning(image));
            (
                X86_KERNEL.sha256.to_string(),
                FileRef {
                    file: X86_INITRD.file.into(),
                    sha256: X86_INITRD.sha256.into(),
                },
            )
        }
    };
    if rec.as_ref().is_some_and(|r| r.arch != "x86_64") {
        return Err(format!(
            "{}: an {} image; the KVM engine runs x86_64 images",
            image.display(),
            rec.as_ref().map(|r| r.arch.as_str()).unwrap_or("")
        ));
    }
    let shown: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    let initrd = locate(&iref, Role::Initrd, dirs).ok_or_else(|| {
        format!(
            "{} needs the initramfs {} (sha256 {}), which is not in {}",
            image.display(),
            iref.file,
            iref.sha256,
            shown.join(", ")
        )
    })?;
    let kernel = match given {
        Some(k) => {
            note = Some(format!(
                "the KVM engine boots {} (HYDO_KVM_KERNEL), not checked against the image",
                k.display()
            ));
            k.to_path_buf()
        }
        None => {
            let epoch = rec
                .as_ref()
                .map_or(hydo_image::EPOCH_BEFORE_RECORDS, |r| r.epoch);
            let variant = rec.as_ref().and_then(|r| r.kernel_variant.as_deref());
            let io = rec.as_ref().is_some_and(|r| r.proc_io());
            let t =
                hydo_kvm::kernel::twin_for(&kref, vcpus, epoch, variant, io).ok_or_else(|| {
                    match variant {
                        Some(v) => format!(
                        "{} was built for the guest kernel variant {v:?} (guest.kernel) at the \
                         fingerprint epoch {epoch}, which has no KVM build in this hydo \
                         (hydo_kvm::kernel::TWINS); this hydo knows the variants {:?}: rebuild \
                         the image with this hydo",
                        image.display(),
                        hydo_image::kernels::VARIANTS
                    ),
                        None => format!(
                        "{} was built for the guest kernel with sha256 {kref}, which has no KVM \
                         build in this hydo (hydo_kvm::kernel::TWINS); rebuild the image with \
                         this hydo",
                        image.display()
                    ),
                    }
                })?;
            let want = FileRef {
                file: t.file.into(),
                sha256: t.sha256.into(),
            };
            locate(&want, Role::Kernel, dirs).ok_or_else(|| {
                format!(
                    "the KVM engine's guest kernel {} (Linux {}, sha256 {}) is not in {}; build it \
                     with {}sh hv/kvm/linux/build-kernel.sh (docs/BUILD-PINS.md) or name it \
                     with HYDO_KVM_KERNEL",
                    t.file,
                    t.version,
                    t.sha256,
                    shown.join(", "),
                    t.build_vars(),
                )
            })?
        }
    };
    Ok(KvmBoot {
        kernel,
        initrd,
        note,
    })
}

impl Kvm {
    pub fn for_release(root: &Path, config: hydo_config::Env) -> Kvm {
        Kvm {
            program: program(root, &hydo_config::kvm::Engine::from_env(&config)),
            env: Vec::new(),
            config,
        }
    }

    fn engine_env(&self) -> hydo_config::Env {
        self.config
            .with(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }

    /// Whether the engine runs in this process (the default; `HYDO_KVM=process`: the program).
    pub fn in_process(&self) -> bool {
        hydo_config::kvm::Engine::from_env(&self.engine_env()).choice
            == hydo_config::kvm::Choice::InProcess
    }

    fn argv(&self, spec: &BootSpec, mode: &str, s: &Schedule) -> Vec<String> {
        let mut a = vec![self.program.display().to_string()];
        a.extend(spec.engine_args(EngineKind::Kvm));
        a.extend(["--control".into(), "boot".into(), mode.to_string()]);
        a.push(spec.kernel.display().to_string());
        a.push(
            spec.initrd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        a.push(spec.image.display().to_string());
        a.push(spec.outdir.display().to_string());
        a.push(s.to_string());
        a
    }

    fn command(&self, spec: &BootSpec, argv: &[String]) -> Command {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        // Milestone 255: the function contracts' plan next to the image, when it has one
        if let Some(p) = crate::hvm::contracts_for(&spec.image) {
            c.env(hydo_config::registry::HYDO_CONTRACTS, p);
        }
        // Milestone 263: the image's added disks (guest.disks)
        if let Some(d) = crate::image::guest_disks(&spec.image) {
            c.env(hydo_config::registry::HYDO_GUEST_DISKS, d);
        }
        // Milestone 274: the image's branch tick (guest.branch_tick)
        if let Some(p) = crate::image::branch_tick(&spec.image) {
            c.env(hydo_config::registry::HYDO_BRANCH_TICK, p);
        }
        c
    }

    fn check_built(&self) -> Result<(), EngineError> {
        if !crate::hvx::is_executable(&self.program) {
            return Err(EngineError::NotBuilt(self.program.clone()));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn spawn(
    k: &Kvm,
    spec: &BootSpec,
    one: bool,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::process::Started, EngineError> {
    use crate::protocol::{EngineMsg, HostMsg};
    use hydo_kvm::serve::{run, Mode, Start};
    let mut e = k.engine_env();
    // Milestone 255: the function contracts' plan next to the image, when it has one
    if let Some(p) = crate::hvm::contracts_for(&spec.image) {
        e = e.with([(
            hydo_config::registry::HYDO_CONTRACTS,
            p.display().to_string(),
        )]);
    }
    // Milestone 263: the image's added disks (guest.disks), as the engine program gets them
    if let Some(d) = crate::image::guest_disks(&spec.image) {
        e = e.with([(hydo_config::registry::HYDO_GUEST_DISKS, d)]);
    }
    // Milestone 274: the image's branch tick (guest.branch_tick)
    if let Some(p) = crate::image::branch_tick(&spec.image) {
        e = e.with([(hydo_config::registry::HYDO_BRANCH_TICK, p)]);
    }
    let o = hydo_config::kvm::Options::from_env(&e);
    let dev = hydo_config::kvm::Engine::from_env(&e).dev();
    let hv = hydo_kvm::sys::Kvm::open(&dev.display().to_string())
        .and_then(|k| k.with_level(level_of(spec.cpu)))
        .map(|k| k.with_cache(spec.cpu_cache))
        .map_err(EngineError::Unsupported)?;
    let image_epoch = Some(spec.epoch);
    let initrd = spec.initrd.clone().ok_or_else(|| {
        EngineError::Unsupported("the KVM engine needs the image's initramfs".into())
    })?;
    let start = Start {
        mode: if one {
            Mode::One(schedule.clone())
        } else {
            Mode::Serve {
                boot: Some(schedule.clone()),
            }
        },
        kernel: spec.kernel.clone(),
        initrd,
        disk: Some(spec.image.clone()),
        outdir: spec.outdir.clone(),
        ram_mib: Some(spec.ram_mib as u64),
        image_epoch,
        vcpus: spec.vcpus.max(1) as usize,
    };
    let diag: Box<dyn std::io::Write + Send> = Box::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?,
    );
    let (htx, hrx) = std::sync::mpsc::channel::<HostMsg>();
    let (etx, erx) = std::sync::mpsc::channel::<EngineMsg>();
    let port = hydo_hvx::port::ChanPort::new(hrx, etx);
    let j = std::thread::Builder::new()
        .name("kvm-engine".into())
        .spawn(move || run(start, &o, Box::new(port), diag, None, Box::new(hv)))
        .map_err(EngineError::Io)?;
    Ok(crate::process::Started {
        proc: crate::process::Proc::Thread(Some(j)),
        link: Box::new(crate::link::ChanLink::new(htx, erx)),
    })
}

#[cfg(not(target_os = "linux"))]
fn spawn(
    _k: &Kvm,
    _spec: &BootSpec,
    _one: bool,
    _schedule: &Schedule,
    _stderr_log: &Path,
) -> Result<crate::process::Started, EngineError> {
    Err(EngineError::Unsupported(
        "the KVM engine needs Linux (/dev/kvm)".into(),
    ))
}

/// The KVM engine's guest CPU level for `guest.cpu`.
pub fn level_of(cpu: crate::CpuLevel) -> hydo_kvm::cpuid::Level {
    match cpu {
        crate::CpuLevel::V2 => hydo_kvm::cpuid::Level::V2,
        crate::CpuLevel::V3 => hydo_kvm::cpuid::Level::V3,
    }
}

/// The machine the KVM engine gives a guest: at the image's CPU level (v2, or v3 on a host whose
/// KVM has AVX2: the engine refuses the run otherwise), with one vCPU or up to
/// `hydo_kvm::smp::MAX_VCPUS` (docs/design/kvm-vcpus.md; they need the SMP guest kernel). A
/// count beyond is refused rather than run on fewer.
fn check_machine(spec: &BootSpec) -> Result<(), EngineError> {
    if spec.vcpus == 0 || spec.vcpus as usize > hydo_kvm::smp::MAX_VCPUS {
        return Err(EngineError::Unsupported(format!(
            "the KVM engine runs 1 to {} vCPUs; {} asks for {}",
            hydo_kvm::smp::MAX_VCPUS,
            spec.image.display(),
            spec.vcpus,
        )));
    }
    Ok(())
}

impl ControlledEngine for Kvm {
    fn kind(&self) -> EngineKind {
        EngineKind::Kvm
    }

    fn start_controlled(
        &self,
        spec: &BootSpec,
        boot: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledBoot, EngineError> {
        check_machine(spec)?;
        if self.in_process() {
            return Ok(ControlledBoot::from_started(spawn(
                self, spec, false, boot, stderr_log,
            )?));
        }
        self.check_built()?;
        let argv = self.argv(spec, "serve", boot);
        ControlledBoot::start(self.command(spec, &argv), stderr_log)
    }

    fn one_controlled(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledRun, EngineError> {
        check_machine(spec)?;
        if self.in_process() {
            return Ok(ControlledRun::from_started(spawn(
                self, spec, true, schedule, stderr_log,
            )?));
        }
        self.check_built()?;
        let argv = self.argv(spec, "one", schedule);
        ControlledRun::start(self.command(spec, &argv), stderr_log)
    }

    /// A worker keeps guest RAM, its disk and S0 and S1 of both in memory.
    fn max_workers(&self, spec: &BootSpec) -> usize {
        let disk = std::fs::metadata(&spec.image)
            .map(|m| m.len() >> 20)
            .unwrap_or(0);
        crate::machine_workers((spec.ram_mib as u64 + disk) * 3 + 256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_twin_is_of_the_recorded_x86_kernel() {
        use hydo_kvm::kernel::{
            twin_for, twin_of, CURRENT, CURRENT_SMP, KERNEL, KERNEL_CG, KERNEL_CONTAINERS,
            KERNEL_CONTAINERS_IO, KERNEL_IO, KERNEL_SMP, KERNEL_SMP_CG, KERNEL_SMP_CONTAINERS,
            KERNEL_SMP_CONTAINERS_IO, KERNEL_SMP_IO,
        };
        let x = X86_KERNEL.sha256;
        assert_eq!(KERNEL.of, x);
        assert_eq!(KERNEL.version, X86_KERNEL.version);
        // what an image's record means: the builds with /proc/<pid>/io from epoch 7 (M376),
        // unless the record says proc_io: false
        let io = |e: u64| hydo_image::defaults(e, "x86_64").proc_io;
        // an image built at epoch 2 or before boots the builds it was recorded with; several
        // vCPUs: the SMP build of the same kernel
        for e in [1, 2] {
            assert_eq!(twin_for(x, 1, e, None, io(e)), Some(KERNEL));
            assert_eq!(twin_for(x, 2, e, None, io(e)), Some(KERNEL_SMP));
            assert_eq!(twin_of(x, e), Some(KERNEL));
        }
        // epochs 3 to 6 (M234): the builds with cgroups; from epoch 7 they boot only an image
        // whose record says proc_io: false
        for e in [3, 6] {
            assert_eq!(twin_for(x, 1, e, None, io(e)), Some(KERNEL_CG));
            assert_eq!(twin_for(x, 4, e, None, io(e)), Some(KERNEL_SMP_CG));
            assert_eq!(twin_of(x, e), Some(KERNEL_CG));
            // a record of epoch 6 never asks for the new builds, but none would be found
            assert_eq!(twin_for(x, 1, e, None, true), None);
        }
        for e in [7, hydo_image::EPOCH] {
            assert_eq!(twin_for(x, 1, e, None, io(e)), Some(KERNEL_IO));
            assert_eq!(twin_for(x, 4, e, None, io(e)), Some(KERNEL_SMP_IO));
            assert_eq!(twin_of(x, e), Some(KERNEL_IO));
            assert_eq!(twin_for(x, 1, e, None, false), Some(KERNEL_CG));
            assert_eq!(twin_for(x, 2, e, None, false), Some(KERNEL_SMP_CG));
        }
        assert_eq!((CURRENT, CURRENT_SMP), (KERNEL_IO, KERNEL_SMP_IO));
        assert_eq!(
            (KERNEL_CG.file, KERNEL_SMP_CG.file),
            ("bzImage-kvm-cg", "bzImage-kvm-smp-cg")
        );
        assert_eq!(
            (KERNEL_IO.file, KERNEL_SMP_IO.file),
            ("bzImage-kvm-io", "bzImage-kvm-smp-io")
        );
        assert_eq!(twin_of(&"ab".repeat(32), 3), None);
        // the variant `containers` (M256): its own builds, from epoch 3 only; another variant
        // has no build
        let c = Some(hydo_image::kernels::CONTAINERS);
        for e in [3, 6] {
            assert_eq!(twin_for(x, 1, e, c, io(e)), Some(KERNEL_CONTAINERS));
            assert_eq!(twin_for(x, 2, e, c, io(e)), Some(KERNEL_SMP_CONTAINERS));
            assert_eq!(twin_for(x, 1, e, Some("other"), io(e)), None);
        }
        for e in [7, hydo_image::EPOCH] {
            assert_eq!(twin_for(x, 1, e, c, io(e)), Some(KERNEL_CONTAINERS_IO));
            assert_eq!(twin_for(x, 2, e, c, io(e)), Some(KERNEL_SMP_CONTAINERS_IO));
            assert_eq!(twin_for(x, 1, e, c, false), Some(KERNEL_CONTAINERS));
            assert_eq!(twin_for(x, 1, e, Some("other"), io(e)), None);
        }
        assert_eq!(twin_for(x, 1, 2, c, false), None);
        assert_eq!(
            (KERNEL_CONTAINERS.file, KERNEL_SMP_CONTAINERS.file),
            ("bzImage-kvm-containers", "bzImage-kvm-smp-containers")
        );
        assert_eq!(
            (KERNEL_CONTAINERS_IO.file, KERNEL_SMP_CONTAINERS_IO.file),
            ("bzImage-kvm-containers-io", "bzImage-kvm-smp-containers-io")
        );
        assert_eq!(hydo_kvm::kernel::all().count(), 10);
        // the command that builds each, as the refusal names it
        assert_eq!(KERNEL.build_vars(), "");
        assert_eq!(KERNEL_SMP_CG.build_vars(), "CGROUPS=1 SMP=1 ");
        assert_eq!(KERNEL_SMP_CONTAINERS.build_vars(), "CONTAINERS=1 SMP=1 ");
        assert_eq!(KERNEL_IO.build_vars(), "PROCIO=1 ");
        assert_eq!(
            KERNEL_SMP_CONTAINERS_IO.build_vars(),
            "CONTAINERS=1 PROCIO=1 SMP=1 "
        );
    }

    #[test]
    fn the_program_command_line() {
        let k = Kvm {
            program: "/r/bin/hydo-kvm".into(),
            env: Vec::new(),
            config: hydo_config::Env::default(),
        };
        let spec = BootSpec {
            image: "/i/x-disk.img".into(),
            ram_mib: 2048,
            vcpus: 1,
            cpu: crate::CpuLevel::V2,
            cpu_cache: false,
            epoch: hydo_image::EPOCH,
            kernel: "/g/bzImage-kvm".into(),
            initrd: Some("/g/initramfs-kafka.cpio.gz".into()),
            outdir: "/o".into(),
            snapshots: crate::SnapshotStore::Memory,
        };
        let s = Schedule::parse("rb=64 | run").unwrap();
        assert_eq!(
            k.argv(&spec, "serve", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --control boot serve /g/bzImage-kvm \
             /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        assert!(k.in_process());
        assert!(check_machine(&spec).is_ok());
        let v3 = BootSpec {
            cpu: crate::CpuLevel::V3,
            ..spec.clone()
        };
        assert!(check_machine(&v3).is_ok());
        assert_eq!(
            k.argv(&v3, "one", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --cpu v3 --control boot one /g/bzImage-kvm \
             /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        // M203: guest.cpu_cache
        let cache = BootSpec {
            cpu_cache: true,
            ..v3.clone()
        };
        assert_eq!(
            k.argv(&cache, "one", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --cpu v3 --cpu-cache on --control boot one \
             /g/bzImage-kvm /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        // M204: an image built at fingerprint epoch 1 runs as epoch 1 did
        let old = BootSpec {
            epoch: 1,
            ..spec.clone()
        };
        assert_eq!(
            k.argv(&old, "one", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --image-epoch 1 --control boot one \
             /g/bzImage-kvm /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        // M290: every epoch before this hydo's is named, as the in-process engine gets it (an
        // image before epoch 4 hashes its disk byte by byte)
        let three = BootSpec {
            epoch: 3,
            ..spec.clone()
        };
        assert_eq!(
            k.argv(&three, "one", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --image-epoch 3 --control boot one \
             /g/bzImage-kvm /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        // several vCPUs (docs/design/kvm-vcpus.md): up to hydo_kvm::smp::MAX_VCPUS
        let two = BootSpec {
            vcpus: 2,
            ..spec.clone()
        };
        assert!(check_machine(&two).is_ok());
        assert_eq!(
            k.argv(&two, "one", &s).join(" "),
            "/r/bin/hydo-kvm --ram-mib 2048 --vcpus 2 --control boot one /g/bzImage-kvm \
             /g/initramfs-kafka.cpio.gz /i/x-disk.img /o rb=64 | run"
        );
        for cpu in [crate::CpuLevel::V2, crate::CpuLevel::V3] {
            let more = BootSpec {
                vcpus: 9,
                cpu,
                ..spec.clone()
            };
            assert!(check_machine(&more).is_err());
        }
    }
}

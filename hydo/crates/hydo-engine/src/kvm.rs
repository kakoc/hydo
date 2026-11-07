//! The KVM engine (`hydo/crates/hydo-kvm`, docs/design/kvm-engine.md): x86_64 agent images on
//! stock Linux KVM (`/dev/kvm`), on a thread of this process by default (KVM allows several VMs in
//! one process), or as the program `hydo-kvm` with `HYDO_KVM=process`:
//!
//! ```text
//! hydo-kvm [--ram-mib N] --control boot serve|one <bzImage-kvm> <initramfs> <disk> <outdir> ["<schedule>"]
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
/// (`hydo_kvm::kernel::twin_of`), looked up by its own sha256 in `dirs`, unless `given` names one.
pub fn boot_files(image: &Path, dirs: &[PathBuf], given: Option<&Path>) -> Result<KvmBoot, String> {
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
            let t = hydo_kvm::kernel::twin_of(&kref).ok_or_else(|| {
                format!(
                    "{} was built for the guest kernel with sha256 {kref}, which has no KVM build in \
                     this hydo (hydo_kvm::kernel::TWINS); rebuild the image with this hydo",
                    image.display()
                )
            })?;
            let want = FileRef {
                file: t.file.into(),
                sha256: t.sha256.into(),
            };
            locate(&want, Role::Kernel, dirs).ok_or_else(|| {
                format!(
                    "the KVM engine's guest kernel {} (Linux {}, sha256 {}) is not in {}; build it \
                     with hv/kvm/linux/build-kernel.sh (docs/BUILD-PINS.md) or name it with \
                     HYDO_KVM_KERNEL",
                    t.file,
                    t.version,
                    t.sha256,
                    shown.join(", ")
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

    fn command(&self, argv: &[String]) -> Command {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in &self.env {
            c.env(k, v);
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
    let e = k.engine_env();
    let o = hydo_config::kvm::Options::from_env(&e);
    let dev = hydo_config::kvm::Engine::from_env(&e).dev();
    let hv =
        hydo_kvm::sys::Kvm::open(&dev.display().to_string()).map_err(EngineError::Unsupported)?;
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

/// The machine the KVM engine gives every guest: one vCPU at the v2 level. An image that asks
/// for more (several vCPUs, `guest.cpu: v3`) is refused rather than run on less (an AVX2 program
/// would fault in the guest).
fn check_machine(spec: &BootSpec) -> Result<(), EngineError> {
    if spec.vcpus != 1 || spec.cpu != crate::CpuLevel::V2 {
        return Err(EngineError::Unsupported(format!(
            "the KVM engine runs one vCPU at the v2 CPU level; {} asks for {} vCPU(s) at {} \
             (several vCPUs and guest.cpu: v3 need the hvx engine)",
            spec.image.display(),
            spec.vcpus,
            match spec.cpu {
                crate::CpuLevel::V2 => "v2",
                crate::CpuLevel::V3 => "v3",
            }
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
        ControlledBoot::start(self.command(&argv), stderr_log)
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
        ControlledRun::start(self.command(&argv), stderr_log)
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
        assert_eq!(hydo_kvm::kernel::KERNEL.of, X86_KERNEL.sha256);
        assert_eq!(hydo_kvm::kernel::KERNEL.version, X86_KERNEL.version);
        assert!(hydo_kvm::kernel::twin_of(X86_KERNEL.sha256).is_some());
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
        for (vcpus, cpu) in [(2, crate::CpuLevel::V2), (1, crate::CpuLevel::V3)] {
            let more = BootSpec {
                vcpus,
                cpu,
                ..spec.clone()
            };
            assert!(check_machine(&more).is_err());
        }
    }
}

//! Worker pools over the hvx engine, one per image, as explore.py's `Pool(n, outdir)` started
//! them for run.py: `HYDO_BUILD` (default `~/hydo/build`) holds the images, the guest kernel
//! `bzImage` and `initramfs-kafka.cpio.gz`; the engine is the Rust engine in this process when it
//! can open the device, otherwise the program `hydo-hvx` (`hydo_engine::hvx::program`, or
//! `HYDO_HVX_EXPLORE`) started through `~/bin/hvx-run` (or `HYDO_HVX_RUN`, the bare-metal machine's shared
//! module lock); the values are the program's `hydo_config::Config` (M144). An agent image (its
//! `/etc/hydo/control` says `agent`, M121) is run through the host controller
//! (`hydo_control::agent::AgentEngine`, M126), as `hydo explore` runs one; an agent image the
//! controller cannot drive (for example one without an embedded manifest) is refused with the
//! controller's message (M129).

use hydo_engine::{hvx, BootSpec, ControlledEngine, CpuLevel, Engine, SnapshotStore};
use hydo_explore::pool::{Host, Pool};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What every pool of one invocation shares.
#[derive(Clone, Debug)]
pub struct Engines {
    pub root: PathBuf,
    pub build: PathBuf,
    pub vcpus: u32,
    /// explore.py's EXTRA_ENV (listrun.py `--env KEY=VAL`).
    pub extra_env: Vec<String>,
    /// The configuration the program read at its start.
    pub config: hydo_config::Config,
}

impl Engines {
    pub fn new(
        root: &Path,
        vcpus: u32,
        extra_env: Vec<String>,
        config: &hydo_config::Config,
    ) -> Engines {
        Engines {
            root: root.to_path_buf(),
            build: config.paths.build.clone(),
            vcpus,
            extra_env,
            config: config.clone(),
        }
    }

    /// explore.py `engine_args()` as one string (the result cache's key): `--vcpus N` above 1.
    pub fn engine_args(&self) -> String {
        if self.vcpus > 1 {
            format!("--vcpus {}", self.vcpus)
        } else {
            String::new()
        }
    }

    /// Start `workers` workers of `image` writing into `outdir` (consoles in `outdir/consoles`,
    /// `outdir/worker-<i>.err`). Each boots in its own thread; jobs wait for the boot.
    pub fn pool(&self, image: &str, workers: usize, outdir: &Path) -> Pool {
        let engine = hvx::Hvx {
            engine: hvx::program(&self.root, &self.config.engine),
            hvx_run: Some(self.config.paths.hvx_run.clone()),
            extra_env: self.extra_env.clone(),
            env: hvx::wrapper_env(&self.config.engine),
            config: self.config.env.clone(),
        };
        let (ram_mib, cpu, cpu_cache) = guest_of(&self.build.join(image));
        // the kernel and initramfs the image's record names (M155); an image without the record
        // boots the build directory's, with a warning
        let img = self.build.join(image);
        let b = hydo_engine::image::boot_files(
            &img,
            &self.build.join("bzImage"),
            Some(&self.build.join("initramfs-kafka.cpio.gz")),
            None,
            &[],
        )
        .unwrap_or_else(|e| {
            eprintln!("hydo-bench: {e}");
            std::process::exit(2)
        });
        if let Some(n) = &b.note {
            eprintln!("hydo-bench: {n}");
        }
        let spec = BootSpec {
            image: img,
            ram_mib,
            vcpus: self.vcpus,
            cpu,
            cpu_cache,
            epoch: hydo_engine::image::image_epoch(&self.build.join(image)).0,
            kernel: b.kernel,
            initrd: b.initrd,
            outdir: outdir.join("consoles"),
            snapshots: SnapshotStore::Files,
        };
        let _ = std::fs::create_dir_all(outdir);
        let host = Host {
            outdir: outdir.to_path_buf(),
            onboard: self.root.join("hydo").join("onboard"),
            known: None,
            pool: self.config.pool.clone(),
        };
        let engine = agent_engine(Arc::new(engine), &spec.image, &self.config.control);
        Pool::new(
            engine,
            spec,
            workers.max(1),
            host,
            image.to_string(),
            self.engine_args(),
        )
    }
}

/// The guest's RAM, CPU level and CPU cache leaves from the manifest an image carries
/// (`guest.ram_mib`, `guest.cpu`, `guest.cpu_cache`, as `hydo run` and the explorer pick them;
/// M136: `list` on the onboarding examples needs them, the benchmark's own manifest has the
/// defaults); 1024 MiB and v2 for an image without the keys, and the cache leaves as the image's
/// fingerprint epoch decides (M204). Read with the plan meta, as the host controller reads it
/// (M207: without it a composed plan's manifest did not read and the pool booted without the
/// cache leaves); a manifest that cannot be read ends the program.
fn guest_of(image: &Path) -> (u32, CpuLevel, bool) {
    let g = hydo_control::agent::image_guest(image).unwrap_or_else(|e| {
        eprintln!("hydo-bench: {e}");
        std::process::exit(2)
    });
    let cpu = if g.cpu == "v3" {
        CpuLevel::V3
    } else {
        CpuLevel::V2
    };
    (
        g.ram_mib,
        cpu,
        hydo_engine::image::cpu_cache_of(image, g.cpu_cache),
    )
}

/// The host controller over `engine` for the agent image `image`; exits with the controller's
/// refusal when it cannot drive the image (a shell-init image, M145).
fn agent_engine(
    engine: Arc<dyn ControlledEngine + Send + Sync>,
    image: &Path,
    control: &hydo_config::run::Control,
) -> Arc<dyn Engine + Send + Sync> {
    match hydo_control::agent::AgentEngine::for_image(engine, image) {
        Ok(a) => Arc::new(a.with_control(control)),
        Err(e) => {
            eprintln!("hydo-bench: {e}");
            std::process::exit(2);
        }
    }
}

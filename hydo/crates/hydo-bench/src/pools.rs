//! Worker pools over the hvx engine, one per image, as explore.py's `Pool(n, outdir)` started
//! them for run.py: `HYDO_BUILD` (default `~/hydo/build`) holds the images, the guest kernel
//! `bzImage` and `initramfs-kafka.cpio.gz`; the engine is the Rust engine in this process when it
//! can open the device, otherwise the program `hydo-hvx` (`hydo_engine::hvx::program`, or
//! `HYDO_HVX_EXPLORE`) started through `~/bin/hvx-run` (or `HYDO_HVX_RUN`, the box's shared
//! module lock); the values are the program's `hydo_config::Config` (M144). An agent image (its
//! `/etc/hydo/control` says `agent`, M121) is run through the host controller
//! (`hydo_control::agent::AgentEngine`, M126), as `hydo explore` runs one; an agent image the
//! controller cannot drive (for example one without an embedded manifest) is refused with the
//! controller's message (M129).

use hydo_engine::{hvx, BootSpec, CpuLevel, Engine, SnapshotStore};
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
        let (ram_mib, cpu) = guest_of(&self.build.join(image));
        let spec = BootSpec {
            image: self.build.join(image),
            ram_mib,
            vcpus: self.vcpus,
            cpu,
            kernel: self.build.join("bzImage"),
            initrd: Some(self.build.join("initramfs-kafka.cpio.gz")),
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
        let engine: Arc<dyn Engine + Send + Sync> = Arc::new(engine);
        let engine = agent_or(engine, &spec.image, &self.config.control);
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

/// `engine` itself for a shell image, the host controller over it for an agent image; exits with
/// the controller's refusal when it cannot drive the image.
/// The guest's RAM and CPU level from the manifest an image carries (`guest.ram_mib`,
/// `guest.cpu`, as the explorer picks them for `--image`; M136: `list` on the onboarding examples
/// needs them, the benchmark's own manifest has the defaults); 1024 MiB and v2 for an image
/// without a manifest (the stack images) or without the keys.
fn guest_of(image: &Path) -> (u32, CpuLevel) {
    let Ok(text) = hydo_engine::image::embedded_manifest(image) else {
        return (1024, CpuLevel::V2);
    };
    let Ok(m) = hydo_manifest::from_embedded(&text, None) else {
        return (1024, CpuLevel::V2);
    };
    let g = &m.data["guest"];
    let ram = g["ram_mib"].as_u64().map_or(1024, |r| r as u32);
    let cpu = if g["cpu"].as_str() == Some("v3") {
        CpuLevel::V3
    } else {
        CpuLevel::V2
    };
    (ram, cpu)
}

fn agent_or(
    engine: Arc<dyn Engine + Send + Sync>,
    image: &Path,
    control: &hydo_config::run::Control,
) -> Arc<dyn Engine + Send + Sync> {
    match hydo_control::agent::AgentEngine::for_image(engine.clone(), image) {
        Ok(None) => engine,
        Ok(Some(a)) => Arc::new(a.with_control(control)),
        Err(e) => {
            eprintln!("hydo-bench: {e}");
            std::process::exit(2);
        }
    }
}

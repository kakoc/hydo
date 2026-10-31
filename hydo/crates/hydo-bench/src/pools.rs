//! Worker pools over the hvx engine, one per image, as explore.py's `Pool(n, outdir)` started
//! them for run.py: `$HYDO_BUILD` (default `~/hydo/build`) holds the images, the guest kernel
//! `bzImage` and `initramfs-kafka.cpio.gz`; the engine is `<root>/hv/hvx/hvx_explore` (or
//! `$HYDO_HVX_EXPLORE`) started through `~/bin/hvx-run` (or `$HYDO_HVX_RUN`, the box's shared
//! module lock), with snapshot files in `$HYDO_SNAP_DIR` (default `/var/tmp/hvx-snap`). Snapshots
//! stay in files unless `HYDO_SNAPSHOTS=memory` (the benchmark runs beside other sessions on a
//! shared box; either store gives the same fingerprints, M102).

use hydo_engine::{hvx, BootSpec, CpuLevel, Engine, SnapshotStore};
use hydo_explore::pool::{Host, Pool};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn env_path(k: &str, d: PathBuf) -> PathBuf {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(d)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// What every pool of one invocation shares.
#[derive(Clone, Debug)]
pub struct Engines {
    pub root: PathBuf,
    pub build: PathBuf,
    pub vcpus: u32,
    /// explore.py's EXTRA_ENV (listrun.py `--env KEY=VAL`).
    pub extra_env: Vec<String>,
}

impl Engines {
    pub fn new(root: &Path, vcpus: u32, extra_env: Vec<String>) -> Engines {
        Engines {
            root: root.to_path_buf(),
            build: env_path("HYDO_BUILD", home().join("hydo").join("build")),
            vcpus,
            extra_env,
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
            engine: env_path(
                "HYDO_HVX_EXPLORE",
                self.root.join("hv").join("hvx").join("hvx_explore"),
            ),
            hvx_run: Some(env_path("HYDO_HVX_RUN", home().join("bin").join("hvx-run"))),
            snap_dir: std::env::var("HYDO_SNAP_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/var/tmp/hvx-snap".into()),
            extra_env: self.extra_env.clone(),
            env: Vec::new(),
        };
        let spec = BootSpec {
            image: self.build.join(image),
            ram_mib: 1024,
            vcpus: self.vcpus,
            cpu: CpuLevel::V2,
            kernel: self.build.join("bzImage"),
            initrd: Some(self.build.join("initramfs-kafka.cpio.gz")),
            outdir: outdir.join("consoles"),
            snapshots: if std::env::var("HYDO_SNAPSHOTS").as_deref() == Ok("memory") {
                SnapshotStore::Memory
            } else {
                SnapshotStore::Files
            },
        };
        let _ = std::fs::create_dir_all(outdir);
        let host = Host {
            outdir: outdir.to_path_buf(),
            onboard: self.root.join("hydo").join("onboard"),
            known: None,
        };
        let engine: Arc<dyn Engine + Send + Sync> = Arc::new(engine);
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

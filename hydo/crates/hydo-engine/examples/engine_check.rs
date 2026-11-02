//! Check the engine interface on a real engine: every schedule runs once through a serve worker
//! (boot once, S0/S1 snapshots, each run in a fresh VM from S1) and once from a fresh boot
//! (`one`), and the two fingerprints must be equal.
//!
//! ```text
//! engine_check <release root> <image> <outdir> "<schedule>"...
//! ```
//!
//! The engine is the platform's (hvx on Linux through the release's `libexec/hvx-run`, hvm on the
//! Mac); the guest RAM is taken from `HYDO_RAM_MIB` (default 1024).

use hydo_engine::{hvm, hvx, BootSpec, CpuLevel, Engine, EngineKind, SnapshotStore};
use hydo_schedule::Schedule;
use std::path::PathBuf;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: engine_check <release root> <image> <outdir> \"<schedule>\"...");
        std::process::exit(2);
    }
    let root = PathBuf::from(&a[1]);
    let image = PathBuf::from(&a[2]).canonicalize().expect("image");
    let outdir = PathBuf::from(&a[3]);
    std::fs::create_dir_all(&outdir).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let ram_mib = std::env::var("HYDO_RAM_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);
    let (engine, kernel, initrd): (Box<dyn Engine>, PathBuf, Option<PathBuf>) =
        match EngineKind::native() {
            EngineKind::Hvx => {
                let e = hvx::Hvx::for_release(&root, hydo_config::Env::process());
                (
                    Box::new(e),
                    root.join("guest/bzImage"),
                    Some(root.join("guest/initramfs-kafka.cpio.gz")),
                )
            }
            EngineKind::Hvm => {
                let k =
                    hvm::kernel_for(&image, None, &root.join("hv/hvm/explore/out/Image-explore"))
                        .expect("kernel");
                (
                    Box::new(hvm::Hvm::for_release(&root, hydo_config::Env::process())),
                    k,
                    None,
                )
            }
        };
    let spec = |dir: &str| BootSpec {
        image: image.clone(),
        ram_mib,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel: kernel.clone(),
        initrd: initrd.clone(),
        outdir: outdir.join(dir),
        snapshots: SnapshotStore::Memory,
    };
    let scheds: Vec<Schedule> = a[4..]
        .iter()
        .map(|s| Schedule::parse(s).unwrap_or_else(|e| panic!("{s}: {e}")))
        .collect();
    std::fs::create_dir_all(outdir.join("serve")).expect("serve dir");
    let mut w = engine
        .start(&spec("serve"), &outdir.join("worker.err"))
        .unwrap_or_else(|e| panic!("start: {e}"));
    println!(
        "{} worker READY boot_secs={}",
        engine.kind().name(),
        w.boot_secs()
    );
    let mut served = Vec::new();
    for (i, s) in scheds.iter().enumerate() {
        let r = w
            .run(&format!("s{i}"), s)
            .unwrap_or_else(|e| panic!("run: {e}"));
        println!(
            "serve {} fp={} secs={} recovery={} verdict={}",
            s,
            r.fp,
            r.secs_text,
            r.recovery(&outdir.join("serve")),
            r.verdict
        );
        served.push(r.fp);
    }
    w.close().expect("close");
    let mut same = 0;
    for (i, s) in scheds.iter().enumerate() {
        let r = engine
            .one(&spec(&format!("one{i}")), s)
            .unwrap_or_else(|e| panic!("one: {e}"));
        let r = r.result.unwrap_or_else(|| panic!("no RESULT for {s}"));
        let ok = r.fp == served[i];
        same += ok as usize;
        println!(
            "one   {} fp={} {}",
            s,
            r.fp,
            if ok { "SAME" } else { "DIFFERENT" }
        );
    }
    println!(
        "engine_check: {same} of {} schedules: serve fingerprint == fresh-boot fingerprint",
        scheds.len()
    );
    std::process::exit(if same == scheds.len() { 0 } else { 1 });
}

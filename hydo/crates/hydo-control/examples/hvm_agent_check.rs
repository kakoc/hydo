//! The agent-image variant of `hv/hvm/explore/lockstep-check.sh`'s `run` group (M143). Since M136
//! every image `hydo build` makes is an agent image, which runs only under the host controller,
//! so the shell images' check "a CTL whose host only answers CONT gives RUN's fingerprint" has no
//! counterpart. This one checks what remains: every schedule, driven by the controller, gives one
//! fingerprint, one control-transcript hash (`ctl=`) and one verdict from the host's S1 in a
//! serve worker (twice) and from a fresh boot (`one`, twice).
//!
//! ```text
//! hvm_agent_check <engine program> <Image> <agent image .cpio.gz> <outdir> "<schedule>"...
//! ```
//!
//! Exit status 0 when every schedule agrees.

use hydo_control::agent::AgentEngine;
use hydo_engine::hvm::Hvm;
use hydo_engine::{BootSpec, CpuLevel, Engine, SnapshotStore};
use hydo_schedule::Schedule;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("usage: hvm_agent_check <engine program> <Image> <agent image .cpio.gz> <outdir> \"<schedule>\"...");
        std::process::exit(2);
    }
    let outdir = PathBuf::from(&a[4]);
    std::fs::create_dir_all(&outdir).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let image = PathBuf::from(&a[3]).canonicalize().expect("image");
    let inner = Arc::new(Hvm {
        hvmx: PathBuf::from(&a[1]),
        env: vec![(
            "HVM_SNAP_DIR".into(),
            outdir.join("snap").display().to_string(),
        )],
        config: hydo_config::Env::process(),
    });
    let engine = match AgentEngine::for_image(inner, &image) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("hvm_agent_check: {e}");
            std::process::exit(2);
        }
    };
    let spec = |dir: &str| BootSpec {
        image: image.clone(),
        ram_mib: 1024,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel: PathBuf::from(&a[2]),
        initrd: None,
        outdir: outdir.join(dir),
        snapshots: SnapshotStore::Files,
    };
    let serve = spec("serve");
    std::fs::create_dir_all(&serve.outdir).expect("serve dir");
    let mut w = engine
        .start(&serve, &outdir.join("serve.err"))
        .expect("start");
    let mut failed = false;
    for (i, text) in a[5..].iter().enumerate() {
        let s = Schedule::parse(text).expect("schedule");
        let mut got = Vec::new();
        for k in 0..2 {
            let r = w.run(&format!("s{i}-{k}"), &s).expect("run from S1");
            got.push((format!("s1/{k}"), r));
        }
        for k in 0..2 {
            let sp = spec(&format!("one{i}-{k}"));
            let r = engine.one(&sp, &s).expect("one");
            got.push((format!("one/{k}"), r.result.expect("a RESULT")));
        }
        let key = |r: &hydo_engine::protocol::RunResult| {
            (
                r.fp.clone(),
                r.extra("ctl").map(str::to_string),
                r.verdict.clone(),
            )
        };
        let first = key(&got[0].1);
        let ok = got.iter().all(|(_, r)| key(r) == first);
        failed |= !ok;
        let all: Vec<String> = got
            .iter()
            .map(|(k, r)| format!("{k}={} ctl={}", r.fp, r.extra("ctl").unwrap_or("-")))
            .collect();
        println!(
            "{} {text} fp={} ctl={} {}{}",
            if ok { "OK  " } else { "FAIL" },
            first.0,
            first.1.as_deref().unwrap_or("-"),
            first.2.split(" (").next().unwrap_or(""),
            if ok {
                String::new()
            } else {
                format!(" [{}]", all.join(" "))
            }
        );
    }
    let _ = w.close();
    std::process::exit(i32::from(failed));
}

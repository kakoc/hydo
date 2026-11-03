//! The port's constants, struct layouts and device tree against the C engine (hv/hvm/hvm.c and
//! virtio.c, deleted in M147). `tests/probe.c` includes the C sources and prints every
//! Hypervisor.framework constant the port names, the snapshot's system-register lists, `struct
//! mach` and `struct vstate` filled with distinct values, and two device trees; its answers are
//! recorded in `tests/golden/c_layout.txt`, and this test compares the Rust values with them.
//!
//! To check against the C itself again, or to record the golden anew from it (Apple Silicon with
//! the SDK's Hypervisor.framework headers and clang), give a directory with the C files:
//!
//! ```text
//! mkdir /tmp/hvm-c && for f in hvm.c virtio.c virtio_blk.c virtio_net.c virtio.h virtio_int.h; do
//!   git show 145c456:hv/hvm/$f > /tmp/hvm-c/$f; done
//! HYDO_HVM_C_SRC=/tmp/hvm-c [HYDO_BLESS=1] cargo test -p hydo-hvm --test c_layout
//! ```

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use hydo_hvm::hv::{self, sr};
use hydo_hvm::layout::*;
use hydo_hvm::machine::{Mach, MACH_SIZE};
use hydo_hvm::virtio::{VState, VSTATE_SIZE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The golden's first line.
const HEADER: &str =
    "# hv/hvm/hvm.c and virtio*.c (deleted in M147; git show 145c456:hv/hvm/<file>) \
compiled with tests/probe.c and run on an M2 Pro, macOS 26.4.1\n";

/// The probe's output from the C sources in `src`.
fn c_probe(src: &Path) -> String {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hvm-probe");
    let st = std::process::Command::new("clang")
        .args(["-O1", "-w", "-o"])
        .arg(&out)
        .arg(here.join("tests/probe.c"))
        .arg(src.join("virtio.c"))
        .arg(src.join("virtio_blk.c"))
        .arg(src.join("virtio_net.c"))
        .arg("-I")
        .arg(src)
        .args(["-framework", "Hypervisor"])
        .status()
        .expect("clang");
    assert!(st.success(), "the C probe does not compile");
    let o = std::process::Command::new(&out).output().expect("probe");
    String::from_utf8(o.stdout).unwrap()
}

/// The recorded C answers; with `HYDO_HVM_C_SRC` the C's own, compared with the golden first
/// (`HYDO_BLESS=1` records them instead).
fn probe() -> HashMap<String, String> {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/c_layout.txt");
    let mut text = std::fs::read_to_string(&golden).unwrap_or_default();
    if let Some(src) = std::env::var_os("HYDO_HVM_C_SRC") {
        let c = format!("{HEADER}{}", c_probe(Path::new(&src)));
        if std::env::var("HYDO_BLESS").as_deref() == Ok("1") {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &c).unwrap();
            println!("c_layout: recorded {}", golden.display());
        } else {
            assert_eq!(c, text, "the C's answers are not the recorded ones");
        }
        text = c;
    }
    assert!(!text.is_empty(), "no golden {}", golden.display());
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

#[test]
fn constants_layouts_and_device_tree() {
    let p = probe();
    let n = |k: &str| p[k].parse::<u64>().unwrap();
    assert_eq!(n("HV_REG_X0"), hv::HV_REG_X0 as u64);
    assert_eq!(n("HV_REG_PC"), hv::HV_REG_PC as u64);
    assert_eq!(n("HV_REG_FPCR"), hv::HV_REG_FPCR as u64);
    assert_eq!(n("HV_REG_FPSR"), hv::HV_REG_FPSR as u64);
    assert_eq!(n("HV_REG_CPSR"), hv::HV_REG_CPSR as u64);
    assert_eq!(n("HV_SIMD_FP_REG_Q0"), hv::HV_SIMD_FP_REG_Q0 as u64);
    assert_eq!(n("HV_INTERRUPT_TYPE_IRQ"), hv::HV_INTERRUPT_TYPE_IRQ as u64);
    assert_eq!(
        n("HV_EXIT_REASON_CANCELED"),
        hv::HV_EXIT_REASON_CANCELED as u64
    );
    assert_eq!(
        n("HV_EXIT_REASON_EXCEPTION"),
        hv::HV_EXIT_REASON_EXCEPTION as u64
    );
    assert_eq!(n("exit_size"), 32);
    assert_eq!(n("exit_exception"), 8);
    for (i, r) in hydo_hvm::vmm::SR.iter().enumerate() {
        assert_eq!(n(&format!("SR{i}")), *r as u64, "SR[{i}]");
    }
    for (i, r) in hydo_hvm::vmm::DR.iter().enumerate() {
        assert_eq!(n(&format!("DR{i}")), *r as u64, "DR[{i}]");
    }
    assert_eq!(sr::MDSCR_EL1, 32786);
    assert_eq!(n("mach_size"), MACH_SIZE as u64);
    assert_eq!(n("vstate_size"), VSTATE_SIZE as u64);

    let mut m = Mach {
        vtime: 1,
        exits: 2,
        cntp_ctl: 3,
        cntp_cval: 4,
        console_len: 99,
        halted: 98,
        stepping: 97,
        mdscr_saved: 96,
        n_wfi: 1,
        n_wfi_jump: 2,
        n_cntpct: 3,
        n_mmio: 4,
        n_hvc: 5,
        n_sysreg_other: 6,
        n_irq_line: 7,
        n_steps: 8,
        n_step_runs: 9,
        ..Mach::default()
    };
    let g = &mut m.gic;
    (g.dctlr, g.cctlr, g.pmr, g.bpr) = (5, 6, 7, 8);
    for i in 0..2 {
        g.enabled[i] = 10 + i as u32;
        g.pending[i] = 20 + i as u32;
        g.active[i] = 30 + i as u32;
        g.group[i] = 40 + i as u32;
    }
    for i in 0..4 {
        g.cfg[i] = 50 + i as u32;
    }
    for i in 0..NIRQ {
        g.prio[i] = i as u8;
        g.targets[i] = (200 - i) as u8;
    }
    let u = &mut m.uart;
    (u.ier, u.lcr, u.mcr, u.scr, u.dll, u.dlm, u.fcr) = (1, 2, 3, 4, 5, 6, 7);
    assert_eq!(p["mach"], hex(&m.bytes()));

    let mut v = VState {
        ndev: 2,
        step: 64,
        ..VState::default()
    };
    for d in 0..2 {
        let x = &mut v.dev[d];
        (x.ty, x.status, x.dfsel, x.drfsel, x.qsel, x.drv_features) = (1 + d as u32, 2, 3, 4, 5, 6);
        for q in &mut x.q {
            (q.num, q.ready, q.desc, q.avail, q.used, q.last_avail) = (7, 8, 9, 10, 11, 12);
        }
        (x.isr, x.isr_shown, x.armed, x.due, x.config_gen) = (13, 14, 15, 16, 17);
    }
    let b = &mut v.blk;
    (b.seed, b.classes, b.prob, b.max_delay) = (18, 19, 20, 21);
    (
        b.req_seq,
        b.kick_seq,
        b.fault_count,
        b.disk_size,
        b.n_unsync,
    ) = (22, 23, 24, 25, 26);
    let t = &mut v.net;
    for i in 0..6 {
        t.mac[i] = 27 + i as u8;
    }
    (t.rx_n, t.rx_head, t.rx_next_at, t.pace, t.seed, t.classes) = (33, 34, 35, 36, 37, 38);
    (t.prob, t.rx_seq, t.tx_seq, t.fault_count, t.tx_len) = (39, 40, 41, 42, 43);
    assert_eq!(p["vstate"], hex(&v.bytes()));

    let f0 = hydo_hvm::fdt::build(
        "console=ttyS0 hydo",
        INITRD_IPA,
        INITRD_IPA + 12345,
        1 << 30,
        &[],
    );
    assert_eq!(p["fdt0"], hex(&f0));
    let devs: Vec<(u64, u32)> = (0..2).map(hydo_hvm::virtio::Virtio::dev_info).collect();
    let f2 = hydo_hvm::fdt::build(BOOTARGS, INITRD_IPA, INITRD_IPA + 999, 3 << 30, &devs);
    assert_eq!(p["fdt2"], hex(&f2));
}

//! hv/hvm/hvm.c's own `main` in Rust: the boot probes `run`, `print` and `branch` (BOOT.md), with
//! hvm's defaults (256 MiB, the hardware IRQ line unless HVM_SOFT_IRQ=1, no mlock). The M48 boot
//! fingerprint and the netns kernel's are taken with `print`.
//!
//! ```text
//! hydo-hvm hvm run    <Image> <initrd> [end-marker]
//! hydo-hvm hvm print  <Image> <initrd> <end-marker>
//! hydo-hvm hvm branch <Image> <initrd> <mid-marker> <end-marker>
//! ```

use crate::layout::*;
use crate::machine::{Pct, Tjit};
use crate::serve::{opts_from, Lookup};
use crate::vmm::{now_s, Vmm};
use crate::{catch_fatal, fatal, say, Diag};
use hydo_hvx::cstr::{self, Scan};
use std::io::Write;
use std::sync::{Arc, Mutex};

fn print_fp(vm: &Vmm, out: &mut dyn Write, tag: &str, p: [u64; 5], stop: i32, secs: f64, dump: Option<&str>) {
    if let Some(d) = dump {
        dump_state(vm, d);
    }
    let m = &vm.m;
    let _ = writeln!(
        out,
        "{tag}: stop={stop} exits={} vtime={} ({:.3} s virtual) console={} B wall={secs:.2} s | fp={:016x} (console {:016x} regs {:016x} mach {:016x} ram {:016x})",
        m.exits,
        m.vtime,
        m.vtime as f64 / CNTFRQ as f64,
        m.console_len,
        p[4],
        p[0],
        p[1],
        p[2],
        p[3]
    );
    let _ = writeln!(
        out,
        "{tag}: cntpct reads {}, mmio {}, hvc {}, wfi {} (clock jumps {}), other sysreg traps {}, entries with IRQ line up {}, single steps {} in {} runs",
        m.n_cntpct, m.n_mmio, m.n_hvc, m.n_wfi, m.n_wfi_jump, m.n_sysreg_other, m.n_irq_line, m.n_steps, m.n_step_runs
    );
    if let Some(s) = vm.vio.stats(tag) {
        let _ = writeln!(out, "{s}");
    }
    if vm.pct.n != 0 {
        let _ = writeln!(
            out,
            "{tag}: preemption points: {} chosen, {} made the timer due, {} found no timer armed",
            vm.pct.n, vm.pct.fired, vm.pct.missed
        );
    }
    if vm.tj.on {
        let _ = writeln!(
            out,
            "{tag}: tick jitter: {} timer interrupts delayed, last at tick {}",
            vm.tj.applied, vm.tj.last_applied_k
        );
    }
    for l in vm.spin_stats(tag) {
        let _ = writeln!(out, "{l}");
    }
    let _ = out.flush();
}

/// HVM_DUMP=<prefix>: <prefix>.regs (text) and <prefix>.ram (raw)
fn dump_state(vm: &Vmm, prefix: &str) {
    use std::fmt::Write as _;
    let s = vm.vtake();
    let mut t = String::new();
    for i in 0..31 {
        let _ = writeln!(t, "x{i} {:016x}", s.x[i]);
    }
    let _ = writeln!(t, "pc {:016x}\ncpsr {:016x}\nfpcr {:016x}\nfpsr {:016x}", s.pc, s.cpsr, s.fpcr, s.fpsr);
    for i in 0..32 {
        let _ = write!(t, "q{i} ");
        for b in s.q[i].0 {
            let _ = write!(t, "{b:02x}");
        }
        t.push('\n');
    }
    for (i, r) in crate::vmm::SR.iter().enumerate() {
        let _ = writeln!(t, "sr{i}(0x{r:x}) {} {:016x}", s.ok[i], s.sr[i]);
    }
    let _ = std::fs::write(format!("{prefix}.regs"), t);
    let _ = std::fs::write(format!("{prefix}.ram"), vm.ram.bytes());
}

/// hvm's `main` (`args[0]` is the program, `args[1]` the mode).
pub fn main_with(args: &[String], get: Lookup, out: Box<dyn Write + Send>, diag: Box<dyn Write + Send>) -> i32 {
    let diag: Diag = Arc::new(Mutex::new(diag));
    let d2 = diag.clone();
    catch_fatal(&d2, move || hvm_main(args, get, out, diag))
}

fn hvm_main(args: &[String], get: Lookup, mut out: Box<dyn Write + Send>, diag: Diag) -> i32 {
    if args.len() < 4 {
        say(&diag, "usage: hvm run|print|branch <Image> <initrd> [markers...]");
        return 2;
    }
    let mode = args[1].as_str();
    let kernel = std::fs::read(&args[2]).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", args[2])));
    let initrd = std::fs::read(&args[3]).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", args[3])));
    let mut vo = opts_from(get, false);
    vo.ram_size = (get("HVM_MEM_MB")
        .map(|v| cstr::atoi(v.as_bytes()))
        .unwrap_or(256) as i64 as u64)
        << 20;
    if vo.rw_on && !vo.spin_observe {
        say(&diag, "hvm: HVM_SPIN_REWIND=1 needs the observer (HVM_SPIN_OBSERVE)");
        return 2;
    }
    let max_exits = get("HVM_MAX_EXITS").map(|v| cstr::strtoull0(v.as_bytes()));
    let dump = get("HVM_DUMP");
    let echo: Option<Box<dyn Write + Send>> = Some(Box::new(std::io::stdout()));
    let echo_on = vo.echo;
    let mut vm = Vmm::new(&vo, &kernel, &initrd, diag.clone(), echo, false);
    if let Some(m) = max_exits {
        vm.max_exits = m;
    }
    if let Some(p) = get("HVM_PCT") {
        let mut sc = Scan::new(p.as_bytes());
        let sd = sc.hex().unwrap_or(0);
        let n = if sc.lit(",").ok() { sc.uint().unwrap_or(0) } else { 0 };
        let lo = if sc.lit(",").ok() { sc.ull().unwrap_or(0) } else { 0 };
        let hi = if sc.lit(",").ok() { sc.ull().unwrap_or(0) } else { 0 };
        vm.pct = Pct::set(sd, n, lo, hi);
    }
    if let Some(p) = get("HVM_TJ") {
        let mut sc = Scan::new(p.as_bytes());
        let mut t = Tjit {
            seed: sc.hex().unwrap_or(0),
            ..Tjit::default()
        };
        t.permille = if sc.lit(",").ok() { sc.uint().unwrap_or(0) } else { 0 };
        t.max_delay = if sc.lit(",").ok() { sc.uint().unwrap_or(0) } else { 0 };
        t.lo = if sc.lit(",").ok() { sc.ull().unwrap_or(0) } else { 0 };
        t.hi = if sc.lit(",").ok() { sc.ull().unwrap_or(0) } else { 0 };
        t.on = t.permille != 0 && t.max_delay != 0;
        vm.tj = t;
    }
    let t0 = now_s();
    let mut none = |_: &mut Vmm| false;
    if mode == "run" || mode == "print" {
        let end = args.get(4).map(|s| s.as_bytes().to_vec());
        let stop = vm.run_until(end.as_deref(), &mut none);
        let dt = now_s() - t0;
        if mode == "run" && !echo_on {
            let _ = out.write_all(&vm.console[..vm.m.console_len as usize]);
        }
        let p = vm.parts();
        print_fp(&vm, &mut *out, mode, p, stop, dt, dump.as_deref());
        vm.vio.finish(&vo.virtio);
        return if stop == STOP_MARKER || (end.is_none() && stop != STOP_LIMIT) {
            0
        } else {
            1
        };
    }
    if mode == "branch" && args.len() >= 6 {
        let (mid, end) = (args[4].as_bytes(), args[5].as_bytes());
        let s1 = vm.run_until(Some(mid), &mut none);
        if s1 != STOP_MARKER {
            say(&diag, &format!("branch: mid marker not reached (stop {s1})"));
            return 1;
        }
        let vs = vm.vtake();
        let ram_copy = vm.ram.bytes().to_vec();
        let msave = vm.m;
        let dsave = vm.vio.save();
        let _ = writeln!(
            out,
            "branch: snapshot at exit {} (vtime {}, console {} B)",
            vm.m.exits, vm.m.vtime, vm.m.console_len
        );
        let sa = vm.run_until(Some(end), &mut none);
        let a = vm.parts();
        print_fp(&vm, &mut *out, "branch-A (continue)", a, sa, now_s() - t0, dump.as_deref());
        // restore into a FRESH vCPU after trashing RAM
        vm.fresh_vcpu();
        vm.ram.bytes_mut().fill(0xee);
        vm.spin_reset();
        vm.ram.bytes_mut().copy_from_slice(&ram_copy);
        vm.vput(&vs);
        vm.m = msave;
        vm.vio.restore(&dsave);
        let sb = vm.run_until(Some(end), &mut none);
        let b = vm.parts();
        print_fp(&vm, &mut *out, "branch-B (restored)", b, sb, now_s() - t0, dump.as_deref());
        let same = a[4] == b[4];
        let _ = writeln!(out, "branch: {}", if same { "IDENTICAL" } else { "DIFFERENT" });
        let _ = out.flush();
        return if same { 0 } else { 1 };
    }
    say(&diag, "bad mode");
    2
}

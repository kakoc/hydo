//! The Rust ABI and boot recipe against the C header and `hvx_boot.h` themselves: a C probe is
//! compiled against `hv/hvx/hvx_abi.h` (on macOS with stand-ins for the two Linux headers it
//! includes, using Linux's generic ioctl encoding) and prints every struct size, every field
//! offset and every ioctl number the engine uses; a second probe runs `hvx_boot.h`'s recipe on a
//! synthetic bzImage and initramfs and writes guest RAM and the initial system registers, which
//! must equal the Rust recipe's bytes. Needs a C compiler (`$CC`, default `cc`).

use hydo_hvx::abi::*;
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-hvx-abi-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("linux")).unwrap();
    d
}

/// Compile `src` against hv/hvx (and, off Linux, the stand-in headers) and run it.
fn probe(name: &str, src: &str, args: &[&str]) -> Vec<u8> {
    let d = scratch(name);
    if !cfg!(target_os = "linux") {
        std::fs::write(
            d.join("linux/types.h"),
            "#include <stdint.h>\ntypedef uint8_t __u8; typedef uint16_t __u16; typedef uint32_t __u32; typedef uint64_t __u64;\n",
        )
        .unwrap();
        std::fs::write(
            d.join("linux/ioctl.h"),
            "#define _IOC(d,t,n,s) (((unsigned long)(d)<<30)|((unsigned long)(t)<<8)|(unsigned long)(n)|((unsigned long)(s)<<16))\n\
             #define _IO(t,n) _IOC(0,(t),(n),0)\n#define _IOR(t,n,T) _IOC(2,(t),(n),sizeof(T))\n\
             #define _IOW(t,n,T) _IOC(1,(t),(n),sizeof(T))\n#define _IOWR(t,n,T) _IOC(3,(t),(n),sizeof(T))\n",
        )
        .unwrap();
    }
    let c = d.join("probe.c");
    std::fs::write(&c, src).unwrap();
    let exe = d.join("probe");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(cc);
    cmd.arg("-O0").arg("-w").arg("-o").arg(&exe).arg(&c);
    if !cfg!(target_os = "linux") {
        cmd.arg("-I").arg(&d);
    }
    cmd.arg("-I").arg(root().join("hv/hvx"));
    let st = cmd.status().expect("a C compiler ($CC or cc)");
    assert!(st.success(), "the probe {name} did not compile");
    let out = Command::new(&exe).args(args).output().unwrap();
    assert!(out.status.success(), "the probe {name} failed");
    out.stdout
}

macro_rules! layout {
    ($( $t:ident { $($f:ident),* } )*) => {{
        let mut v: Vec<(String, usize)> = Vec::new();
        $(
            v.push((stringify!($t).to_string(), size_of::<$t>()));
            $( v.push((format!("{}.{}", stringify!($t), stringify!($f)), offset_of!($t, $f))); )*
        )*
        v
    }};
}

macro_rules! ioctls {
    ($($n:ident),*) => { vec![ $( (stringify!($n).to_string(), $n as usize) ),* ] };
}

#[test]
fn structs_and_ioctls_match_the_header() {
    let mut want = layout! {
        hvx_config { mem_size, n_vcpus, flags }
        hvx_regs { rax, rbx, rcx, rdx, rsi, rdi, rbp, r8, r9, r10, r11, r12, r13, r14, r15, rip, rsp, rflags }
        hvx_exit { reason, insn_len, qualification, guest_phys }
        hvx_tsc { tsc, step, aux, count, mode }
        hvx_run_ctl { flags, max_insns, icount, qualification, guest_phys, reason, insn_len }
        hvx_segment { base, limit, selector, ar }
        hvx_sregs { cr0, cr3, cr4, efer, dr7, cs, ss, ds, es, fs, gs, tr, ldtr, gdt_base, idt_base, gdt_limit, idt_limit }
        hvx_snapshot { regs, tsc, tsc_step, tsc_aux, tsc_mode, icount, rdtsc_count, inject_at_icount,
            inject_vector, inject_valid, n_guest_pages, sw_msr, sregs, lapic_apic_base, lapic_tsc_deadline,
            lapic_reg, ioapic_ioregsel, ioapic_id, ioapic_redir, pic_mask, lapic_in_service, uart_ier,
            uart_lcr, uart_mcr, uart_fcr, uart_scr, uart_dll, uart_dlm, vblk_desc, vblk_avail, vblk_used,
            vblk_qsize, vblk_qready, vblk_last_avail, vblk_isr, vblk_irq_vec, vblk_irq_mode,
            vblk_irq_pending, vblk_completion_armed, vblk_completion_due, vblk_status, vblk_qsel,
            vblk_dfeat_sel, vblk_gfeat_sel, vblk_gfeatures, vblk_pci_command, vblk_pci_bar0,
            vblk_pci_cfg_addr, vblk_pci_intline, vblk_msix_control, vblk_config_msix, vblk_queue_msix,
            vblk_msix_tbl, fault_seed, fault_classes, fault_probability, fault_max_delay, fault_kick_seq,
            fault_req_seq, fault_count, vnet_present, vnet_mac, vnet_pad, vnet_dev_status, vnet_qsel,
            vnet_dfeat_sel, vnet_gfeat_sel, vnet_gfeatures, vnet_pci_command, vnet_pci_bar0,
            vnet_pci_intline, vnet_isr, vnet_msix_control, vnet_config_msix, vnet_msix_tbl, vnet_desc,
            vnet_avail, vnet_used, vnet_qsize, vnet_qready, vnet_last_avail, vnet_queue_msix,
            vnet_completion_armed, vnet_completion_due, vnet_irq_pending, vnet_rx_head, vnet_rx_tail,
            vnet_tx_len, vnet_tx_count, vnet_rx_paced, vnet_pad2, vnet_rx_next_deliver, nfault_seed,
            nfault_classes, nfault_probability, nfault_max_delay, nfault_rx_kick_seq, nfault_tx_kick_seq,
            nfault_pad, nfault_rx_seq, nfault_tx_seq, nfault_count, fx, pmu_total, pmu_next, irr, xcr0,
            xsave_ext }
        hvx_pmu { period, tick_tsc, total, next, ticks, overshoots, steps, land_hash, hops }
        hvx_console { len, buf }
        hvx_input { len, buf }
        hvx_disk { len, data, flags }
        hvx_fault { seed, classes, probability, max_delay, _pad, req_seq, fault_count }
        hvx_tjitter { seed, max_delay, permille, n_explicit, tick_lo, tick_hi, explicit_, applied, applied_hash, last_tick }
        hvx_unsync_rec { doff, len, valid }
        hvx_unsync_io { addr, len, n, pad }
        hvx_xtick { seed, rate, permille, lo, hi, tsc, n_explicit, pad, explicit_, applied, applied_hash, first, last }
        hvx_flushcut { on, off, target, count, lo, hi, hung, pad }
        hvx_sched_vcpu { branches, turns, cr2, halted, halt_len, reinj_valid, reinj_info, reinj_ec, reinj_len }
        hvx_sched { quantum, seed, stop_turn, pause_k, n_vcpus, cur, in_turn, turn_over, pause_run, turn_end,
            turns, guard, turn_hash, tsc, tsc_step, tsc_mode, icount, pmu_total, pmu_next, time_jumps,
            deferred, overshoots, wakeups, v }
        hvx_turnctl { margin, burst_seed, burst_len, burst_q, burst_permille, pad0, burst_window, burst_turns, clipped, pad }
        hvx_ftstat_vcpu { calls, total_cyc, fpu_cyc, vmxon_cyc, pmu_cyc, vmcs_cyc, load_cyc, save_cyc,
            pmuoff_cyc, vmxoff_cyc, step_entries, step_cyc, free_entries, free_cyc, ovf, skid_max, skid_sum,
            skid_hist, steps_turn, steps_tick, rearms, resident, step_hw_cyc, pad }
        hvx_ftstat { n_vcpus, pad0, tsc_khz, sched_cyc, sched_turns, pad, v }
        hvx_pt_config { buf_mib, flags, pad, cr3, lo, hi }
        hvx_pt_status { bytes, buf_bytes, status, ctl, saved, pad }
        hvx_pt_read { offset, len, addr }
        hvx_hc_io { addr, len, n, records, errors }
        hvx_hc_state { addr, cap, out_len, out_done, in_len, flags, pad }
        hvx_dirty { ram_addr, ram_bytes, disk_addr, disk_bytes, flags, pad, ram_dirty, disk_dirty }
    };
    // the explicit pads of the snapshot are not fields in C: check that the next field's offset
    // is the C one (it is in the list) and leave them out
    want.retain(|(n, _)| !n.contains("._pad0") && !n.contains("._pad1") && !n.contains("._pad2"));
    // the tick-jitter explicit list's element type has a name only in Rust
    want.push(("hvx_tj_explicit".into(), size_of::<hvx_tj_explicit>()));
    let ioctl = ioctls!(
        HVX_CREATE_VM,
        HVX_SET_REGS,
        HVX_GET_REGS,
        HVX_RUN,
        HVX_SET_TSC,
        HVX_GET_TSC,
        HVX_RUNC,
        HVX_GET_SNAPSHOT,
        HVX_SET_SNAPSHOT,
        HVX_GET_CONSOLE,
        HVX_SET_SREGS,
        HVX_PUT_INPUT,
        HVX_SET_VCPU,
        HVX_SET_DISK,
        HVX_SET_FAULT,
        HVX_SET_PMU,
        HVX_GET_PMU,
        HVX_SET_PT,
        HVX_GET_PT,
        HVX_READ_PT,
        HVX_SET_TJITTER,
        HVX_GET_TJITTER,
        HVX_GET_UNSYNC,
        HVX_SET_UNSYNC,
        HVX_SET_XTICK,
        HVX_GET_XTICK,
        HVX_SET_FLUSHCUT,
        HVX_GET_FLUSHCUT,
        HVX_GET_SCHED,
        HVX_SET_SCHED,
        HVX_GET_FTSTAT,
        HVX_RESET_FTSTAT,
        HVX_SET_TURNCTL,
        HVX_GET_TURNCTL,
        HVX_RESET_VM,
        HVX_GET_DIRTY,
        HVX_HC_GET,
        HVX_HC_PUT,
        HVX_HC_GET_STATE,
        HVX_HC_SET_STATE
    );
    let consts: Vec<(String, usize)> = vec![
        (
            "HVX_VM_IDLE_HEARTBEAT".into(),
            HVX_VM_IDLE_HEARTBEAT as usize,
        ),
        ("HVX_VM_X86_64_V2".into(), HVX_VM_X86_64_V2 as usize),
        ("HVX_VM_PMU_CLOCK".into(), HVX_VM_PMU_CLOCK as usize),
        ("HVX_VM_PAUSE_EXIT".into(), HVX_VM_PAUSE_EXIT as usize),
        ("HVX_VM_PT_TRACE".into(), HVX_VM_PT_TRACE as usize),
        ("HVX_PT_USER".into(), HVX_PT_USER as usize),
        ("HVX_PT_NORETCOMP".into(), HVX_PT_NORETCOMP as usize),
        ("HVX_VM_SMP_SCHED".into(), HVX_VM_SMP_SCHED as usize),
        ("HVX_VM_SMP_BOOT".into(), HVX_VM_SMP_BOOT as usize),
        ("HVX_VM_X86_64_V3".into(), HVX_VM_X86_64_V3 as usize),
        ("HVX_CONSOLE_SZ".into(), HVX_CONSOLE_SZ),
        ("HVX_INPUT_MAX".into(), HVX_INPUT_MAX),
        ("HVX_DISK_MMAP_PGOFF".into(), HVX_DISK_MMAP_PGOFF as usize),
        ("HVX_DISK_NO_IDMAP".into(), HVX_DISK_NO_IDMAP as usize),
        ("HVX_FAULT_ERROR".into(), HVX_FAULT_ERROR as usize),
        ("HVX_FAULT_DROP".into(), HVX_FAULT_DROP as usize),
        (
            "HVX_FAULT_DROP_UNFLUSHED".into(),
            HVX_FAULT_DROP_UNFLUSHED as usize,
        ),
        ("HVX_EXIT_THROTTLE".into(), HVX_EXIT_THROTTLE as usize),
        ("HVX_EXIT_STEP".into(), HVX_EXIT_STEP as usize),
        ("HVX_UNSYNC_MAX".into(), HVX_UNSYNC_MAX as usize),
        ("HVX_UNSYNC_ENTBYTES".into(), HVX_UNSYNC_ENTBYTES as usize),
        ("HVX_UNSYNC_BYTES(256)".into(), unsync_bytes(256) as usize),
        ("HVX_SCHED_MAX_VCPUS".into(), HVX_SCHED_MAX_VCPUS),
        ("HVX_TJ_MAX_EXPLICIT".into(), HVX_TJ_MAX_EXPLICIT),
        ("HVX_XT_MAX_EXPLICIT".into(), HVX_XT_MAX_EXPLICIT),
        ("HVX_VM_HOSTCALL".into(), HVX_VM_HOSTCALL as usize),
        ("HVX_VM_DIRTY_LOG".into(), HVX_VM_DIRTY_LOG as usize),
        ("HVX_EXIT_HOSTCALL".into(), HVX_EXIT_HOSTCALL as usize),
        ("HVX_HC_DATA_PORT".into(), HVX_HC_DATA_PORT as usize),
        ("HVX_HC_CTL_PORT".into(), HVX_HC_CTL_PORT as usize),
        ("HVX_HC_AVAIL_PORT".into(), HVX_HC_AVAIL_PORT as usize),
        ("HVX_HC_MAGIC".into(), HVX_HC_MAGIC as usize),
        ("HVX_HC_SZ".into(), HVX_HC_SZ),
        ("HVX_HC_STATE_OVF".into(), HVX_HC_STATE_OVF as usize),
        ("HVX_DIRTY_CLEAR".into(), HVX_DIRTY_CLEAR as usize),
    ];
    let mut src = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include \"hvx_abi.h\"\n\
         typedef struct { __u64 tick, delay; } hvx_tj_explicit;\nint main(void) {\n",
    );
    for (n, _) in &want {
        match n.split_once('.') {
            None if n == "hvx_tj_explicit" => src.push_str(&format!(
                "printf(\"{n} %zu\\n\", sizeof(hvx_tj_explicit));\n"
            )),
            None => src.push_str(&format!("printf(\"{n} %zu\\n\", sizeof(struct {n}));\n")),
            Some((t, f)) => src.push_str(&format!(
                "printf(\"{n} %zu\\n\", offsetof(struct {t}, {f}));\n"
            )),
        }
    }
    for (n, _) in ioctl.iter().chain(&consts) {
        src.push_str(&format!("printf(\"{n} %lu\\n\", (unsigned long)({n}));\n"));
    }
    src.push_str("return 0; }\n");
    let out = String::from_utf8(probe("layout", &src, &[])).unwrap();
    let got: Vec<(String, usize)> = out
        .lines()
        .map(|l| {
            let (n, v) = l.rsplit_once(' ').unwrap();
            (n.to_string(), v.parse().unwrap())
        })
        .collect();
    let mut all = want.clone();
    all.extend(ioctl);
    all.extend(consts);
    assert_eq!(got.len(), all.len());
    let mut bad = Vec::new();
    for ((n, c), (_, r)) in got.iter().zip(&all) {
        if c != r {
            bad.push(format!("{n}: C {c}, Rust {r}"));
        }
    }
    assert!(
        bad.is_empty(),
        "differs from hvx_abi.h:\n{}",
        bad.join("\n")
    );
    println!(
        "{} sizes, offsets, ioctl numbers and constants equal",
        all.len()
    );
}

/// A deterministic byte stream (xorshift) for the synthetic images.
fn bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn boot_recipe_matches_hvx_boot_h() {
    let mut image = bytes(0x9000, 7);
    image[0x1f1] = 6; // setup_sects
    image[0x1fe] = 0x55;
    image[0x1ff] = 0xaa;
    image[0x202..0x206].copy_from_slice(b"HdrS");
    image[0x206] = 0x0f;
    image[0x207] = 0x02;
    image[0x236] |= 1; // XLF_KERNEL_64
    let initrd = bytes(0x3456, 11);
    let cmdline = "root=/dev/vda1 ro console=ttyS0 hydo_phase=serve";
    let mem_size = 64usize << 20;
    let d = scratch("bootin");
    std::fs::write(d.join("image"), &image).unwrap();
    std::fs::write(d.join("initrd"), &initrd).unwrap();
    for ncpus in [1u32, 3] {
        let src = format!(
            "#include \"hvx_boot.h\"\nint main(int c, char **v) {{\n\
             size_t il, rl; uint8_t *img = read_file(v[1], &il), *ird = read_file(v[2], &rl);\n\
             uint8_t *mem = calloc(1, {mem_size}); struct hvx_sregs s;\n\
             build_page_tables_and_gdt(mem, {mem_size});\n\
             if (load_bzimage_n(mem, {mem_size}, img, il, ird, rl, \"{cmdline}\", {ncpus}) < 0) return 1;\n\
             fill_sregs(&s); fwrite(mem, 1, {mem_size}, stdout); fwrite(&s, 1, sizeof(s), stdout); return 0; }}\n"
        );
        let a = d.join("image").display().to_string();
        let b = d.join("initrd").display().to_string();
        let out = probe(&format!("boot{ncpus}"), &src, &[&a, &b]);
        let mut mem = vec![0u8; mem_size];
        let mut w = Vec::new();
        hydo_hvx::boot::build_page_tables_and_gdt(&mut mem, &mut w);
        hydo_hvx::boot::load_bzimage_n(&mut mem, &image, &initrd, cmdline, ncpus, &mut w).unwrap();
        let s = hydo_hvx::boot::fill_sregs();
        assert_eq!(out.len(), mem_size + size_of::<hvx_sregs>());
        assert!(
            out[..mem_size] == mem[..],
            "guest RAM differs ({ncpus} CPUs)"
        );
        assert_eq!(&out[mem_size..], s.as_bytes(), "sregs differ");
        // every byte the recipe wrote is inside a range it reports
        for (i, &b) in mem.iter().enumerate() {
            if b != 0 {
                assert!(
                    w.iter()
                        .any(|&(o, n)| (i as u64) >= o && (i as u64) < o + n),
                    "byte {i:#x} written outside the reported ranges"
                );
            }
        }
    }
}

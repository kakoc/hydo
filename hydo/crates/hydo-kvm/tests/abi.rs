//! The hand-written KVM types (`hydo_kvm::abi`) against `<linux/kvm.h>` itself: a C probe prints
//! every struct size, field offset, ioctl number, exit reason and capability the engine uses, and
//! each must equal the Rust value. Needs a C compiler (`$CC`, default `cc`) and the Linux UAPI
//! headers (`$HYDO_KVM_HEADERS`, default `/usr/include`); where `linux/kvm.h` is not there (a
//! Mac), the test says so and passes (`src/abi.rs` checks the sizes against recorded values).

use hydo_kvm::abi::*;
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::process::Command;

macro_rules! layout {
    ($( $t:ident as $c:literal { $($f:ident),* } )*) => {{
        let mut v: Vec<(String, u64, String)> = Vec::new();
        $(
            v.push((format!("sizeof {}", $c), size_of::<$t>() as u64, format!("sizeof(struct {})", $c)));
            $( v.push((format!("{}.{}", $c, stringify!($f)), offset_of!($t, $f) as u64,
                       format!("offsetof(struct {}, {})", $c, stringify!($f)))); )*
        )*
        v
    }};
}

macro_rules! consts {
    ($($n:ident),*) => { vec![ $( (stringify!($n).to_string(), $n as u64, stringify!($n).to_string()) ),* ] };
}

#[test]
fn types_and_numbers_match_linux_kvm_h() {
    let inc =
        PathBuf::from(std::env::var("HYDO_KVM_HEADERS").unwrap_or_else(|_| "/usr/include".into()));
    if !inc.join("linux/kvm.h").exists() {
        eprintln!(
            "abi: {} has no linux/kvm.h; the probe runs on Linux (src/abi.rs checks recorded sizes)",
            inc.display()
        );
        return;
    }
    let mut want = layout! {
        kvm_userspace_memory_region as "kvm_userspace_memory_region" { slot, flags, guest_phys_addr, memory_size, userspace_addr }
        kvm_regs as "kvm_regs" { rax, rbx, rcx, rdx, rsi, rdi, rsp, rbp, r8, r15, rip, rflags }
        kvm_segment as "kvm_segment" { base, limit, selector, present, dpl, db, s, l, g, avl, unusable }
        kvm_dtable as "kvm_dtable" { base, limit }
        kvm_sregs as "kvm_sregs" { cs, ds, es, fs, gs, ss, tr, ldt, gdt, idt, cr0, cr2, cr3, cr4, cr8, efer, apic_base, interrupt_bitmap }
        kvm_fpu as "kvm_fpu" { fpr, fcw, fsw, ftwx, last_opcode, last_ip, last_dp, xmm, mxcsr }
        kvm_xsave as "kvm_xsave" { region }
        kvm_msr_entry as "kvm_msr_entry" { index, data }
        kvm_cpuid_entry2 as "kvm_cpuid_entry2" { function, index, flags, eax, ebx, ecx, edx }
        kvm_interrupt as "kvm_interrupt" { irq }
        kvm_vcpu_events as "kvm_vcpu_events" { exception, interrupt, nmi, sipi_vector, flags, smi, exception_has_payload, exception_payload }
        kvm_debugregs as "kvm_debugregs" { db, dr6, dr7, flags }
        kvm_enable_cap as "kvm_enable_cap" { cap, flags, args }
        kvm_msr_filter_range as "kvm_msr_filter_range" { flags, nmsrs, base, bitmap }
        kvm_msr_filter as "kvm_msr_filter" { flags, ranges }
        kvm_guest_debug as "kvm_guest_debug" { control, pad }
        kvm_debug_exit_arch as "kvm_debug_exit_arch" { exception, pad, pc, dr6, dr7 }
    };
    want.push((
        "kvm_segment.type".into(),
        offset_of!(kvm_segment, type_) as u64,
        "offsetof(struct kvm_segment, type)".into(),
    ));
    // the head of struct kvm_run and its exit members
    for (name, v) in [
        (
            "kvm_run.request_interrupt_window",
            offset_of!(kvm_run_head, request_interrupt_window),
        ),
        (
            "kvm_run.immediate_exit",
            offset_of!(kvm_run_head, immediate_exit),
        ),
        ("kvm_run.exit_reason", offset_of!(kvm_run_head, exit_reason)),
        (
            "kvm_run.ready_for_interrupt_injection",
            offset_of!(kvm_run_head, ready_for_interrupt_injection),
        ),
        ("kvm_run.if_flag", offset_of!(kvm_run_head, if_flag)),
        ("kvm_run.cr8", offset_of!(kvm_run_head, cr8)),
        ("kvm_run.apic_base", offset_of!(kvm_run_head, apic_base)),
        ("kvm_run.io", KVM_RUN_EXIT_OFFSET),
        (
            "kvm_run.io.direction",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_io, direction),
        ),
        (
            "kvm_run.io.size",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_io, size),
        ),
        (
            "kvm_run.io.port",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_io, port),
        ),
        (
            "kvm_run.io.count",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_io, count),
        ),
        (
            "kvm_run.io.data_offset",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_io, data_offset),
        ),
        (
            "kvm_run.mmio.phys_addr",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_mmio, phys_addr),
        ),
        (
            "kvm_run.mmio.data",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_mmio, data),
        ),
        (
            "kvm_run.mmio.len",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_mmio, len),
        ),
        (
            "kvm_run.mmio.is_write",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_mmio, is_write),
        ),
        (
            "kvm_run.msr.error",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_msr, error),
        ),
        (
            "kvm_run.msr.reason",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_msr, reason),
        ),
        (
            "kvm_run.msr.index",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_msr, index),
        ),
        (
            "kvm_run.msr.data",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_msr, data),
        ),
        (
            "kvm_run.internal.suberror",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_internal, suberror),
        ),
        (
            "kvm_run.internal.ndata",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_internal, ndata),
        ),
        (
            "kvm_run.internal.data",
            KVM_RUN_EXIT_OFFSET + offset_of!(kvm_run_internal, data),
        ),
    ] {
        let path = name.trim_start_matches("kvm_run.");
        want.push((
            name.to_string(),
            v as u64,
            format!("offsetof(struct kvm_run, {path})"),
        ));
    }
    want.extend(consts!(
        KVM_GET_API_VERSION,
        KVM_CREATE_VM,
        KVM_CHECK_EXTENSION,
        KVM_GET_VCPU_MMAP_SIZE,
        KVM_GET_SUPPORTED_CPUID,
        KVM_CREATE_VCPU,
        KVM_SET_USER_MEMORY_REGION,
        KVM_SET_TSS_ADDR,
        KVM_SET_IDENTITY_MAP_ADDR,
        KVM_RUN,
        KVM_GET_REGS,
        KVM_SET_REGS,
        KVM_GET_SREGS,
        KVM_SET_SREGS,
        KVM_INTERRUPT,
        KVM_GET_MSRS,
        KVM_SET_MSRS,
        KVM_GET_FPU,
        KVM_SET_FPU,
        KVM_GET_XSAVE,
        KVM_SET_XSAVE,
        KVM_SET_CPUID2,
        KVM_GET_VCPU_EVENTS,
        KVM_SET_VCPU_EVENTS,
        KVM_GET_DEBUGREGS,
        KVM_SET_DEBUGREGS,
        KVM_ENABLE_CAP,
        KVM_X86_SET_MSR_FILTER,
        KVM_SET_GUEST_DEBUG,
        KVM_GUESTDBG_ENABLE,
        KVM_GUESTDBG_SINGLESTEP,
        KVM_GUESTDBG_USE_HW_BP,
        KVM_EXIT_DEBUG,
        KVM_EXIT_IO,
        KVM_EXIT_HLT,
        KVM_EXIT_MMIO,
        KVM_EXIT_IRQ_WINDOW_OPEN,
        KVM_EXIT_SHUTDOWN,
        KVM_EXIT_FAIL_ENTRY,
        KVM_EXIT_INTR,
        KVM_EXIT_INTERNAL_ERROR,
        KVM_EXIT_SYSTEM_EVENT,
        KVM_EXIT_X86_RDMSR,
        KVM_EXIT_X86_WRMSR,
        KVM_EXIT_IO_IN,
        KVM_EXIT_IO_OUT,
        KVM_CAP_USER_MEMORY,
        KVM_CAP_SET_TSS_ADDR,
        KVM_CAP_EXT_CPUID,
        KVM_CAP_SET_IDENTITY_MAP_ADDR,
        KVM_CAP_VCPU_EVENTS,
        KVM_CAP_DEBUGREGS,
        KVM_CAP_XSAVE,
        KVM_CAP_IMMEDIATE_EXIT,
        KVM_CAP_X86_USER_SPACE_MSR,
        KVM_CAP_X86_MSR_FILTER,
        KVM_CAP_ENFORCE_PV_FEATURE_CPUID,
        KVM_CAP_PMU_CAPABILITY,
        KVM_MSR_FILTER_READ,
        KVM_MSR_FILTER_WRITE,
        KVM_MSR_FILTER_DEFAULT_DENY,
        KVM_MSR_EXIT_REASON_INVAL,
        KVM_MSR_EXIT_REASON_UNKNOWN,
        KVM_MSR_EXIT_REASON_FILTER,
        KVM_PMU_CAP_DISABLE,
        KVM_CPUID_FLAG_SIGNIFCANT_INDEX,
        KVM_MSR_FILTER_MAX_RANGES
    ));
    want.push((
        "KVM_API_VERSION".into(),
        KVM_API_VERSION as u64,
        "KVM_API_VERSION".into(),
    ));
    want.push((
        "sizeof kvm_msrs".into(),
        size_of::<kvm_msrs_header>() as u64,
        "sizeof(struct kvm_msrs)".into(),
    ));
    want.push((
        "sizeof kvm_cpuid2".into(),
        size_of::<kvm_cpuid2_header>() as u64,
        "sizeof(struct kvm_cpuid2)".into(),
    ));

    let mut c = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include <linux/kvm.h>\nint main(void){\n",
    );
    for (name, _, expr) in &want {
        c.push_str(&format!(
            "printf(\"%s %llu\\n\", \"{name}\", (unsigned long long)({expr}));\n"
        ));
    }
    c.push_str("return 0;}\n");
    let d = std::env::temp_dir().join(format!("hydo-kvm-abi-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("probe.c"), c).unwrap();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let st = Command::new(cc)
        .arg("-I")
        .arg(&inc)
        .arg("-o")
        .arg(d.join("probe"))
        .arg(d.join("probe.c"))
        .status()
        .expect("a C compiler ($CC or cc)");
    assert!(st.success(), "the probe did not compile");
    let out = Command::new(d.join("probe")).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let got: std::collections::HashMap<String, u64> = text
        .lines()
        .filter_map(|l| {
            let (k, v) = l.rsplit_once(' ')?;
            Some((k.to_string(), v.parse().ok()?))
        })
        .collect();
    let mut bad = Vec::new();
    for (name, v, _) in &want {
        match got.get(name) {
            Some(g) if g == v => {}
            g => bad.push(format!("{name}: Rust {v}, linux/kvm.h {g:?}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    eprintln!("abi: {} values equal linux/kvm.h's", want.len());
}

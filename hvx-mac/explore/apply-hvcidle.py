#!/usr/bin/env python3
"""Add CONFIG_ARM64_HVM_IDLE_HVC to a Linux 6.6 tree (argv[1]).

With the option set, arm64's cpu_do_idle() asks the VMM to idle with an HVC
(SMCCC vendor-hypervisor function 0xC6000020) instead of executing WFI.

Why: a WFI only traps to the hypervisor if it would actually enter a low-power
state. If any wake-up event is pending at that instant -- including a HOST
interrupt that Hypervisor.framework is about to take at EL2 -- the WFI completes
as a no-op inside the guest, with no VM exit. The guest then runs one more idle
loop iteration (an extra RCU idle entry/exit, a tick_nohz recomputation) before
the next WFI traps. Every exit stays identical, but those counters end up
different, so the end state depends on host interrupt timing: the rare
divergence under host load in hvx-mac/explore/README.md. An HVC always traps, so
with this option every idle entry is a VM exit and the VMM decides it
deterministically. With the option off the code is unchanged.
"""
import sys

root = sys.argv[1]
p = root + "/arch/arm64/kernel/idle.c"
k = root + "/arch/arm64/Kconfig"
s = open(p).read()
if "ARM64_HVM_IDLE_HVC" in s:
    print("already applied (start from a pristine file)")
    sys.exit(1)
old = """	dsb(sy);
	wfi();

	arm_cpuidle_restore_irq_context(&context);"""
new = """	dsb(sy);
#ifdef CONFIG_ARM64_HVM_IDLE_HVC
	{
		/* hydo: idle through the VMM (always a VM exit), see Kconfig */
		register unsigned long x0 asm("x0") = 0xc6000020UL;
		asm volatile("hvc #0" : "+r" (x0) : : "x1", "x2", "x3", "memory");
	}
#else
	wfi();
#endif

	arm_cpuidle_restore_irq_context(&context);"""
assert s.count(old) == 1, "cpu_do_idle not found"
open(p, "w").write(s.replace(old, new))

ks = open(k).read()
anchor = "config ARM64_USE_LSE_ATOMICS"
assert ks.count(anchor) == 1, "Kconfig anchor not found"
entry = """config ARM64_HVM_IDLE_HVC
	bool "Idle through an HVC to the VMM instead of WFI (hydo)"
	help
	  cpu_do_idle() issues HVC function 0xC6000020 instead of WFI, so every
	  idle entry is a VM exit. For a deterministic VMM on Apple's
	  Hypervisor.framework, where a WFI with a pending (host) wake-up event
	  completes inside the guest without trapping. Say N on real hardware.

"""
open(k, "w").write(ks.replace(anchor, entry + anchor))
print("applied")

#!/usr/bin/env python3
"""Add CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT to a pristine Linux 6.6 tree (argv[1]).

With the option set, an arm64 kernel uses the PHYSICAL counter (CNTPCT_EL0)
everywhere, from the first instruction on, even without hyp mode:
  - the default arch_timer_read_counter (get_cycles, random_get_entropy) is the
    physical counter -- add_device_randomness() reads it long before early
    parameters are parsed, so a boot parameter would be too late;
  - the clocksource/sched_clock use the physical counter;
  - EL0 may not read the virtual counter directly: CNTKCTL_EL1.EL0VCTEN stays
    clear, so a user-space `mrs cntvct_el0` traps to cntvct_read_handler, which
    returns the physical counter;
  - the vDSO counter fast path is off (clock_gettime is a system call).
Apple's Hypervisor.framework traps CNTPCT_EL0 but cannot trap CNTVCT_EL0 on M1/M2,
so this is what lets a VMM give the guest a deterministic clock. With the option
off the code is unchanged.
"""
import sys

root = sys.argv[1]
p = root + "/drivers/clocksource/arm_arch_timer.c"
k = root + "/drivers/clocksource/Kconfig"
s = open(p).read()
if "arch_timer_force_cntpct" in s:
    print("already applied (start from a pristine file)")
    sys.exit(1)

edits = [
    # the force flag, next to vdso_default; vdso fast path off when forced
    ('''static enum vdso_clock_mode vdso_default = VDSO_CLOCKMODE_ARCHTIMER;''',
     '''static enum vdso_clock_mode vdso_default =
	IS_ENABLED(CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT) ? VDSO_CLOCKMODE_NONE : VDSO_CLOCKMODE_ARCHTIMER;'''),
    ('''static bool evtstrm_enable __ro_after_init = IS_ENABLED(CONFIG_ARM_ARCH_TIMER_EVTSTREAM);''',
     '''static bool evtstrm_enable __ro_after_init = IS_ENABLED(CONFIG_ARM_ARCH_TIMER_EVTSTREAM);
/* hydo: physical counter only (see CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT) */
static const bool arch_timer_force_cntpct = IS_ENABLED(CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT);'''),
    # default counter accessor from the very beginning
    ('''u64 (*arch_timer_read_counter)(void) __ro_after_init = arch_counter_get_cntvct;''',
     '''u64 (*arch_timer_read_counter)(void) __ro_after_init =
	IS_ENABLED(CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT) ? arch_counter_get_cntpct : arch_counter_get_cntvct;'''),
    # never give EL0 direct access to the virtual counter
    ('''	if (arch_timer_this_cpu_has_cntvct_wa())
		pr_info("CPU%d: Trapping CNTVCT access\\n", smp_processor_id());''',
     '''	if (arch_timer_this_cpu_has_cntvct_wa() || arch_timer_force_cntpct)
		pr_info("CPU%d: Trapping CNTVCT access\\n", smp_processor_id());'''),
    # clocksource / sched_clock on the physical counter
    ('''		if ((IS_ENABLED(CONFIG_ARM64) && !is_hyp_mode_available()) ||
		    arch_timer_uses_ppi == ARCH_TIMER_VIRT_PPI) {''',
     '''		if (!arch_timer_force_cntpct &&
		    ((IS_ENABLED(CONFIG_ARM64) && !is_hyp_mode_available()) ||
		     arch_timer_uses_ppi == ARCH_TIMER_VIRT_PPI)) {'''),
]
for old, new in edits:
    assert s.count(old) == 1, old
    s = s.replace(old, new)
open(p, "w").write(s)

ks = open(k).read()
anchor = "config ARM_ARCH_TIMER_EVTSTREAM\n"
assert ks.count(anchor) == 1
ks = ks.replace(anchor, '''config ARM_ARCH_TIMER_FORCE_CNTPCT
	bool "Use only the physical counter (deterministic hypervisors)"
	depends on ARM_ARCH_TIMER && ARM64
	help
	  Read time only from CNTPCT_EL0, from boot on, and trap EL0 reads of
	  CNTVCT_EL0. For hypervisors that can trap the physical counter but
	  not the virtual one (Apple Hypervisor.framework on M1/M2), so they can
	  give the guest a deterministic clock. Say N on real hardware.

''' + anchor)
open(k, "w").write(ks)
print("applied")

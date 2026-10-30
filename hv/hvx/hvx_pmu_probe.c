// SPDX-License-Identifier: GPL-2.0
/* Throwaway capability probe for milestone 17 (PMU fast instruction clock).
 * Reads, from INSIDE L1 (where hvx runs), the VMX capability MSRs and CPUID.0AH
 * to confirm a guest-only PMU is available:
 *   - VM-entry  "load IA32_PERF_GLOBAL_CTRL" = VM-entry  ctls bit 13
 *   - VM-exit   "load IA32_PERF_GLOBAL_CTRL" = VM-exit   ctls bit 12
 *   - CPUID.0AH: PerfMon version >= 2, >= 1 fixed counter, INST_RETIRED.ANY
 * Run as root in L1:  sudo modprobe msr; sudo ./hvx_pmu_probe
 */
#include <stdio.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>

static int rdmsr(uint32_t msr, uint64_t *val)
{
	int fd = open("/dev/cpu/0/msr", O_RDONLY);
	if (fd < 0) { perror("open /dev/cpu/0/msr"); return -1; }
	int n = pread(fd, val, 8, msr);
	close(fd);
	if (n != 8) { perror("pread msr"); return -1; }
	return 0;
}

static void cpuid(uint32_t leaf, uint32_t *a, uint32_t *b, uint32_t *c, uint32_t *d)
{
	uint32_t ra, rb, rc, rd;
	__asm__ volatile("cpuid" : "=a"(ra), "=b"(rb), "=c"(rc), "=d"(rd)
			 : "a"(leaf), "c"(0));
	*a = ra; *b = rb; *c = rc; *d = rd;
}

int main(void)
{
	uint64_t basic, misc, true_exit, true_entry, exit_c, entry_c;
	uint32_t a, b, c, d;
	int true_ctls;

	if (rdmsr(0x480, &basic)) return 1;			/* IA32_VMX_BASIC */
	rdmsr(0x485, &misc);					/* IA32_VMX_MISC */
	true_ctls = (basic >> 55) & 1;
	rdmsr(0x483, &exit_c);					/* IA32_VMX_EXIT_CTLS */
	rdmsr(0x484, &entry_c);					/* IA32_VMX_ENTRY_CTLS */
	rdmsr(0x48f, &true_exit);				/* IA32_VMX_TRUE_EXIT_CTLS */
	rdmsr(0x490, &true_entry);				/* IA32_VMX_TRUE_ENTRY_CTLS */

	printf("IA32_VMX_BASIC        = 0x%016llx (TRUE ctls=%d)\n",
	       (unsigned long long)basic, true_ctls);
	printf("IA32_VMX_MISC         = 0x%016llx (preempt-timer rate=%llu)\n",
	       (unsigned long long)misc, (unsigned long long)(misc & 0x1f));

	uint64_t ex  = true_ctls ? true_exit  : exit_c;
	uint64_t en  = true_ctls ? true_entry : entry_c;
	/* allowed-1 == high 32 bits */
	int exit_load  = (ex  >> (32 + 12)) & 1;   /* VM-exit  bit 12 */
	int entry_load = (en  >> (32 + 13)) & 1;   /* VM-entry bit 13 */

	printf("VM-EXIT  ctls (%s)  = 0x%016llx  load-PERF_GLOBAL_CTRL(b12) allowed-1 = %d\n",
	       true_ctls ? "TRUE" : "legacy", (unsigned long long)ex, exit_load);
	printf("VM-ENTRY ctls (%s)  = 0x%016llx  load-PERF_GLOBAL_CTRL(b13) allowed-1 = %d\n",
	       true_ctls ? "TRUE" : "legacy", (unsigned long long)en, entry_load);

	cpuid(0x0a, &a, &b, &c, &d);
	printf("CPUID.0AH eax=0x%08x ebx=0x%08x ecx=0x%08x edx=0x%08x\n", a, b, c, d);
	printf("  PerfMon version         = %u\n", a & 0xff);
	printf("  GP counters/thread      = %u (width %u)\n", (a >> 8) & 0xff, (a >> 16) & 0xff);
	printf("  fixed counters          = %u (width %u)\n", d & 0x1f, (d >> 5) & 0xff);
	printf("  INST_RETIRED.ANY avail  = %s (ebx bit1 clear = available)\n",
	       (b & 0x2) ? "NO" : "yes");

	int ok = exit_load && entry_load && (a & 0xff) >= 2 && (d & 0x1f) >= 1;
	printf("\nGUEST-ONLY PMU FAST CLOCK SUPPORTED: %s\n", ok ? "YES" : "NO");
	return ok ? 0 : 2;
}

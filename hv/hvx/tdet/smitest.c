// SPDX-License-Identifier: GPL-2.0
/*
 * smitest - does a System Management Interrupt (SMI) taken while the guest runs add
 * branches to hvx's guest-only PMU branch counter?
 *
 * Guest: mov $N,%rcx ; 1: dec %rcx ; jnz 1b ; hlt  -- exactly N conditional branches.
 * The PMU clock period is set beyond N (no ticks). At the HLT the module's branch total
 * must be exactly N. The host's SMI count (MSR 0x34 via /dev/cpu/0/msr) is read before
 * and after. Prints one line per run: "SMITEST n=<N> total=<t> diff=<t-N> smi=<delta> secs=<s>".
 * Usage (root, via ~/bin/hvx-run): smitest <N> <runs>   env HVX_DEV overrides /dev/hvx.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <time.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include "../hvx_abi.h"

#define MEM (2 * 1024 * 1024)

static unsigned long long smi(void)
{
	unsigned long long v = 0;
	int fd = open("/dev/cpu/0/msr", O_RDONLY);
	if (fd >= 0) { if (pread(fd, &v, 8, 0x34) != 8) v = 0; close(fd); }
	return v;
}

static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }

int main(int argc, char **argv)
{
	uint64_t n = argc > 1 ? strtoull(argv[1], 0, 0) : 100000000000ULL;
	int runs = argc > 2 ? atoi(argv[2]) : 1, r;
	const char *dev = getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx";
	static const uint8_t code[] = {
		0x48, 0xb9, 0,0,0,0,0,0,0,0,	/* movabs $N,%rcx */
		0x48, 0xff, 0xc9,		/* 1: dec %rcx */
		0x75, 0xfb,			/* jnz 1b */
		0xf4,				/* hlt */
	};
	for (r = 0; r < runs; r++) {
		struct hvx_config cfg; struct hvx_regs rg; struct hvx_tsc t; struct hvx_pmu pm; struct hvx_exit ex;
		uint8_t *mem; unsigned long long s0, s1; double t0;
		int fd = open(dev, O_RDWR);
		if (fd < 0) { perror(dev); return 1; }
		memset(&cfg, 0, sizeof(cfg)); cfg.mem_size = MEM; cfg.flags = HVX_VM_PMU_CLOCK;
		if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return 1; }
		mem = mmap(NULL, MEM, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (mem == MAP_FAILED) { perror("mmap"); return 1; }
		memcpy(mem + HVX_GUEST_CODE_BASE, code, sizeof(code));
		memcpy(mem + HVX_GUEST_CODE_BASE + 2, &n, 8);
		memset(&rg, 0, sizeof(rg)); rg.rip = HVX_GUEST_CODE_BASE; rg.rsp = MEM - 16; rg.rflags = 0x2;
		ioctl(fd, HVX_SET_REGS, &rg);
		memset(&t, 0, sizeof(t)); t.tsc = 1000; t.step = 1; t.mode = HVX_TSC_COUNTER;
		ioctl(fd, HVX_SET_TSC, &t);
		memset(&pm, 0, sizeof(pm)); pm.period = n * 4 + 1000000; pm.tick_tsc = 1000000;
		if (ioctl(fd, HVX_SET_PMU, &pm) < 0) { perror("SET_PMU"); return 1; }
		s0 = smi(); t0 = now();
		for (;;) {
			memset(&ex, 0, sizeof(ex));
			if (ioctl(fd, HVX_RUN, &ex) < 0) { if (errno == EINTR) continue; perror("HVX_RUN"); return 1; }
			if (ex.reason == 12) break;
			if (ex.reason == HVX_EXIT_THROTTLE) continue;
			fprintf(stderr, "unexpected exit %u\n", ex.reason); return 1;
		}
		s1 = smi();
		memset(&pm, 0, sizeof(pm)); ioctl(fd, HVX_GET_PMU, &pm);
		printf("SMITEST n=%llu total=%llu diff=%lld smi=%llu secs=%.1f\n", (unsigned long long)n,
		       (unsigned long long)pm.total, (long long)(pm.total - n), s1 - s0, now() - t0);
		fflush(stdout);
		munmap(mem, MEM); close(fd);
	}
	return 0;
}

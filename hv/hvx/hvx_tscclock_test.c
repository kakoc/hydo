// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_tscclock_test - prove the TSC is tied to the instruction clock (milestone 6).
 *
 * The same guest reads RDTSC twice, with exactly 6 instructions between the two
 * reads (the first RDTSC, two stores, three NOPs), then HLTs:
 *
 *   rdtsc ; mov %eax,0x00(%rbx) ; mov %edx,0x04(%rbx)   ; TSC #0 -> [0x5000]
 *   nop ; nop ; nop
 *   rdtsc ; mov %eax,0x08(%rbx) ; mov %edx,0x0c(%rbx)   ; TSC #1 -> [0x5008]
 *   hlt
 *
 * We run it two ways and compare the delta TSC#1 - TSC#0:
 *   HVX_TSC_COUNTER : advances once per RDTSC *call*  -> delta == step
 *   HVX_TSC_ICOUNT  : advances per instruction retired -> delta == 6 * step
 *
 * The ICOUNT delta being 6x proves the guest clock tracks execution POSITION,
 * not the number of RDTSC calls -- the deterministic clock hydo needs. We also
 * check ICOUNT is reproducible across runs and that TSC#0 == base exactly.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_tscclock_test hvx_tscclock_test.c
 * Run   (inside L1):  sudo ./hvx_tscclock_test
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

#include "hvx_abi.h"

#define EXIT_HLT        12
#define GUEST_MEM_BYTES (64UL * 4096)
#define RESULT_GPA      0x5000UL
#define TSC_BASE        0x0000000100000000ULL	/* high dword set: exercises EDX */
#define TSC_STEP        1000ULL
#define INSNS_BETWEEN   6			/* rdtsc, mov, mov, nop, nop, nop */

/* rbx is provided via HVX_SET_REGS (= 0x5000); the guest is pure logic. */
static const unsigned char guest_code[] = {
	0x0f, 0x31,			/* rdtsc                 (read #0) */
	0x89, 0x43, 0x00,		/* mov %eax,0x00(%rbx) */
	0x89, 0x53, 0x04,		/* mov %edx,0x04(%rbx) */
	0x90, 0x90, 0x90,		/* nop ; nop ; nop */
	0x0f, 0x31,			/* rdtsc                 (read #1) */
	0x89, 0x43, 0x08,		/* mov %eax,0x08(%rbx) */
	0x89, 0x53, 0x0c,		/* mov %edx,0x0c(%rbx) */
	0xf4,				/* hlt */
};

static int set_entry(int fd)
{
	struct hvx_regs regs;

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rbx    = RESULT_GPA;		/* store base for the guest */
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	return ioctl(fd, HVX_SET_REGS, &regs) < 0 ? -1 : 0;
}

/* Single-step to HLT, continuing transparently across the in-kernel cap. */
static int run_to_halt(int fd, struct hvx_run_ctl *out)
{
	struct hvx_run_ctl ctl;
	int reset = 1;

	for (;;) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (reset ? HVX_RUN_RESET_ICOUNT : 0);
		reset = 0;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE)
			continue;
		*out = ctl;
		return 0;
	}
}

/* Run the guest once in the given TSC mode; return the two TSC reads. */
static int run_mode(int fd, void *mem, __u64 mode, uint64_t *t0, uint64_t *t1)
{
	struct hvx_tsc tsc = { .tsc = TSC_BASE, .step = TSC_STEP, .aux = 0, .mode = mode };
	struct hvx_run_ctl ctl;

	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	memset((unsigned char *)mem + RESULT_GPA, 0, 2 * sizeof(uint64_t));
	if (set_entry(fd) < 0) { perror("HVX_SET_REGS"); return -1; }
	if (run_to_halt(fd, &ctl) < 0) return -1;
	if (ctl.reason != EXIT_HLT) {
		fprintf(stderr, "run_mode: ended on reason %u, not HLT\n", ctl.reason);
		return -1;
	}
	*t0 = *(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA);
	*t1 = *(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA + 8);
	return 0;
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	uint64_t c0, c1, i0, i1, j0, j1;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* COUNTER mode: delta is per-RDTSC-call => exactly one step. */
	if (run_mode(fd, mem, HVX_TSC_COUNTER, &c0, &c1) < 0) goto out;
	printf("hvx_tscclock_test: COUNTER: tsc0=0x%016" PRIx64 " tsc1=0x%016" PRIx64
	       " delta=%" PRIu64 " (expect %" PRIu64 ")\n",
	       c0, c1, (uint64_t)(c1 - c0), (uint64_t)TSC_STEP);
	if (c0 != TSC_BASE || c1 - c0 != TSC_STEP) {
		fprintf(stderr, "hvx_tscclock_test: FAIL: COUNTER delta %" PRIu64 " != step %" PRIu64 "\n",
			(uint64_t)(c1 - c0), (uint64_t)TSC_STEP);
		goto out;
	}

	/* ICOUNT mode: delta is per-instruction => 6 * step, and tsc0 == base. */
	if (run_mode(fd, mem, HVX_TSC_ICOUNT, &i0, &i1) < 0) goto out;
	printf("hvx_tscclock_test: ICOUNT : tsc0=0x%016" PRIx64 " tsc1=0x%016" PRIx64
	       " delta=%" PRIu64 " (expect %" PRIu64 " = %d insns * %" PRIu64 ")\n",
	       i0, i1, (uint64_t)(i1 - i0), (uint64_t)(INSNS_BETWEEN * TSC_STEP),
	       INSNS_BETWEEN, (uint64_t)TSC_STEP);
	if (i0 != TSC_BASE || i1 - i0 != INSNS_BETWEEN * TSC_STEP) {
		fprintf(stderr, "hvx_tscclock_test: FAIL: ICOUNT tsc0=0x%" PRIx64
			" delta=%" PRIu64 ", expected base + %" PRIu64 "\n",
			i0, (uint64_t)(i1 - i0), (uint64_t)(INSNS_BETWEEN * TSC_STEP));
		goto out;
	}

	/* ICOUNT determinism: a second run reproduces the exact values. */
	if (run_mode(fd, mem, HVX_TSC_ICOUNT, &j0, &j1) < 0) goto out;
	if (j0 != i0 || j1 != i1) {
		fprintf(stderr, "hvx_tscclock_test: FAIL: ICOUNT not reproducible "
			"(run2 0x%" PRIx64 ",0x%" PRIx64 " vs 0x%" PRIx64 ",0x%" PRIx64 ")\n",
			j0, j1, i0, i1);
		goto out;
	}

	printf("hvx_tscclock_test: MILESTONE 6 PASS: RDTSC tied to the instruction clock "
	       "(ICOUNT delta = %dx COUNTER delta), deterministic\n", INSNS_BETWEEN);
	rc = 0;

out:
	close(fd);
	return rc;
}

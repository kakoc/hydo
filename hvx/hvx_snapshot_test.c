// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_snapshot_test - prove snapshot / branch-from-snapshot (milestone 7).
 *
 * The guest accumulates an instruction-clock-dependent value into memory:
 *
 *   loop:  rdtsc                 ; EAX = low32 of (base + icount*step)  [ICOUNT mode]
 *          add %rax, (%rbx)      ; acc += that value   (%rbx = 0x5000)
 *          dec %ecx             ; %ecx = iteration count
 *          jnz loop
 *          hlt
 *
 * With 20 iterations that is 80 instructions and an accumulator that depends on
 * BOTH the instruction clock and the loop counter. The test:
 *   1. runs it uninterrupted to HLT, recording acc_full and icount (== 80);
 *   2. runs again but stops after exactly 30 instructions, snapshots
 *      (HVX_GET_SNAPSHOT + memcpy of mmap'd RAM), continues to HLT -> acc1;
 *   3. restores the snapshot (HVX_SET_SNAPSHOT + memcpy RAM back), continues to
 *      HLT -> acc2.
 *
 * It asserts acc_full == acc1 == acc2 and icount == 80 for all three. Because
 * the accumulator depends on the instruction clock, the loop counter (%ecx),
 * and the partial sum already written to RAM, any state the snapshot FAILED to
 * capture would make acc2 diverge -- so equality proves the snapshot is complete.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_snapshot_test hvx_snapshot_test.c
 * Run   (inside L1):  sudo ./hvx_snapshot_test
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
#define ACC_GPA         0x5000UL
#define LOOP_COUNT      20
#define TOTAL_INSNS     (4 * LOOP_COUNT)	/* rdtsc+add+dec+jnz per iter = 80 */
#define SNAP_AT         30			/* snapshot mid-loop */
#define TSC_BASE        0x0000000100000000ULL
#define TSC_STEP        1000ULL

/* loop: rdtsc ; add %rax,(%rbx) ; dec %ecx ; jnz loop ; hlt   (at 0x3000) */
static const unsigned char guest_code[] = {
	0x0f, 0x31,			/* rdtsc */
	0x48, 0x01, 0x03,		/* add %rax,(%rbx) */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xf7,			/* jnz .-9 (back to rdtsc) */
	0xf4,				/* hlt */
};

/* Fresh start: ICOUNT-mode TSC, cleared accumulator, entry registers. */
static int setup(int fd, void *mem)
{
	struct hvx_tsc tsc = { .tsc = TSC_BASE, .step = TSC_STEP, .aux = 0, .mode = HVX_TSC_ICOUNT };
	struct hvx_regs regs;

	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	*(volatile uint64_t *)((unsigned char *)mem + ACC_GPA) = 0;

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rbx    = ACC_GPA;		/* accumulator address */
	regs.rcx    = LOOP_COUNT;	/* loop counter */
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	return ioctl(fd, HVX_SET_REGS, &regs) < 0 ? -1 : 0;
}

/* Single-step to HLT (continuing across the in-kernel cap). reset zeroes icount. */
static int run_to_halt(int fd, int reset, struct hvx_run_ctl *out)
{
	struct hvx_run_ctl ctl;

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

static uint64_t acc(void *mem)
{
	return *(volatile uint64_t *)((unsigned char *)mem + ACC_GPA);
}

int main(void)
{
	int fd, rc = 1;
	void *mem, *saved;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl full, part, cont1, cont2;
	struct hvx_snapshot snap;
	uint64_t acc_full, acc1, acc2;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	saved = malloc(GUEST_MEM_BYTES);
	if (!saved) { perror("malloc"); goto out; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* 1. uninterrupted reference run. */
	if (setup(fd, mem) < 0) goto out;
	if (run_to_halt(fd, 1, &full) < 0) goto out;
	acc_full = acc(mem);
	printf("hvx_snapshot_test: full run: reason=%u icount=%" PRIu64 " acc=0x%" PRIx64 "\n",
	       full.reason, (uint64_t)full.icount, acc_full);
	if (full.reason != EXIT_HLT || full.icount != TOTAL_INSNS) {
		fprintf(stderr, "hvx_snapshot_test: FAIL: reference run reason=%u icount=%" PRIu64
			" (expected HLT, %d)\n", full.reason, (uint64_t)full.icount, TOTAL_INSNS);
		goto out;
	}

	/* 2. stop mid-run, snapshot (software state + guest RAM), finish. */
	if (setup(fd, mem) < 0) goto out;
	memset(&part, 0, sizeof(part));
	part.flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT;
	part.max_insns = SNAP_AT;
	if (ioctl(fd, HVX_RUNC, &part) < 0) { perror("HVX_RUNC"); goto out; }
	if (part.reason != HVX_EXIT_STEP || part.icount != SNAP_AT) {
		fprintf(stderr, "hvx_snapshot_test: FAIL: partial stop reason=%u icount=%" PRIu64
			" (expected STEP at %d)\n", part.reason, (uint64_t)part.icount, SNAP_AT);
		goto out;
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(saved, mem, GUEST_MEM_BYTES);
	printf("hvx_snapshot_test: snapshot at icount=%" PRIu64 " (rip=0x%" PRIx64
	       " rcx=%" PRIu64 " pages=%" PRIu64 ")\n",
	       (uint64_t)snap.icount, (uint64_t)snap.regs.rip,
	       (uint64_t)snap.regs.rcx, (uint64_t)snap.n_guest_pages);
	if (snap.icount != SNAP_AT) {
		fprintf(stderr, "hvx_snapshot_test: FAIL: snapshot icount=%" PRIu64 " != %d\n",
			(uint64_t)snap.icount, SNAP_AT);
		goto out;
	}

	if (run_to_halt(fd, 0, &cont1) < 0) goto out;
	acc1 = acc(mem);
	printf("hvx_snapshot_test: continue A: icount=%" PRIu64 " acc=0x%" PRIx64 "\n",
	       (uint64_t)cont1.icount, acc1);

	/* 3. restore the snapshot and replay the tail again. */
	if (ioctl(fd, HVX_SET_SNAPSHOT, &snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, saved, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &cont2) < 0) goto out;
	acc2 = acc(mem);
	printf("hvx_snapshot_test: continue B: icount=%" PRIu64 " acc=0x%" PRIx64 " (from restored snapshot)\n",
	       (uint64_t)cont2.icount, acc2);

	if (cont1.reason != EXIT_HLT || cont2.reason != EXIT_HLT ||
	    cont1.icount != TOTAL_INSNS || cont2.icount != TOTAL_INSNS) {
		fprintf(stderr, "hvx_snapshot_test: FAIL: continuations did not reach HLT at %d "
			"(A: reason=%u icount=%" PRIu64 "; B: reason=%u icount=%" PRIu64 ")\n",
			TOTAL_INSNS, cont1.reason, (uint64_t)cont1.icount,
			cont2.reason, (uint64_t)cont2.icount);
		goto out;
	}
	if (acc1 != acc_full || acc2 != acc_full) {
		fprintf(stderr, "hvx_snapshot_test: FAIL: acc mismatch (full=0x%" PRIx64
			" A=0x%" PRIx64 " B=0x%" PRIx64 ") -- snapshot incomplete or non-deterministic\n",
			acc_full, acc1, acc2);
		goto out;
	}

	printf("hvx_snapshot_test: MILESTONE 7 PASS: snapshot/restore reproduces the run "
	       "bit-for-bit (acc=0x%" PRIx64 ", icount=%d) from a mid-execution branch point\n",
	       acc_full, TOTAL_INSNS);
	rc = 0;

out:
	free(saved);
	close(fd);
	return rc;
}

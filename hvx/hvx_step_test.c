// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_step_test - prove the MTF instruction clock (milestone 5).
 *
 * The determinism engine needs to advance a guest to an EXACT instruction and
 * count retired instructions reproducibly. hvx now does this with the Monitor
 * Trap Flag: HVX_RUNC single-steps the guest and reports a running icount, and
 * can stop after exactly N instructions.
 *
 * The guest is a fixed-size loop with a statically known instruction count:
 *   mov $100, %ecx          ; 1 instruction
 * loop:
 *   dec %ecx                ; 100x
 *   jnz loop                ; 100x (taken 99 times, falls through once)
 *   hlt                     ; returns to userspace (not counted)
 * => exactly 1 + 100 + 100 = 201 instructions retire before HLT.
 *
 * The test checks: (1) single-stepping to HLT counts exactly 201 instructions,
 * (2) a second run counts identically (determinism), and (3) stopping after
 * exactly 50 instructions lands on instruction 50 and resuming reaches the same
 * total -- i.e. "advance to instruction N" is precise.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_step_test hvx_step_test.c
 * Run   (inside L1):  sudo ./hvx_step_test
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
#define LOOP_COUNT      100
#define EXPECTED_INSNS  (1 + 2 * LOOP_COUNT)	/* mov + LOOP_COUNT*(dec+jnz) = 201 */

/* mov $100,%ecx ; dec %ecx ; jnz -4 ; hlt   (loaded at 0x3000) */
static const unsigned char guest_code[] = {
	0xb9, 0x64, 0x00, 0x00, 0x00,	/* mov $0x64,%ecx */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xfc,			/* jnz .-4 (back to dec) */
	0xf4,				/* hlt */
};

static int set_entry(int fd)
{
	struct hvx_regs regs;

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	return 0;
}

/*
 * Single-step to a real exit, transparently continuing across the in-kernel
 * safety cap (HVX_EXIT_THROTTLE). `reset` zeroes the instruction clock first.
 * Returns the final control block (reason should be EXIT_HLT), or reason 0 on
 * error.
 */
static struct hvx_run_ctl run_to_halt(int fd, int reset)
{
	struct hvx_run_ctl ctl;

	for (;;) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (reset ? HVX_RUN_RESET_ICOUNT : 0);
		ctl.max_insns = 0;			/* run until a real exit / cap */
		reset = 0;				/* only the first sub-call resets */
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); ctl.reason = 0; return ctl; }
		if (ctl.reason == HVX_EXIT_THROTTLE)
			continue;			/* cap hit; keep going */
		return ctl;
	}
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl a, b, part, cont;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* (1) single-step the whole guest; expect exactly EXPECTED_INSNS. */
	if (set_entry(fd) < 0) goto out;
	a = run_to_halt(fd, 1);
	printf("hvx_step_test: run A: reason=%u icount=%" PRIu64 " (expected HLT=%d, %d insns)\n",
	       a.reason, (uint64_t)a.icount, EXIT_HLT, EXPECTED_INSNS);
	if (a.reason != EXIT_HLT) {
		fprintf(stderr, "hvx_step_test: FAIL: run A ended on reason %u, not HLT\n", a.reason);
		goto out;
	}
	if (a.icount != EXPECTED_INSNS) {
		fprintf(stderr, "hvx_step_test: FAIL: counted %" PRIu64 " instructions, expected %d\n",
			(uint64_t)a.icount, EXPECTED_INSNS);
		goto out;
	}

	/* (2) determinism: a second run counts identically. */
	if (set_entry(fd) < 0) goto out;
	b = run_to_halt(fd, 1);
	printf("hvx_step_test: run B: reason=%u icount=%" PRIu64 "\n", b.reason, (uint64_t)b.icount);
	if (b.reason != EXIT_HLT || b.icount != a.icount) {
		fprintf(stderr, "hvx_step_test: FAIL: run B (reason %u, icount %" PRIu64
			") differs from run A (icount %" PRIu64 ")\n",
			b.reason, (uint64_t)b.icount, (uint64_t)a.icount);
		goto out;
	}

	/* (3) stop after exactly 50 instructions, then resume to the same total. */
	if (set_entry(fd) < 0) goto out;
	memset(&part, 0, sizeof(part));
	part.flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT;
	part.max_insns = 50;
	if (ioctl(fd, HVX_RUNC, &part) < 0) { perror("HVX_RUNC"); goto out; }
	printf("hvx_step_test: partial: reason=%u icount=%" PRIu64 " (expected STEP=%u, 50 insns)\n",
	       part.reason, (uint64_t)part.icount, HVX_EXIT_STEP);
	if (part.reason != HVX_EXIT_STEP || part.icount != 50) {
		fprintf(stderr, "hvx_step_test: FAIL: precise stop landed on reason %u icount %" PRIu64
			", expected STEP at 50\n", part.reason, (uint64_t)part.icount);
		goto out;
	}
	cont = run_to_halt(fd, 0);	/* keep the clock running */
	printf("hvx_step_test: resume: reason=%u icount=%" PRIu64 "\n", cont.reason, (uint64_t)cont.icount);
	if (cont.reason != EXIT_HLT || cont.icount != a.icount) {
		fprintf(stderr, "hvx_step_test: FAIL: resume reached reason %u icount %" PRIu64
			", expected HLT at %" PRIu64 "\n",
			cont.reason, (uint64_t)cont.icount, (uint64_t)a.icount);
		goto out;
	}

	printf("hvx_step_test: MILESTONE 5 PASS: MTF instruction clock exact (%d), "
	       "deterministic, and stops precisely at N\n", EXPECTED_INSNS);
	rc = 0;

out:
	close(fd);
	return rc;
}

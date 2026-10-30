// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_intr_test - prove instruction-timed interrupt injection (milestone 8).
 *
 * The guest is a simple loop that increments a counter in memory:
 *   loop:  inc qword [rbx]   ; rbx = 0x5000 (guest counter)
 *          dec %ecx
 *          jnz loop
 *          hlt
 * 10 iterations = 30 instructions, guest counter = 10.
 *
 * The module's built-in interrupt handler (at GPA_ISR) increments a separate
 * counter at 0x2f00 and IRETQs back. The test:
 *   1. baseline (no injection): guest counter 10, handler counter 0, icount 30;
 *   2. inject vector 0x20 at instruction 15: handler runs exactly once (handler
 *      counter 1), the guest still finishes its work (guest counter 10, IRETQ
 *      resumed the loop), and icount grows by the handler's instructions;
 *   3. determinism: injecting again reproduces the exact same icount and state;
 *   4. snapshot carries the pending injection: snapshot BEFORE instruction 15
 *      (injection scheduled but not delivered), finish (it fires), then restore
 *      and finish again -- the restored branch fires the interrupt identically.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_intr_test hvx_intr_test.c
 * Run   (inside L1):  sudo ./hvx_intr_test
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
#define GUEST_CTR_GPA   0x5000UL
#define INTR_CTR_GPA    0x2f00UL		/* must match GPA_INTR_COUNT in hvx.c */
#define VECTOR          0x20
#define ITERS           10
#define BASE_INSNS      (3 * ITERS)		/* inc + dec + jnz per iter = 30 */
#define INJECT_AT       15

/* loop: inc qword [rbx] ; dec %ecx ; jnz loop ; hlt   (at 0x3000) */
static const unsigned char guest_code[] = {
	0x48, 0xff, 0x03,		/* inc qword [rbx] */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xf9,			/* jnz .-7 (back to inc) */
	0xf4,				/* hlt */
};

static uint64_t rd64(void *mem, unsigned long gpa)
{
	return *(volatile uint64_t *)((unsigned char *)mem + gpa);
}

static int setup(int fd, void *mem)
{
	struct hvx_regs regs;

	*(volatile uint64_t *)((unsigned char *)mem + GUEST_CTR_GPA) = 0;
	*(volatile uint64_t *)((unsigned char *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rbx    = GUEST_CTR_GPA;
	regs.rcx    = ITERS;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x202;		/* IF=1: required to inject an external interrupt */
	return ioctl(fd, HVX_SET_REGS, &regs) < 0 ? -1 : 0;
}

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

static int schedule(int fd, uint64_t at)
{
	struct hvx_inject inj = { .vector = VECTOR, .type = 0, .at_icount = at };

	return ioctl(fd, HVX_INJECT, &inj) < 0 ? -1 : 0;
}

int main(void)
{
	int fd, rc = 1;
	void *mem, *saved;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl base, inj, inj2, cont1, cont2;
	struct hvx_snapshot snap;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	saved = malloc(GUEST_MEM_BYTES);
	if (!saved) { perror("malloc"); goto out; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* 1. baseline: no injection. */
	if (setup(fd, mem) < 0) goto out;
	if (run_to_halt(fd, 1, &base) < 0) goto out;
	printf("hvx_intr_test: baseline: icount=%" PRIu64 " guest_ctr=%" PRIu64 " intr_ctr=%" PRIu64 "\n",
	       (uint64_t)base.icount, rd64(mem, GUEST_CTR_GPA), rd64(mem, INTR_CTR_GPA));
	if (base.reason != EXIT_HLT || base.icount != BASE_INSNS ||
	    rd64(mem, GUEST_CTR_GPA) != ITERS || rd64(mem, INTR_CTR_GPA) != 0) {
		fprintf(stderr, "hvx_intr_test: FAIL: baseline off (reason=%u icount=%" PRIu64 ")\n",
			base.reason, (uint64_t)base.icount);
		goto out;
	}

	/* 2. inject vector 0x20 at instruction 15. */
	if (setup(fd, mem) < 0) goto out;
	if (schedule(fd, INJECT_AT) < 0) { perror("HVX_INJECT"); goto out; }
	if (run_to_halt(fd, 1, &inj) < 0) goto out;
	printf("hvx_intr_test: injected: icount=%" PRIu64 " guest_ctr=%" PRIu64 " intr_ctr=%" PRIu64
	       " (baseline icount was %d)\n", (uint64_t)inj.icount, rd64(mem, GUEST_CTR_GPA),
	       rd64(mem, INTR_CTR_GPA), BASE_INSNS);
	if (inj.reason != EXIT_HLT || rd64(mem, INTR_CTR_GPA) != 1 ||
	    rd64(mem, GUEST_CTR_GPA) != ITERS || inj.icount <= base.icount) {
		fprintf(stderr, "hvx_intr_test: FAIL: injection did not deliver cleanly "
			"(reason=%u intr_ctr=%" PRIu64 " guest_ctr=%" PRIu64 " icount=%" PRIu64 ")\n",
			inj.reason, rd64(mem, INTR_CTR_GPA), rd64(mem, GUEST_CTR_GPA),
			(uint64_t)inj.icount);
		goto out;
	}

	/* 3. determinism: same injection -> same icount and state. */
	if (setup(fd, mem) < 0) goto out;
	if (schedule(fd, INJECT_AT) < 0) { perror("HVX_INJECT"); goto out; }
	if (run_to_halt(fd, 1, &inj2) < 0) goto out;
	if (inj2.icount != inj.icount || rd64(mem, INTR_CTR_GPA) != 1 ||
	    rd64(mem, GUEST_CTR_GPA) != ITERS) {
		fprintf(stderr, "hvx_intr_test: FAIL: injection not deterministic "
			"(icount %" PRIu64 " vs %" PRIu64 ")\n",
			(uint64_t)inj2.icount, (uint64_t)inj.icount);
		goto out;
	}

	/* 4. the pending injection is part of the snapshot: snapshot before it
	 * fires, finish, then restore and finish again -> fires identically. */
	if (setup(fd, mem) < 0) goto out;
	if (schedule(fd, INJECT_AT) < 0) { perror("HVX_INJECT"); goto out; }
	{
		struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
					    .max_insns = INJECT_AT - 5 };	/* stop before 15 */
		if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); goto out; }
		if (step.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "hvx_intr_test: FAIL: pre-injection stop reason=%u\n", step.reason);
			goto out;
		}
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(saved, mem, GUEST_MEM_BYTES);
	if (!snap.inject_valid || snap.inject_at_icount != INJECT_AT) {
		fprintf(stderr, "hvx_intr_test: FAIL: snapshot missed pending injection "
			"(valid=%u at=%" PRIu64 ")\n", snap.inject_valid, (uint64_t)snap.inject_at_icount);
		goto out;
	}
	if (run_to_halt(fd, 0, &cont1) < 0) goto out;			/* injection fires */
	if (rd64(mem, INTR_CTR_GPA) != 1 || cont1.icount != inj.icount) {
		fprintf(stderr, "hvx_intr_test: FAIL: continue-after-snapshot diverged\n");
		goto out;
	}
	if (ioctl(fd, HVX_SET_SNAPSHOT, &snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, saved, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &cont2) < 0) goto out;			/* fires again from restore */
	printf("hvx_intr_test: snapshot-carried injection: restored branch icount=%" PRIu64
	       " intr_ctr=%" PRIu64 "\n", (uint64_t)cont2.icount, rd64(mem, INTR_CTR_GPA));
	if (rd64(mem, INTR_CTR_GPA) != 1 || rd64(mem, GUEST_CTR_GPA) != ITERS ||
	    cont2.icount != inj.icount) {
		fprintf(stderr, "hvx_intr_test: FAIL: restored branch did not re-fire the interrupt identically "
			"(icount %" PRIu64 " vs %" PRIu64 ")\n", (uint64_t)cont2.icount, (uint64_t)inj.icount);
		goto out;
	}

	printf("hvx_intr_test: MILESTONE 8 PASS: interrupt delivered at instruction %d, "
	       "deterministic, guest resumed via IRETQ, and the schedule survives snapshot/restore\n",
	       INJECT_AT);
	rc = 0;

out:
	free(saved);
	close(fd);
	return rc;
}

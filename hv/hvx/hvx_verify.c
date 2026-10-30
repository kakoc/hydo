// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_verify - hydo's determinism-verification model, natively on hvx (M9).
 *
 * Mirrors src/verify.rs: a canonical fingerprint over the full VM state and the
 * three equality checks that define "deterministic":
 *   replay      - two fresh runs of the same guest hash identically.
 *   round-trip  - restore(snapshot) then re-snapshot yields the same fingerprint
 *                 (restore is idempotent on the fingerprint).
 *   branch      - step to the midpoint, snapshot; finish -> fp_orig. Restore the
 *                 midpoint; finish -> fp_branch. fp_orig == fp_branch == fp_full.
 *
 * The fingerprint (like hydo's Snapshot::fingerprint) feeds MEANINGFUL fields in
 * a fixed little-endian order -- so incidental padding never enters the hash --
 * then the guest RAM length and bytes. hydo uses BLAKE3; we use a self-contained
 * FNV-1a so this test needs no crypto library. It only has to detect divergence.
 *
 * The guest accumulates instruction-clock-dependent RDTSC values into RAM, so
 * its final state (RAM + regs + icount + tsc) is a sensitive divergence target:
 *   loop: rdtsc ; add %rax,(%rbx) ; dec %ecx ; jnz loop ; hlt
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_verify hvx_verify.c
 * Run   (inside L1):  sudo ./hvx_verify
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
#define TSC_BASE        0x0000000100000000ULL
#define TSC_STEP        1000ULL

static const unsigned char guest_code[] = {
	0x0f, 0x31,			/* rdtsc */
	0x48, 0x01, 0x03,		/* add %rax,(%rbx) */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xf7,			/* jnz .-9 */
	0xf4,				/* hlt */
};

/* ---- canonical fingerprint (FNV-1a over meaningful fields + guest RAM) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;

	for (i = 0; i < n; i++) {
		h ^= b[i];
		h *= 0x100000001b3ULL;
	}
	return h;
}

static uint64_t fingerprint(int fd, void *mem)
{
	struct hvx_snapshot s;
	uint64_t ram_len = GUEST_MEM_BYTES;
	uint64_t h = 0xcbf29ce484222325ULL;	/* FNV offset basis */

	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }

	h = fnv(h, "hvx-state-v1", 12);
	h = fnv(h, &s.regs, sizeof(s.regs));	/* 18 u64: rax..r15, rip, rsp, rflags */
	h = fnv(h, &s.tsc, sizeof(s.tsc));
	h = fnv(h, &s.tsc_step, sizeof(s.tsc_step));
	h = fnv(h, &s.tsc_aux, sizeof(s.tsc_aux));
	h = fnv(h, &s.tsc_mode, sizeof(s.tsc_mode));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.rdtsc_count, sizeof(s.rdtsc_count));
	h = fnv(h, &s.inject_at_icount, sizeof(s.inject_at_icount));
	h = fnv(h, &s.inject_vector, sizeof(s.inject_vector));
	h = fnv(h, &s.inject_valid, sizeof(s.inject_valid));
	h = fnv(h, s.sw_msr, sizeof(s.sw_msr));	/* curated software MSR store (M12) */
	h = fnv(h, &s.sregs, sizeof(s.sregs));	/* privileged register state (M13) */
	h = fnv(h, &ram_len, sizeof(ram_len));	/* length-prefix, then the RAM */
	h = fnv(h, mem, GUEST_MEM_BYTES);
	return h;
}

/* ---- driving the guest ---- */
static int setup(int fd, void *mem)
{
	struct hvx_tsc tsc = { .tsc = TSC_BASE, .step = TSC_STEP, .aux = 0, .mode = HVX_TSC_ICOUNT };
	struct hvx_regs regs;

	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	*(volatile uint64_t *)((unsigned char *)mem + ACC_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rbx    = ACC_GPA;
	regs.rcx    = LOOP_COUNT;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
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
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}

int main(void)
{
	int fd, rc = 1;
	void *mem, *saved, *savedmid;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl r;
	struct hvx_snapshot snap, mid;
	uint64_t fp_a, fp_b, fp_round, fp_orig, fp_branch;
	int ok_replay, ok_round, ok_branch;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	saved = malloc(GUEST_MEM_BYTES);
	savedmid = malloc(GUEST_MEM_BYTES);
	if (!saved || !savedmid) { perror("malloc"); goto out; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* replay: two fresh runs hash identically. */
	if (setup(fd, mem) < 0 || run_to_halt(fd, 1, &r) < 0) goto out;
	fp_a = fingerprint(fd, mem);
	if (setup(fd, mem) < 0 || run_to_halt(fd, 1, &r) < 0) goto out;
	fp_b = fingerprint(fd, mem);
	ok_replay = (fp_a == fp_b);
	printf("hvx_verify: replay     fp_a=%016" PRIx64 " fp_b=%016" PRIx64 " -> %s\n",
	       fp_a, fp_b, ok_replay ? "OK" : "MISMATCH");

	/* round-trip: restore(snapshot) then re-snapshot -> same fingerprint. */
	if (ioctl(fd, HVX_GET_SNAPSHOT, &snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(saved, mem, GUEST_MEM_BYTES);
	if (ioctl(fd, HVX_SET_SNAPSHOT, &snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, saved, GUEST_MEM_BYTES);
	fp_round = fingerprint(fd, mem);
	ok_round = (fp_round == fp_b);
	printf("hvx_verify: round-trip fp=%016" PRIx64 " (vs %016" PRIx64 ") -> %s\n",
	       fp_round, fp_b, ok_round ? "OK" : "MISMATCH");

	/* branch: step to the midpoint, snapshot, finish; restore, finish. */
	if (setup(fd, mem) < 0) goto out;
	{
		struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
					    .max_insns = TOTAL_INSNS / 2 };
		if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); goto out; }
		if (step.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "hvx_verify: FAIL: midpoint stop reason=%u\n", step.reason);
			goto out;
		}
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(savedmid, mem, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem);
	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, savedmid, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem);
	ok_branch = (fp_orig == fp_branch && fp_branch == fp_a);
	printf("hvx_verify: branch     fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a,
	       ok_branch ? "OK" : "MISMATCH");

	if (ok_replay && ok_round && ok_branch) {
		printf("hvx_verify: MILESTONE 9 PASS: replay / round-trip / branch all "
		       "fingerprint-equal -- hvx is deterministic under its own verify model\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_verify: FAIL: a determinism check did not hold\n");
	}

out:
	free(saved);
	free(savedmid);
	close(fd);
	return rc;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_smp_test - prove DETERMINISTIC multi-vCPU (milestone 19).
 *
 * You cannot run vCPUs in parallel on physical cores and be deterministic --
 * real concurrent memory interleaving is nondeterministic. hvx instead
 * SERIALIZES the vCPUs onto one execution context and interleaves them by a
 * DETERMINISTIC QUANTUM measured in the MTF instruction clock: run vCPU0 for Q
 * instructions (single-step), then vCPU1 for Q, round-robin. The interleaving is
 * then a pure function of the per-vCPU instruction counts, so it reproduces
 * bit-for-bit; and CHANGING Q changes the interleaving in a still-reproducible
 * way. The schedule is driven entirely from userspace (HVX_SET_VCPU + HVX_RUNC).
 *
 * Part 1 -- shared-counter race (the core proof).
 *   Two vCPUs share a counter at ACC_GPA and each does K NON-ATOMIC increments:
 *       loop: mov (%rbx),%eax ; inc %eax ; mov %eax,(%rbx) ; dec %ecx ; jnz loop ; hlt
 *   The load/modify/store can straddle a quantum boundary, so how the two vCPUs
 *   interleave decides how many updates are lost -- a function of Q:
 *     - Q >= 5K  (serialized): vCPU0 finishes before vCPU1 starts -> counter = 2K.
 *     - Q == 1   (finest interleave, lockstep): every pair of iterations nets one
 *       increment (one update lost each time) -> counter = K.
 *   Asserted: (a) same Q run twice -> byte-identical fingerprint (regs of both
 *   vCPUs + shared RAM); (b) different Q -> different (but each reproducible)
 *   fingerprint AND different counter (2K vs K), proving the schedule
 *   deterministically controls the result.
 *
 * Part 2 -- cross-vCPU IPI.
 *   vCPU0 writes the x2APIC ICR (MSR 0x830) to send vector 0x20 to APIC ID 1;
 *   the module latches a deterministic instruction-timed injection on vCPU1.
 *   vCPU1 runs an interruptible loop; the module's built-in ISR (inc [INTR_CTR];
 *   iretq) fires exactly once, and vCPU1 finishes its own work via IRETQ.
 *   Asserted: (c) the target's handler ran exactly once at a reproducible point
 *   (INTR_CTR == 1, vCPU1 loop counter == ITERS, fingerprint reproducible).
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_smp_test hvx_smp_test.c
 * Run   (inside L1):  sudo ./hvx_smp_test
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
#define N_VCPUS         2

/* Part 1 */
#define P1_CODE         HVX_GUEST_CODE_BASE	/* 0x3000: both vCPUs run this */
#define ACC_GPA         0x5000UL		/* shared, contended counter */
#define K               20			/* increments per vCPU */

/* Part 2 */
#define V0_CODE         0x3000UL		/* IPI sender */
#define V1_CODE         0x4000UL		/* interruptible loop (own code region) */
#define VCPU1_CTR       0x5000UL		/* vCPU1's own progress counter */
#define INTR_CTR        0x2f00UL		/* must match GPA_INTR_COUNT in hvx.c */
#define IPI_VECTOR      0x20
#define ITERS           20

/* loop: mov (%rbx),%eax ; inc %eax ; mov %eax,(%rbx) ; dec %ecx ; jnz .-10 ; hlt */
static const unsigned char race_code[] = {
	0x8b, 0x03,			/* mov (%rbx),%eax */
	0xff, 0xc0,			/* inc %eax */
	0x89, 0x03,			/* mov %eax,(%rbx) */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xf6,			/* jnz .-10 (back to the load) */
	0xf4,				/* hlt */
};

/* vCPU0: send a fixed IPI (vector 0x20) to x2APIC ID 1 via the ICR, then hlt.
 *   mov $0x830,%ecx ; mov $0x20,%eax ; mov $1,%edx ; wrmsr ; hlt
 * EAX=0x20 -> vector 0x20, delivery mode fixed (0), no shorthand; EDX=1 -> the
 * x2APIC physical destination (vCPU index 1). */
static const unsigned char ipi_send_code[] = {
	0xb9, 0x30, 0x08, 0x00, 0x00,	/* mov $0x830,%ecx  (IA32_X2APIC_ICR) */
	0xb8, 0x20, 0x00, 0x00, 0x00,	/* mov $0x20,%eax   (vector, fixed) */
	0xba, 0x01, 0x00, 0x00, 0x00,	/* mov $1,%edx      (dest APIC ID 1) */
	0x0f, 0x30,			/* wrmsr */
	0xf4,				/* hlt */
};

/* vCPU1: an interruptible loop that increments its own counter, then hlt.
 *   loop: inc qword (%rbx) ; dec %ecx ; jnz .-7 ; hlt */
static const unsigned char loop_code[] = {
	0x48, 0xff, 0x03,		/* inc qword (%rbx) */
	0xff, 0xc9,			/* dec %ecx */
	0x75, 0xf9,			/* jnz .-7 (back to inc) */
	0xf4,				/* hlt */
};

/* ---- canonical fingerprint (FNV-1a over both vCPUs' regs+icount + shared RAM) ---- */
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

static int set_vcpu(int fd, uint32_t i)
{
	return ioctl(fd, HVX_SET_VCPU, &i);
}

static uint64_t fingerprint(int fd, void *mem, int nv)
{
	uint64_t h = 0xcbf29ce484222325ULL;	/* FNV offset basis */
	int i;

	h = fnv(h, "hvx-smp-v1", 10);
	for (i = 0; i < nv; i++) {
		struct hvx_snapshot s;

		if (set_vcpu(fd, i) < 0) { perror("HVX_SET_VCPU"); return 0; }
		if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }
		h = fnv(h, &s.regs, sizeof(s.regs));	/* rax..r15, rip, rsp, rflags */
		h = fnv(h, &s.icount, sizeof(s.icount));
	}
	h = fnv(h, mem, GUEST_MEM_BYTES);		/* the SHARED guest RAM */
	return h;
}

static int set_regs(int fd, uint32_t vcpu, uint64_t rip, uint64_t rbx,
		    uint64_t rcx, uint64_t rflags)
{
	struct hvx_regs r;

	memset(&r, 0, sizeof(r));
	r.rip = rip;
	r.rbx = rbx;
	r.rcx = rcx;
	r.rsp = GUEST_MEM_BYTES - 16;
	r.rflags = rflags;
	if (set_vcpu(fd, vcpu) < 0) { perror("HVX_SET_VCPU"); return -1; }
	if (ioctl(fd, HVX_SET_REGS, &r) < 0) { perror("HVX_SET_REGS"); return -1; }
	return 0;
}

/*
 * Deterministic round-robin: run each not-yet-halted vCPU for Q instructions
 * (MTF single-step) in index order, repeatedly, until every vCPU has HLTed. The
 * schedule is a pure function of Q and the guests' code, so it reproduces.
 */
static int round_robin(int fd, int nv, uint64_t q)
{
	int done[N_VCPUS] = { 0 };
	int remaining = nv, i;
	long guard = 0;

	while (remaining) {
		for (i = 0; i < nv; i++) {
			struct hvx_run_ctl ctl;

			if (done[i])
				continue;
			if (set_vcpu(fd, i) < 0) { perror("HVX_SET_VCPU"); return -1; }
			memset(&ctl, 0, sizeof(ctl));
			ctl.flags = HVX_RUN_SINGLE_STEP;	/* MTF quantum; icount persists */
			ctl.max_insns = q;
			if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }

			if (ctl.reason == HVX_EXIT_STEP || ctl.reason == HVX_EXIT_THROTTLE) {
				/* ran its quantum (or hit the in-kernel cap): keep going */
			} else if (ctl.reason == EXIT_HLT) {
				done[i] = 1;
				remaining--;
			} else {
				fprintf(stderr, "hvx_smp_test: vcpu%d unexpected exit reason=%u "
					"qual=0x%" PRIx64 " gpa=0x%" PRIx64 "\n",
					i, ctl.reason, (uint64_t)ctl.qualification,
					(uint64_t)ctl.guest_phys);
				return -1;
			}
		}
		if (++guard > 100000000L) {
			fprintf(stderr, "hvx_smp_test: runaway schedule (q=%" PRIu64 ")\n", q);
			return -1;
		}
	}
	return 0;
}

static void *create_vm(int *out_fd)
{
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES, .n_vcpus = N_VCPUS };
	void *mem;
	int fd = open("/dev/hvx", O_RDWR);

	if (fd < 0) { perror("open /dev/hvx"); return NULL; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); close(fd); return NULL; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); close(fd); return NULL; }
	*out_fd = fd;
	return mem;
}

/* Part 1: run the shared-counter race with quantum q; report fingerprint+counter. */
static int scenario_race(uint64_t q, uint64_t *fp, uint64_t *counter)
{
	int fd = -1;
	void *mem = create_vm(&fd);

	if (!mem)
		return -1;
	memcpy((unsigned char *)mem + P1_CODE, race_code, sizeof(race_code));
	*(volatile uint64_t *)((unsigned char *)mem + ACC_GPA) = 0;

	if (set_regs(fd, 0, P1_CODE, ACC_GPA, K, 0x2) < 0) goto err;
	if (set_regs(fd, 1, P1_CODE, ACC_GPA, K, 0x2) < 0) goto err;
	if (round_robin(fd, N_VCPUS, q) < 0) goto err;

	*counter = *(volatile uint64_t *)((unsigned char *)mem + ACC_GPA);
	*fp = fingerprint(fd, mem, N_VCPUS);
	munmap(mem, GUEST_MEM_BYTES);
	close(fd);
	return 0;
err:
	munmap(mem, GUEST_MEM_BYTES);
	close(fd);
	return -1;
}

/* Part 2: vCPU0 IPIs vCPU1; report fingerprint + handler-count + vCPU1 progress. */
static int scenario_ipi(uint64_t *fp, uint64_t *intr, uint64_t *v1ctr)
{
	int fd = -1;
	void *mem = create_vm(&fd);

	if (!mem)
		return -1;
	memcpy((unsigned char *)mem + V0_CODE, ipi_send_code, sizeof(ipi_send_code));
	memcpy((unsigned char *)mem + V1_CODE, loop_code, sizeof(loop_code));
	*(volatile uint64_t *)((unsigned char *)mem + VCPU1_CTR) = 0;
	*(volatile uint64_t *)((unsigned char *)mem + INTR_CTR) = 0;

	/* vCPU0 is the sender (IF irrelevant); vCPU1 must be interruptible (IF=1). */
	if (set_regs(fd, 0, V0_CODE, 0, 0, 0x2) < 0) goto err;
	if (set_regs(fd, 1, V1_CODE, VCPU1_CTR, ITERS, 0x202) < 0) goto err;
	if (round_robin(fd, N_VCPUS, 100) < 0) goto err;

	*intr  = *(volatile uint64_t *)((unsigned char *)mem + INTR_CTR);
	*v1ctr = *(volatile uint64_t *)((unsigned char *)mem + VCPU1_CTR);
	*fp = fingerprint(fd, mem, N_VCPUS);
	munmap(mem, GUEST_MEM_BYTES);
	close(fd);
	return 0;
err:
	munmap(mem, GUEST_MEM_BYTES);
	close(fd);
	return -1;
}

int main(void)
{
	uint64_t fpS1, fpS2, cntS1, cntS2;	/* serialized (Q = 200) */
	uint64_t fpF1, fpF2, cntF1, cntF2;	/* finest (Q = 1) */
	uint64_t fpI1, fpI2, intr1, intr2, v1a, v1b;
	int ok_repro, ok_schedule, ok_ipi;

	/* ---- Part 1: shared-counter race ---- */
	if (scenario_race(200, &fpS1, &cntS1) < 0) return 1;	/* Q >= 5K -> serialized */
	if (scenario_race(200, &fpS2, &cntS2) < 0) return 1;
	if (scenario_race(1,   &fpF1, &cntF1) < 0) return 1;	/* Q == 1 -> finest interleave */
	if (scenario_race(1,   &fpF2, &cntF2) < 0) return 1;

	printf("hvx_smp_test: race Q=200 (serialized): counter=%" PRIu64 " (expect %d) fp=%016" PRIx64
	       " / %016" PRIx64 "\n", cntS1, 2 * K, fpS1, fpS2);
	printf("hvx_smp_test: race Q=1   (interleaved): counter=%" PRIu64 " (expect %d) fp=%016" PRIx64
	       " / %016" PRIx64 "\n", cntF1, K, fpF1, fpF2);

	/* (a) reproducibility: same Q run twice is byte-identical. */
	ok_repro = (fpS1 == fpS2) && (fpF1 == fpF2) && (cntS1 == cntS2) && (cntF1 == cntF2);
	/* (b) the quantum deterministically controls the interleaving: different Q ->
	 *     different final state (and here the exact predicted counters 2K vs K). */
	ok_schedule = (fpS1 != fpF1) && (cntS1 == (uint64_t)(2 * K)) && (cntF1 == (uint64_t)K);

	printf("hvx_smp_test: (a) reproducible: %s   (b) schedule-controls-result: %s\n",
	       ok_repro ? "OK" : "MISMATCH", ok_schedule ? "OK" : "FAIL");

	/* ---- Part 2: cross-vCPU IPI ---- */
	if (scenario_ipi(&fpI1, &intr1, &v1a) < 0) return 1;
	if (scenario_ipi(&fpI2, &intr2, &v1b) < 0) return 1;
	printf("hvx_smp_test: IPI: handler_count=%" PRIu64 " vcpu1_progress=%" PRIu64
	       " (expect 1, %d) fp=%016" PRIx64 " / %016" PRIx64 "\n",
	       intr1, v1a, ITERS, fpI1, fpI2);
	ok_ipi = (intr1 == 1) && (v1a == (uint64_t)ITERS) && (fpI1 == fpI2) &&
		 (intr2 == 1) && (v1b == (uint64_t)ITERS);
	printf("hvx_smp_test: (c) cross-vCPU IPI delivered deterministically: %s\n",
	       ok_ipi ? "OK" : "FAIL");

	if (ok_repro && ok_schedule && ok_ipi) {
		printf("hvx_smp_test: MILESTONE 19 PASS: deterministic multi-vCPU -- serialized "
		       "execution + instruction-quantum round-robin reproduces the interleaving "
		       "(and the quantum controls it), plus cross-vCPU IPI delivery is deterministic\n");
		return 0;
	}
	fprintf(stderr, "hvx_smp_test: FAIL: a determinism assertion did not hold\n");
	return 1;
}

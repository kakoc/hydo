// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_rdtsc_test - prove deterministic RDTSC interception (milestone 4).
 *
 * Under stock KVM, a userspace VMM cannot make RDTSC a function of anything but
 * the host time-stamp counter -- which is exactly where hydo's determinism wall
 * was (RDTSC in native_calibrate_cpu_early during Linux boot). Now that hvx owns
 * the VM-execution controls, it sets "RDTSC exiting" and returns a virtual TSC
 * we control.
 *
 * This test loads a guest that executes RDTSC three times into memory, then
 * HLTs. It checks that:
 *   1. the returned values are the deterministic sequence seed, seed+step,
 *      seed+2*step (NOT the huge, ever-changing host TSC), and
 *   2. running the guest a second time from the same seed reproduces the exact
 *      same values -- determinism, independent of wall-clock time.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_rdtsc_test hvx_rdtsc_test.c
 * Run   (inside L1):  sudo ./hvx_rdtsc_test
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
#define RESULT_GPA      0x5000UL		/* guest stores three u64 TSCs here */
#define TSC_SEED        0x0000000100000000ULL	/* high dword set: exercises EDX */
#define TSC_STEP        0x0000000000000100ULL

/*
 * Guest machine code, loaded at HVX_GUEST_CODE_BASE (0x3000):
 *   movabs $0x5000, %rbx            ; result base (GPA == GVA)
 *   rdtsc ; mov %eax,0x00(%rbx) ; mov %edx,0x04(%rbx)   ; TSC #0 -> [0x5000]
 *   rdtsc ; mov %eax,0x08(%rbx) ; mov %edx,0x0c(%rbx)   ; TSC #1 -> [0x5008]
 *   rdtsc ; mov %eax,0x10(%rbx) ; mov %edx,0x14(%rbx)   ; TSC #2 -> [0x5010]
 *   hlt
 */
static const unsigned char guest_code[] = {
	0x48, 0xbb, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0x0f, 0x31, 0x89, 0x43, 0x00, 0x89, 0x53, 0x04,
	0x0f, 0x31, 0x89, 0x43, 0x08, 0x89, 0x53, 0x0c,
	0x0f, 0x31, 0x89, 0x43, 0x10, 0x89, 0x53, 0x14,
	0xf4,
};

/* Run the guest to HLT from a fresh seed; return the three stored TSC values. */
static int run_once(int fd, void *mem, uint64_t out[3])
{
	struct hvx_tsc tsc = { .tsc = TSC_SEED, .step = TSC_STEP, .aux = 0 };
	struct hvx_regs regs;
	struct hvx_exit ex;
	int i;

	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	memset((unsigned char *)mem + RESULT_GPA, 0, 3 * sizeof(uint64_t));

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }

	for (i = 0; i < 8; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT)
			break;
		fprintf(stderr, "run_once: unexpected exit reason=%u qual=0x%" PRIx64 "\n",
			ex.reason, (uint64_t)ex.qualification);
		return -1;
	}

	for (i = 0; i < 3; i++)
		out[i] = *(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA + i * 8);
	return 0;
}

int main(void)
{
	int fd, i, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_tsc got;
	uint64_t a[3], b[3];
	uint64_t expect[3] = { TSC_SEED, TSC_SEED + TSC_STEP, TSC_SEED + 2 * TSC_STEP };

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	if (run_once(fd, mem, a) < 0) goto out;
	if (ioctl(fd, HVX_GET_TSC, &got) < 0) { perror("HVX_GET_TSC"); goto out; }
	printf("hvx_rdtsc_test: run A: tsc[0]=0x%016" PRIx64 " tsc[1]=0x%016" PRIx64
	       " tsc[2]=0x%016" PRIx64 " (rdtsc exits handled: %" PRIu64 ")\n",
	       a[0], a[1], a[2], (uint64_t)got.count);

	for (i = 0; i < 3; i++) {
		if (a[i] != expect[i]) {
			fprintf(stderr, "hvx_rdtsc_test: FAIL: tsc[%d]=0x%" PRIx64
				" but expected deterministic 0x%" PRIx64 "\n",
				i, a[i], expect[i]);
			goto out;
		}
	}
	if (got.count != 3) {
		fprintf(stderr, "hvx_rdtsc_test: FAIL: handled %" PRIu64 " rdtsc exits, expected 3\n",
			(uint64_t)got.count);
		goto out;
	}

	if (run_once(fd, mem, b) < 0) goto out;
	printf("hvx_rdtsc_test: run B: tsc[0]=0x%016" PRIx64 " tsc[1]=0x%016" PRIx64
	       " tsc[2]=0x%016" PRIx64 "\n", b[0], b[1], b[2]);

	for (i = 0; i < 3; i++) {
		if (b[i] != a[i]) {
			fprintf(stderr, "hvx_rdtsc_test: FAIL: run B tsc[%d]=0x%" PRIx64
				" != run A 0x%" PRIx64 " (non-deterministic!)\n",
				i, b[i], a[i]);
			goto out;
		}
	}

	printf("hvx_rdtsc_test: MILESTONE 4 PASS: RDTSC intercepted, deterministic "
	       "virtual TSC returned, two runs bit-identical\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

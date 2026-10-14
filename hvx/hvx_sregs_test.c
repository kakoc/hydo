// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sregs_test - prove privileged-state persistence + HVX_SET_SREGS (M13).
 *
 * Before M13, setup_vmcs rebuilt CR0/CR3/CR4/EFER/segments/GDTR/IDTR from
 * hardcoded flat defaults on every VM entry, so anything the guest changed in
 * that state was lost the moment an HVX_RUN returned. M13 keeps a software copy
 * (struct hvx_sregs) that the module loads from on entry and saves back on exit,
 * so a guest that reprograms CR3 keeps the new page-table root across the ioctl
 * boundary -- a prerequisite for running an OS that switches address spaces.
 *
 * The test builds a SECOND identity page table (PT2) in guest RAM, then:
 *   1. runs guest A: `mov cr3, PT2; hlt`   -> switches to PT2 and halts;
 *   2. HVX_GET_SREGS shows cr3 == PT2 (the module captured the change);
 *   3. runs guest B: `mov rax,cr3; store; hlt` WITHOUT touching sregs -- it must
 *      start under PT2 (proving CR3 persisted across the HVX_RUN boundary), so
 *      the stored value is PT2, not the default 0;
 *   4. HVX_SET_SREGS forces cr3 back to 0 (PT1); guest B now reads 0 -- proving
 *      the userspace-set privileged state takes effect.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_sregs_test hvx_sregs_test.c
 * Run   (inside L1):  sudo ./hvx_sregs_test
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
#define PT2_PML4        0x6000UL	/* second page-table root (identity map) */
#define PT2_PDPT        0x7000UL
#define PT2_PD          0x8000UL
#define GUEST_A_GPA     0x3000UL
#define GUEST_B_GPA     0x3100UL

/* guest A: mov rax, PT2_PML4 ; mov cr3, rax ; hlt */
static const unsigned char guest_a[] = {
	0x48, 0xb8, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* mov rax,0x6000 */
	0x0f, 0x22, 0xd8,		/* mov cr3, rax */
	0xf4,				/* hlt */
};

/* guest B: mov rbx, 0x5000 ; mov rax, cr3 ; mov [rbx], rax ; hlt */
static const unsigned char guest_b[] = {
	0x48, 0xbb, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* mov rbx,0x5000 */
	0x0f, 0x20, 0xd8,		/* mov rax, cr3 */
	0x48, 0x89, 0x03,		/* mov [rbx], rax */
	0xf4,				/* hlt */
};

/* Build an identity 4-level page table (low 1 GiB, 2 MiB pages) at the given
 * guest-physical addresses -- a clone of the module's own PT1, but at PT2. */
static void build_pt2(void *mem)
{
	uint64_t *pml4 = (uint64_t *)((unsigned char *)mem + PT2_PML4);
	uint64_t *pdpt = (uint64_t *)((unsigned char *)mem + PT2_PDPT);
	uint64_t *pd   = (uint64_t *)((unsigned char *)mem + PT2_PD);
	int j;

	pml4[0] = PT2_PDPT | 0x3;	/* present | write */
	pdpt[0] = PT2_PD   | 0x3;
	for (j = 0; j < 512; j++)
		pd[j] = ((uint64_t)j << 21) | 0x83;	/* 2 MiB PS page, present|write */
}

/* Run to HLT from entry point `rip` via plain HVX_RUN; sregs is untouched. */
static int run_from(int fd, uint64_t rip)
{
	struct hvx_regs regs;
	struct hvx_exit ex;
	int i;

	memset(&regs, 0, sizeof(regs));
	regs.rip    = rip;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }

	for (i = 0; i < 8; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT)
			return 0;
		fprintf(stderr, "run_from: unexpected exit reason=%u qual=0x%" PRIx64
			" gpa=0x%" PRIx64 "\n", ex.reason,
			(uint64_t)ex.qualification, (uint64_t)ex.guest_phys);
		return -1;
	}
	fprintf(stderr, "run_from: never reached HLT\n");
	return -1;
}

static uint64_t result(void *mem)
{
	return *(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA);
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_sregs sr;
	uint64_t got;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }

	/* fresh GET_SREGS should report the seeded flat long-mode defaults. */
	if (ioctl(fd, HVX_GET_SREGS, &sr) < 0) { perror("HVX_GET_SREGS"); goto out; }
	printf("hvx_sregs_test: seeded sregs: cr3=0x%" PRIx64 " cr0=0x%" PRIx64
	       " efer=0x%" PRIx64 " cs.sel=0x%x cs.ar=0x%x\n",
	       (uint64_t)sr.cr3, (uint64_t)sr.cr0, (uint64_t)sr.efer,
	       sr.cs.selector, sr.cs.ar);
	if (sr.cr3 != 0 || sr.cs.selector != 0x08 || sr.tr.selector != 0x18) {
		fprintf(stderr, "hvx_sregs_test: FAIL: seeded sregs unexpected\n");
		goto out;
	}

	build_pt2(mem);
	memcpy((unsigned char *)mem + GUEST_A_GPA, guest_a, sizeof(guest_a));
	memcpy((unsigned char *)mem + GUEST_B_GPA, guest_b, sizeof(guest_b));

	/* 1. guest A switches CR3 to PT2 and halts. */
	if (run_from(fd, GUEST_A_GPA) < 0) goto out;

	/* 2. the module captured the new CR3 into sregs. */
	if (ioctl(fd, HVX_GET_SREGS, &sr) < 0) { perror("HVX_GET_SREGS"); goto out; }
	printf("hvx_sregs_test: after guest A: sregs.cr3=0x%" PRIx64 " (expected 0x%lx)\n",
	       (uint64_t)sr.cr3, PT2_PML4);
	if (sr.cr3 != PT2_PML4) {
		fprintf(stderr, "hvx_sregs_test: FAIL: CR3 change not captured (0x%" PRIx64 ")\n",
			(uint64_t)sr.cr3);
		goto out;
	}

	/* 3. guest B, WITHOUT re-setting sregs, must start under PT2 -> reads PT2. */
	*(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA) = 0;
	if (run_from(fd, GUEST_B_GPA) < 0) goto out;
	got = result(mem);
	printf("hvx_sregs_test: guest B read CR3 = 0x%" PRIx64
	       " (persisted across HVX_RUN boundary)\n", got);
	if (got != PT2_PML4) {
		fprintf(stderr, "hvx_sregs_test: FAIL: CR3 did not persist (guest B saw 0x%" PRIx64
			", expected 0x%lx)\n", got, PT2_PML4);
		goto out;
	}

	/* 4. HVX_SET_SREGS forces CR3 back to PT1 (0); guest B now reads 0. */
	if (ioctl(fd, HVX_GET_SREGS, &sr) < 0) { perror("HVX_GET_SREGS"); goto out; }
	sr.cr3 = 0;
	if (ioctl(fd, HVX_SET_SREGS, &sr) < 0) { perror("HVX_SET_SREGS"); goto out; }
	*(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA) = 0xdead;
	if (run_from(fd, GUEST_B_GPA) < 0) goto out;
	got = result(mem);
	printf("hvx_sregs_test: after HVX_SET_SREGS(cr3=0): guest B read CR3 = 0x%" PRIx64 "\n", got);
	if (got != 0) {
		fprintf(stderr, "hvx_sregs_test: FAIL: HVX_SET_SREGS(cr3=0) not honored "
			"(guest B saw 0x%" PRIx64 ")\n", got);
		goto out;
	}

	printf("hvx_sregs_test: MILESTONE 13 PASS: guest CR3 switch persisted across an "
	       "HVX_RUN boundary and HVX_SET_SREGS drives privileged state\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

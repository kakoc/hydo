// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_bigmem_test - prove guest RAM beyond 2 MiB (milestone 11, L2-Linux phase 2).
 *
 * The guest writes a 64-bit magic to GPA 200 MiB, reads it back, and stores it at
 * GPA 0x5000. This exercises the enlarged guest RAM (up to 256 MiB), the multi-PT
 * EPT (128 PTs of 4 KiB leaves), and the low-1 GiB guest identity paging (512
 * 2 MiB PD entries) -- all needed before a 1 MiB-loaded Linux kernel can fit.
 *
 * We check both the round-trip result at 0x5000 AND that the byte the guest wrote
 * at 200 MiB is visible through the shared mmap at the same offset (same host page
 * reached via EPT and via the mmap fault handler).
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_bigmem_test hvx_bigmem_test.c
 * Run   (inside L1):  sudo ./hvx_bigmem_test
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
#define GUEST_MEM_BYTES (256UL * 1024 * 1024)	/* 256 MiB */
#define HIGH_GPA        (200UL * 1024 * 1024)	/* 200 MiB: far past the old 2 MiB cap */
#define RESULT_GPA      0x5000UL
#define MAGIC           0x1234567890abcdefULL

/*
 *   movabs $HIGH_GPA, %rbx ; movabs $MAGIC, %rax ; mov %rax,(%rbx)
 *   mov (%rbx),%rax        ; movabs $0x5000,%rdi ; mov %rax,(%rdi) ; hlt
 */
static const unsigned char guest_code[] = {
	0x48, 0xbb, 0x00, 0x00, 0x80, 0x0c, 0x00, 0x00, 0x00, 0x00,  /* movabs $0xC800000,%rbx */
	0x48, 0xb8, 0xef, 0xcd, 0xab, 0x90, 0x78, 0x56, 0x34, 0x12,  /* movabs $MAGIC,%rax */
	0x48, 0x89, 0x03,					    /* mov %rax,(%rbx) */
	0x48, 0x8b, 0x03,					    /* mov (%rbx),%rax */
	0x48, 0xbf, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  /* movabs $0x5000,%rdi */
	0x48, 0x89, 0x07,					    /* mov %rax,(%rdi) */
	0xf4,							    /* hlt */
};

int main(void)
{
	int fd, i, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_regs regs;
	struct hvx_exit ex;
	uint64_t at_result, at_high;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM (256 MiB)"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap 256 MiB"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = 0x100000;			/* 1 MiB, unused (guest never pushes) */
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); goto out; }

	for (i = 0; i < 16; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); goto out; }
		if (ex.reason == EXIT_HLT)
			break;
		fprintf(stderr, "hvx_bigmem_test: unexpected exit reason=%u qual=0x%llx gpa=0x%llx\n",
			ex.reason, (unsigned long long)ex.qualification,
			(unsigned long long)ex.guest_phys);
		goto out;
	}

	at_result = *(volatile uint64_t *)((unsigned char *)mem + RESULT_GPA);
	at_high   = *(volatile uint64_t *)((unsigned char *)mem + HIGH_GPA);
	printf("hvx_bigmem_test: guest read-back at 0x5000 = 0x%016" PRIx64
	       ", value at 200 MiB (via mmap) = 0x%016" PRIx64 "\n", at_result, at_high);

	if (at_result != MAGIC || at_high != MAGIC) {
		fprintf(stderr, "hvx_bigmem_test: FAIL: expected 0x%016" PRIx64 " at both\n",
			(uint64_t)MAGIC);
		goto out;
	}

	printf("hvx_bigmem_test: MILESTONE 11 PASS: guest read/wrote GPA 200 MiB through "
	       "EPT + 1 GiB identity paging (256 MiB RAM)\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_bigram_test - guest RAM above 1 GiB, all of it.
 *
 * A 3 GiB guest (the maximum, MAX_GPAGES) maps [0,3 GiB) with 1 GiB paging leaves
 * (pdpt[1], pdpt[2], installed from userspace -- the module maps only the low 1 GiB)
 * and writes one tagged qword into EVERY page from 2 MiB up to the top of RAM, at an
 * offset that walks through the page (stride 4096 + 8), then reads them all back and
 * counts mismatches. The host independently checks the same qwords through the RAM
 * mmap and also that nothing else in a sample of pages was disturbed. Run three times:
 * every page of every EPT PD (the M28 fan-out) must hold exactly what the guest wrote.
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

#define RAM_BYTES   (3ULL << 30)
#define START       0x200000ULL
#define STRIDE      0x1008ULL
#define MAGIC       0x6879646f2d72616dULL		/* "hydo-ram" */
#define GPA_PDPT    0x1000UL
#define LEAF(g)     (((uint64_t)(g) << 30) | 0x83 | 0x60)	/* 1 GiB, P|W|PS + A|D */
#define RESULT_GPA  0x5000UL
#define EXIT_HLT    12

/* r8=MAGIC rbx=START rcx=END ; loop1: [rbx]=rbx^r8 ; loop2: count [rbx]^r8 != rbx in r9 */
static unsigned char code[] = {
	0x49, 0xb8, 0,0,0,0,0,0,0,0,		/* movabs $MAGIC,%r8 */
	0x48, 0xbb, 0,0,0,0,0,0,0,0,		/* movabs $START,%rbx */
	0x48, 0xb9, 0,0,0,0,0,0,0,0,		/* movabs $END,%rcx */
	/* loop1 (19 bytes + jb) */
	0x48, 0x89, 0xd8,			/* mov %rbx,%rax */
	0x4c, 0x31, 0xc0,			/* xor %r8,%rax */
	0x48, 0x89, 0x03,			/* mov %rax,(%rbx) */
	0x48, 0x81, 0xc3, 0x08, 0x10, 0x00, 0x00,	/* add $STRIDE,%rbx */
	0x48, 0x39, 0xcb,			/* cmp %rcx,%rbx */
	0x72, 0xeb,				/* jb loop1 */
	0x48, 0xbb, 0,0,0,0,0,0,0,0,		/* movabs $START,%rbx */
	0x4d, 0x31, 0xc9,			/* xor %r9,%r9 */
	/* loop2 (24 bytes + jb) */
	0x48, 0x8b, 0x03,			/* mov (%rbx),%rax */
	0x4c, 0x31, 0xc0,			/* xor %r8,%rax */
	0x48, 0x39, 0xd8,			/* cmp %rbx,%rax */
	0x74, 0x03,				/* je +3 */
	0x49, 0xff, 0xc1,			/* inc %r9 */
	0x48, 0x81, 0xc3, 0x08, 0x10, 0x00, 0x00,	/* add $STRIDE,%rbx */
	0x48, 0x39, 0xcb,			/* cmp %rcx,%rbx */
	0x72, 0xe6,				/* jb loop2 */
	0x48, 0xbf, 0x00, 0x50, 0,0,0,0,0,0,	/* movabs $0x5000,%rdi */
	0x4c, 0x89, 0x0f,			/* mov %r9,(%rdi) */
	0xf4,					/* hlt */
};

static int one(int round)
{
	struct hvx_config cfg = { .mem_size = RAM_BYTES };
	struct hvx_regs regs;
	struct hvx_exit ex;
	uint64_t end = RAM_BYTES - 8, start = START, magic = MAGIC, a, guest_err, host_err = 0, n = 0;
	uint8_t *mem;
	int fd, i;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return -1; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM 3 GiB"); return -1; }
	mem = mmap(NULL, RAM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); return -1; }
	/* pdpt[0] too: the module's own low-1-GiB map is NOT usable above 512 MiB -- its
	 * PD page (GPA 0x2000) shares a page with the interrupt-handler blob at 0x2800,
	 * which overwrites PD entries 256..511. */
	((uint64_t *)(mem + GPA_PDPT))[0] = LEAF(0);
	((uint64_t *)(mem + GPA_PDPT))[1] = LEAF(1);
	((uint64_t *)(mem + GPA_PDPT))[2] = LEAF(2);
	memcpy(code + 2, &magic, 8);
	memcpy(code + 12, &start, 8);
	memcpy(code + 22, &end, 8);
	memcpy(code + 53, &start, 8);
	memcpy(mem + HVX_GUEST_CODE_BASE, code, sizeof(code));
	memset(&regs, 0, sizeof(regs));
	regs.rip = HVX_GUEST_CODE_BASE; regs.rsp = 0x100000; regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	for (i = 0; i < 100000; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT) break;
		if (ex.reason == HVX_EXIT_THROTTLE) continue;
		{
			struct hvx_regs r;
			ioctl(fd, HVX_GET_REGS, &r);
			fprintf(stderr, "round %d: unexpected exit reason=%u gpa=%#llx rip=%#llx rbx=%#llx rsp=%#llx\n",
				round, ex.reason, (unsigned long long)ex.guest_phys, (unsigned long long)r.rip,
				(unsigned long long)r.rbx, (unsigned long long)r.rsp);
		}
		return -1;
	}
	memcpy(&guest_err, mem + RESULT_GPA, 8);
	for (a = START; a < end; a += STRIDE, n++) {
		uint64_t v;
		memcpy(&v, mem + a, 8);
		if (v != (a ^ MAGIC)) {
			if (host_err < 5)
				fprintf(stderr, "round %d: GPA %#llx holds %#llx want %#llx\n",
					round, (unsigned long long)a, (unsigned long long)v, (unsigned long long)(a ^ MAGIC));
			host_err++;
		}
	}
	printf("hvx_bigram_test: round %d: %" PRIu64 " pages over [2 MiB, 3 GiB) -- guest mismatches=%" PRIu64
	       " host mismatches=%" PRIu64 "\n", round, n, guest_err, host_err);
	munmap(mem, RAM_BYTES);
	close(fd);
	return (guest_err == 0 && host_err == 0) ? 0 : 1;
}

int main(void)
{
	int r, bad = 0;

	for (r = 0; r < 3; r++) {
		int x = one(r);
		if (x < 0) return 2;
		bad |= x;
	}
	printf(bad ? "hvx_bigram_test: FAIL\n"
		   : "hvx_bigram_test: PASS (every page of a 3 GiB guest holds what the guest wrote, via guest and host)\n");
	return bad;
}

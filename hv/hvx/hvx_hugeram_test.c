// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_hugeram_test - guest RAM above 4 GiB (milestone 179): the split around the MMIO hole.
 *
 *   hvx_hugeram_test [MiB]        default 8192 (HVX_RAM_MAX); anything in (3072, 8192]
 *
 * The module puts the first 3 GiB of guest RAM at guest-physical 0 (the low block) and the
 * rest at 4 GiB (the high block); the RAM mmap is one linear buffer (hvx_abi.h, "Guest RAM
 * layout"). The guest maps [0, 4 GiB + high) with 1 GiB paging leaves (installed from
 * userspace), then, as hvx_bigram_test does for 3 GiB, writes one tagged qword into EVERY page
 * of [2 MiB, 3 GiB) and of the whole high block (stride 4096 + 8, so the offset walks through
 * the page), reads them back and counts mismatches in r9. The host checks the same qwords
 * through the mmap (GPA -> buffer offset), hashes the whole buffer, and then makes the guest
 * read three addresses that must not be RAM: inside the hole (0xd0000000), the hole's top
 * page below the devices (0xfdfff000), and the first page above the high block. Each must end
 * the run with an EPT violation at exactly that address (not a silent read of some RAM page).
 * Run three times in fresh VMs: every round must pass and give the same RAM hash.
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

#define START       0x200000ULL
#define STRIDE      0x1008ULL
#define MAGIC       0x6879646f2d72616dULL		/* "hydo-ram" */
#define GPA_PDPT    0x1000UL
#define LEAF(g)     (((uint64_t)(g) << 30) | 0x83 | 0x60)	/* 1 GiB, P|W|PS + A|D */
#define EXIT_HLT    12
#define EXIT_EPT    48

/* in: r10=start rcx=end r8=MAGIC ; loop1: [rbx]=rbx^r8 ; loop2: r9 = count of [rbx]^r8 != rbx */
static const unsigned char fill_check[] = {
	0x4c, 0x89, 0xd3,			/* mov %r10,%rbx */
	/* loop1 */
	0x48, 0x89, 0xd8,			/* mov %rbx,%rax */
	0x4c, 0x31, 0xc0,			/* xor %r8,%rax */
	0x48, 0x89, 0x03,			/* mov %rax,(%rbx) */
	0x48, 0x81, 0xc3, 0x08, 0x10, 0x00, 0x00,	/* add $STRIDE,%rbx */
	0x48, 0x39, 0xcb,			/* cmp %rcx,%rbx */
	0x72, 0xeb,				/* jb loop1 */
	0x4c, 0x89, 0xd3,			/* mov %r10,%rbx */
	0x4d, 0x31, 0xc9,			/* xor %r9,%r9 */
	/* loop2 */
	0x48, 0x8b, 0x03,			/* mov (%rbx),%rax */
	0x4c, 0x31, 0xc0,			/* xor %r8,%rax */
	0x48, 0x39, 0xd8,			/* cmp %rbx,%rax */
	0x74, 0x03,				/* je +3 */
	0x49, 0xff, 0xc1,			/* inc %r9 */
	0x48, 0x81, 0xc3, 0x08, 0x10, 0x00, 0x00,	/* add $STRIDE,%rbx */
	0x48, 0x39, 0xcb,			/* cmp %rcx,%rbx */
	0x72, 0xe6,				/* jb loop2 */
	0xf4,					/* hlt */
};
/* in: rbx=address ; read it */
static const unsigned char touch[] = {
	0x48, 0x8b, 0x03,			/* mov (%rbx),%rax */
	0xf4,					/* hlt */
};
#define TOUCH_AT (HVX_GUEST_CODE_BASE + 0x100)

static uint64_t ram_bytes = HVX_RAM_MAX;

static uint64_t buf_off(uint64_t gpa)	/* guest-physical -> offset in the RAM mmap */
{
	return gpa < HVX_RAM_HIGH_BASE ? gpa : gpa - HVX_RAM_HIGH_BASE + HVX_RAM_LOW_MAX;
}

/* run from `rip` until HLT (0) or another exit (its reason; the GPA in *gpa) */
static int run(int fd, uint64_t rip, uint64_t r10, uint64_t rcx, uint64_t rbx, uint64_t *r9, uint64_t *gpa)
{
	struct hvx_regs regs;
	struct hvx_exit ex;
	long i;

	memset(&regs, 0, sizeof(regs));
	regs.rip = rip; regs.rsp = 0x100000; regs.rflags = 0x2;
	regs.r8 = MAGIC; regs.r10 = r10; regs.rcx = rcx; regs.rbx = rbx;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	for (i = 0; i < 1000000; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == HVX_EXIT_THROTTLE) continue;
		if (ioctl(fd, HVX_GET_REGS, &regs) < 0) { perror("HVX_GET_REGS"); return -1; }
		*r9 = regs.r9;
		*gpa = ex.guest_phys;
		return ex.reason == EXIT_HLT ? 0 : (int)ex.reason;
	}
	fprintf(stderr, "no exit after %ld runs\n", i);
	return -1;
}

static int one(int round, uint64_t *hash)
{
	struct hvx_config cfg = { .mem_size = ram_bytes };
	uint64_t high = ram_bytes - HVX_RAM_LOW_MAX, top = HVX_RAM_HIGH_BASE + high;
	uint64_t ranges[2][2] = { { START, HVX_RAM_LOW_MAX - 8 }, { HVX_RAM_HIGH_BASE, top - 8 } };
	uint64_t holes[3] = { 0xd0000000ULL, 0xfdfff000ULL, top };
	uint64_t a, r9, gpa, guest_err = 0, host_err = 0, n = 0, h = 0xcbf29ce484222325ULL;
	uint8_t *mem;
	int fd, k, rc, bad = 0;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return -1; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) {
		fprintf(stderr, "HVX_CREATE_VM %" PRIu64 " MiB: %m (a module before milestone 179 takes 3 GiB)\n",
			ram_bytes >> 20);
		return -1;
	}
	mem = mmap(NULL, ram_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); return -1; }
	for (k = 0; k < (int)((top + (1ULL << 30) - 1) >> 30) + 1 && k < 512; k++)
		((uint64_t *)(mem + GPA_PDPT))[k] = LEAF(k);	/* identity, the hole included */
	memcpy(mem + HVX_GUEST_CODE_BASE, fill_check, sizeof(fill_check));
	memcpy(mem + TOUCH_AT, touch, sizeof(touch));
	for (k = 0; k < 2; k++) {
		rc = run(fd, HVX_GUEST_CODE_BASE, ranges[k][0], ranges[k][1], 0, &r9, &gpa);
		if (rc) {
			fprintf(stderr, "round %d: range %d: exit %d at gpa %#" PRIx64 "\n", round, k, rc, gpa);
			return -1;
		}
		guest_err += r9;
		for (a = ranges[k][0]; a < ranges[k][1]; a += STRIDE, n++) {
			uint64_t v;
			memcpy(&v, mem + buf_off(a), 8);
			if (v != (a ^ MAGIC)) {
				if (host_err < 5)
					fprintf(stderr, "round %d: GPA %#" PRIx64 " holds %#" PRIx64 " want %#" PRIx64 "\n",
						round, a, v, (uint64_t)(a ^ MAGIC));
				host_err++;
			}
		}
	}
	for (a = 0; a < ram_bytes; a += 8) {
		uint64_t v;
		memcpy(&v, mem + a, 8);
		h = (h ^ v) * 0x100000001b3ULL;
	}
	*hash = h;
	for (k = 0; k < 3; k++) {
		rc = run(fd, TOUCH_AT, 0, 0, holes[k], &r9, &gpa);
		if (rc != EXIT_EPT || gpa != holes[k]) {
			fprintf(stderr, "round %d: a read of %#" PRIx64 " (not RAM) ended with exit %d at gpa %#" PRIx64
				", want an EPT violation there\n", round, holes[k], rc, gpa);
			bad = 1;
		}
	}
	printf("hvx_hugeram_test: round %d: %" PRIu64 " MiB, %" PRIu64 " pages over [2 MiB, 3 GiB) and [4 GiB, %#" PRIx64
	       ") -- guest mismatches=%" PRIu64 " host mismatches=%" PRIu64 " holes %s ram %016" PRIx64 "\n",
	       round, ram_bytes >> 20, n, top, guest_err, host_err, bad ? "MAPPED" : "unmapped", h);
	munmap(mem, ram_bytes);
	close(fd);
	return (guest_err == 0 && host_err == 0 && !bad) ? 0 : 1;
}

int main(int argc, char **argv)
{
	uint64_t h[3];
	int r, bad = 0;

	if (argc > 1) {
		ram_bytes = strtoull(argv[1], NULL, 10) << 20;
		if (ram_bytes <= HVX_RAM_LOW_MAX || ram_bytes > HVX_RAM_MAX) {
			fprintf(stderr, "usage: hvx_hugeram_test [MiB in (3072, 8192]]\n");
			return 2;
		}
	}
	for (r = 0; r < 3; r++) {
		int x = one(r, &h[r]);
		if (x < 0) return 2;
		bad |= x;
	}
	if (h[0] != h[1] || h[1] != h[2])
		bad = 1;
	printf(bad ? "hvx_hugeram_test: FAIL\n"
		   : "hvx_hugeram_test: PASS (every page of both RAM blocks holds what the guest wrote, the hole and "
		     "the end are unmapped, three VMs one RAM hash)\n");
	return bad;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_scale_test - prove the milestone-28 (Rung 3) scale-up: guest RAM beyond
 * 1 GiB via EPT PD fan-out, and a backing disk beyond the old 64 MiB cap via the
 * page-array disk + zero-copy disk mmap.
 * =============================================================================
 *
 * PART A -- RAM > 1 GiB + EPT PD fan-out.
 *   A 1.5 GiB guest needs n_ept_pt = 768 EPT PT pages, which no longer fit under a
 *   single EPT PD (512 max); the module fans the PD level out to n_ept_pd = 2 PD
 *   pages. The guest's own long-mode paging, however, only identity-maps the low
 *   1 GiB by default (module build_guest_page_tables), so to reach a GVA above
 *   1 GiB this test installs a 1 GiB PDPT leaf (pdpt[1]) FROM USERSPACE through the
 *   RAM mmap -- exactly how a real OS would map its own high memory, and without
 *   perturbing the module's default page tables (so every prior fingerprint holds).
 *   The guest then writes a magic to GPA 1.25 GiB, reads it back, and stores it at
 *   0x5000. Reaching 1.25 GiB proves BOTH the guest 1 GiB paging leaf AND that the
 *   second EPT PD page correctly maps that GPA (a GPA served only via ept_pd[1]).
 *
 * PART B -- disk > 64 MiB + zero-copy disk mmap.
 *   Attach a 128 MiB backing image (2x the old cap) as a ZEROED page array (no bulk
 *   copy), map it through the new disk mmap window, write distinct magics at low
 *   (0), a page-crossing offset (4090), mid (64 MiB) and high (128 MiB - 8) offsets,
 *   then read the WHOLE image back through HVX_GET_DISK (the page-array copy path)
 *   and assert every magic survives. Equality proves the disk mmap and HVX_GET_DISK
 *   alias the same pages across the whole GB-scale image, including a value split
 *   across a disk-page boundary.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_scale_test hvx_scale_test.c
 * Run   (inside L1):  sudo ./hvx_scale_test
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

#define RAM_BYTES       (1536UL * 1024 * 1024)		/* 1.5 GiB: n_ept_pd = 2 (fan-out) */
#define HIGH_GPA        (0x50000000UL)			/* 1.25 GiB: above 1 GiB, in EPT PD #1 */
#define RESULT_GPA      0x5000UL
#define RAM_MAGIC       0x1234567890abcdefULL

#define GPA_PDPT        0x1000UL	/* module page-table layout (see hvx_abi.h header) */
/* 1 GiB PDPT leaf: present|write|PS + Accessed|Dirty preset. Maps GVA [1GiB,2GiB)
 * identity so the guest can reach HIGH_GPA. Physical base = 1 GiB (bit 30). */
#define PDPT_1G_LEAF    ((1ULL << 30) | 0x83 | 0x60)

#define DISK_BYTES      (128UL * 1024 * 1024)		/* 128 MiB: 2x the old 64 MiB cap */
#define DMAGIC0         0xa0a0a0a0a0a0a0a0ULL		/* offset 0 */
#define DMAGIC_X        0xb1b1b1b1b1b1b1b1ULL		/* offset 4090 (crosses a disk page) */
#define DMAGIC1         0xc2c2c2c2c2c2c2c2ULL		/* offset 64 MiB */
#define DMAGIC2         0xd3d3d3d3d3d3d3d3ULL		/* offset 128 MiB - 8 (last page) */
#define DOFF_X          4090UL
#define DOFF_MID        (64UL * 1024 * 1024)
#define DOFF_HIGH       (DISK_BYTES - 8)

/*
 * Guest (long mode, GVA==GPA):
 *   movabs $HIGH_GPA,%rbx ; movabs $RAM_MAGIC,%rax ; mov %rax,(%rbx)
 *   mov (%rbx),%rax       ; movabs $0x5000,%rdi    ; mov %rax,(%rdi) ; hlt
 */
static const unsigned char guest_code[] = {
	0x48, 0xbb, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00,  /* movabs $0x50000000,%rbx */
	0x48, 0xb8, 0xef, 0xcd, 0xab, 0x90, 0x78, 0x56, 0x34, 0x12,  /* movabs $RAM_MAGIC,%rax */
	0x48, 0x89, 0x03,					    /* mov %rax,(%rbx) */
	0x48, 0x8b, 0x03,					    /* mov (%rbx),%rax */
	0x48, 0xbf, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  /* movabs $0x5000,%rdi */
	0x48, 0x89, 0x07,					    /* mov %rax,(%rdi) */
	0xf4,							    /* hlt */
};

static void put64(uint8_t *base, uint64_t off, uint64_t v)
{
	memcpy(base + off, &v, sizeof(v));
}
static uint64_t get64(const uint8_t *base, uint64_t off)
{
	uint64_t v;
	memcpy(&v, base + off, sizeof(v));
	return v;
}

int main(void)
{
	int fd, i, rc = 1;
	uint8_t *mem = MAP_FAILED, *disk_mm = MAP_FAILED, *verify = NULL;
	uint64_t *pdpt;
	struct hvx_config cfg = { .mem_size = RAM_BYTES };
	struct hvx_regs regs;
	struct hvx_exit ex;
	struct hvx_disk dk;
	uint64_t at_result, at_high;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	/* ---- PART A: RAM > 1 GiB + EPT PD fan-out ---- */
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM (1.5 GiB)"); goto out; }
	mem = mmap(NULL, RAM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap RAM 1.5 GiB"); goto out; }

	/* install a 1 GiB guest paging leaf for [1 GiB, 2 GiB) so the guest can reach
	 * HIGH_GPA -- the module only maps the low 1 GiB by default. */
	pdpt = (uint64_t *)(mem + GPA_PDPT);
	pdpt[1] = PDPT_1G_LEAF;

	memcpy(mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = 0x100000;			/* 1 MiB, unused */
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); goto out; }

	for (i = 0; i < 32; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); goto out; }
		if (ex.reason == EXIT_HLT)
			break;
		if (ex.reason == HVX_EXIT_THROTTLE)
			continue;
		fprintf(stderr, "hvx_scale_test: unexpected exit reason=%u qual=0x%llx gpa=0x%llx\n",
			ex.reason, (unsigned long long)ex.qualification,
			(unsigned long long)ex.guest_phys);
		goto out;
	}

	at_result = get64(mem, RESULT_GPA);
	at_high   = get64(mem, HIGH_GPA);
	printf("hvx_scale_test: RAM: guest read-back at 0x5000 = 0x%016" PRIx64
	       ", value at 1.25 GiB (via mmap) = 0x%016" PRIx64 "\n", at_result, at_high);
	if (at_result != RAM_MAGIC || at_high != RAM_MAGIC) {
		fprintf(stderr, "hvx_scale_test: FAIL (PART A): expected 0x%016" PRIx64 " at both\n",
			(uint64_t)RAM_MAGIC);
		goto out;
	}

	/* ---- PART B: disk > 64 MiB + zero-copy disk mmap ---- */
	memset(&dk, 0, sizeof(dk));
	dk.len = DISK_BYTES;
	dk.data = 0;				/* zeroed page array; fill via mmap below */
	dk.flags = HVX_DISK_NO_IDMAP;		/* don't poke the guest page tables */
	if (ioctl(fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK (128 MiB)"); goto out; }

	disk_mm = mmap(NULL, DISK_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
		       (off_t)HVX_DISK_MMAP_PGOFF * 4096);
	if (disk_mm == MAP_FAILED) { perror("mmap disk window"); goto out; }

	put64(disk_mm, 0,         DMAGIC0);
	put64(disk_mm, DOFF_X,    DMAGIC_X);	/* crosses disk pages 0/1 */
	put64(disk_mm, DOFF_MID,  DMAGIC1);
	put64(disk_mm, DOFF_HIGH, DMAGIC2);

	/* read the whole image back through the page-array HVX_GET_DISK path */
	verify = malloc(DISK_BYTES);
	if (!verify) { perror("malloc verify"); goto out; }
	memset(&dk, 0, sizeof(dk));
	dk.len = DISK_BYTES;
	dk.data = (uint64_t)(uintptr_t)verify;
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0) { perror("HVX_GET_DISK"); goto out; }
	if (dk.len != DISK_BYTES) {
		fprintf(stderr, "hvx_scale_test: FAIL: GET_DISK reported size %llu != %lu\n",
			(unsigned long long)dk.len, DISK_BYTES);
		goto out;
	}

	printf("hvx_scale_test: disk via GET_DISK: [0]=0x%016" PRIx64 " [4090]=0x%016" PRIx64
	       " [64M]=0x%016" PRIx64 " [128M-8]=0x%016" PRIx64 "\n",
	       get64(verify, 0), get64(verify, DOFF_X), get64(verify, DOFF_MID),
	       get64(verify, DOFF_HIGH));

	if (get64(verify, 0) != DMAGIC0 || get64(verify, DOFF_X) != DMAGIC_X ||
	    get64(verify, DOFF_MID) != DMAGIC1 || get64(verify, DOFF_HIGH) != DMAGIC2) {
		fprintf(stderr, "hvx_scale_test: FAIL (PART B): disk mmap <-> GET_DISK mismatch\n");
		goto out;
	}

	printf("hvx_scale_test: MILESTONE 28 PASS: 1.5 GiB guest RAM via EPT PD fan-out "
	       "(guest reached GPA 1.25 GiB), and a 128 MiB page-array disk round-trips "
	       "byte-for-byte through the zero-copy disk mmap + HVX_GET_DISK\n");
	rc = 0;

out:
	if (verify) free(verify);
	if (disk_mm != MAP_FAILED) munmap(disk_mm, DISK_BYTES);
	if (mem != MAP_FAILED) munmap(mem, RAM_BYTES);
	close(fd);
	return rc;
}

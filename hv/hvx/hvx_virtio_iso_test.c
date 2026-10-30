// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_virtio_iso_test - Phase A of milestone 20: the deterministic virtio-blk
 * CORE + completion, validated IN ISOLATION (no PCI transport).
 * =============================================================================
 *
 * A hand-written guest drives the virtio-blk device through the simplest possible
 * kick -- a bare PIO doorbell (the Phase-A test transport) -- so the NOVEL,
 * risky part (deterministic I/O completion + snapshot) is proven on its own
 * before the PCI plumbing is layered on top. If a fingerprint diverges here, it
 * is the completion logic, not PCI enumeration.
 *
 * Setup: attach a 8-sector backing image (HVX_SET_DISK) with sector 2 preloaded
 * with a known pattern. The guest RAM holds one split virtqueue with TWO chains:
 *   chain 0: WRITE  512 bytes -> disk sector 5   (T_OUT)
 *   chain 1: READ   disk sector 2 -> a buffer    (T_IN)
 * The guest programs the queue via the PIO registers, kicks NOTIFY (both requests
 * drain in one kick), then spins until the completion IRQ has run (the module's
 * built-in ISR bumps a counter) and HLTs.
 *
 * Proves:
 *   - data round-trips through the backing image (write landed on disk sector 5;
 *     read pulled disk sector 2 into the guest buffer),
 *   - the completion IRQ fires exactly once, at a REPRODUCIBLE instruction count,
 *   - two fresh runs are byte-identical (replay),
 *   - snapshot/restore is idempotent (round-trip),
 *   - branch-from-snapshot mid-flight (after the kick, before the completion
 *     fires) reproduces the finish bit-for-bit -- over an FNV fingerprint of
 *     regs + device state + guest RAM + the disk image.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_virtio_iso_test hvx_virtio_iso_test.c
 * Run   (inside L1):  sudo ./hvx_virtio_iso_test
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
#define INTR_CTR_GPA    0x2f00UL		/* built-in ISR increments this (GPA_INTR_COUNT) */
#define VECTOR          0x20			/* completion vector (built-in IDT gate) */

/* guest-RAM layout for the virtqueue + buffers */
#define DESC_GPA        0x8000UL
#define AVAIL_GPA       0x8800UL
#define USED_GPA        0x9000UL
#define HDR0_GPA        0xA000UL		/* chain 0 (write) request header */
#define WBUF_GPA        0xA200UL		/* 512 bytes to write */
#define ST0_GPA         0xA400UL		/* chain 0 status byte */
#define HDR1_GPA        0xA800UL		/* chain 1 (read) request header */
#define RBUF_GPA        0xAA00UL		/* 512 bytes read target */
#define ST1_GPA         0xAC00UL		/* chain 1 status byte */
#define QSIZE           8

#define DISK_SECTORS    8
#define DISK_BYTES      (DISK_SECTORS * HVX_VBLK_SECTOR)
#define WRITE_SECTOR    5
#define READ_SECTOR     2

#define MIDPOINT_INSNS  25			/* after the NOTIFY kick, before the IRQ fires */

/* ---- little emitters so the guest program is machine-checked, not hand-hexed ---- */
static size_t emit_out32(uint8_t *p, uint32_t val, uint32_t port)
{
	size_t n = 0;
	p[n++] = 0xb8; memcpy(p + n, &val, 4);  n += 4;	/* mov eax, imm32 */
	p[n++] = 0xba; memcpy(p + n, &port, 4); n += 4;	/* mov edx, imm32 */
	p[n++] = 0xef;					/* out dx, eax */
	return n;
}

/* Build: program the 7 device registers, NOTIFY, then spin on the ISR counter. */
static size_t build_guest(uint8_t *code)
{
	size_t n = 0;

	n += emit_out32(code + n, DESC_GPA,  HVX_VIRTIO_PIO_BASE + HVX_VPIO_DESC_LO);
	n += emit_out32(code + n, AVAIL_GPA, HVX_VIRTIO_PIO_BASE + HVX_VPIO_AVAIL_LO);
	n += emit_out32(code + n, USED_GPA,  HVX_VIRTIO_PIO_BASE + HVX_VPIO_USED_LO);
	n += emit_out32(code + n, QSIZE,     HVX_VIRTIO_PIO_BASE + HVX_VPIO_QSIZE);
	n += emit_out32(code + n, VECTOR,    HVX_VIRTIO_PIO_BASE + HVX_VPIO_IRQ_VEC);
	n += emit_out32(code + n, 1,         HVX_VIRTIO_PIO_BASE + HVX_VPIO_QREADY);
	n += emit_out32(code + n, 0,         HVX_VIRTIO_PIO_BASE + HVX_VPIO_NOTIFY);
	/* movabs rbx, INTR_CTR_GPA */
	code[n++] = 0x48; code[n++] = 0xbb;
	{ uint64_t a = INTR_CTR_GPA; memcpy(code + n, &a, 8); n += 8; }
	/* wait: mov rax,[rbx] ; test rax,rax ; jz wait ; hlt */
	code[n++] = 0x48; code[n++] = 0x8b; code[n++] = 0x03;	/* mov rax,[rbx] */
	code[n++] = 0x48; code[n++] = 0x85; code[n++] = 0xc0;	/* test rax,rax */
	code[n++] = 0x74; code[n++] = 0xf8;			/* jz wait (-8) */
	code[n++] = 0xf4;					/* hlt */
	return n;
}

/* ---- one split-queue descriptor ---- */
struct vqd { uint64_t addr; uint32_t len; uint16_t flags; uint16_t next; };

static void put_desc(void *mem, unsigned i, uint64_t addr, uint32_t len,
		     uint16_t flags, uint16_t next)
{
	struct vqd d = { addr, len, flags, next };
	memcpy((uint8_t *)mem + DESC_GPA + i * 16, &d, sizeof(d));
}

static void put_hdr(void *mem, uint64_t gpa, uint32_t type, uint64_t sector)
{
	uint8_t h[16] = { 0 };
	memcpy(&h[0], &type, 4);
	memcpy(&h[8], &sector, 8);
	memcpy((uint8_t *)mem + gpa, h, 16);
}

/* Lay out the two chains + avail ring in guest RAM, and the write buffer. */
static void build_queue(void *mem)
{
	unsigned i;
	uint8_t wpat[HVX_VBLK_SECTOR], zero = 0;
	uint16_t avail[2 + 2];			/* flags, idx, ring[0], ring[1] */

	/* chain 0: WRITE sector 5  (header -> data -> status) */
	put_hdr(mem, HDR0_GPA, VIRTIO_BLK_T_OUT, WRITE_SECTOR);
	for (i = 0; i < HVX_VBLK_SECTOR; i++)
		wpat[i] = (uint8_t)(0xA0 + (i & 0x1f));
	memcpy((uint8_t *)mem + WBUF_GPA, wpat, HVX_VBLK_SECTOR);
	memcpy((uint8_t *)mem + ST0_GPA, &zero, 1);
	put_desc(mem, 0, HDR0_GPA, 16,             VIRTQ_DESC_F_NEXT, 1);
	put_desc(mem, 1, WBUF_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT, 2);
	put_desc(mem, 2, ST0_GPA,  1,              VIRTQ_DESC_F_WRITE, 0);

	/* chain 1: READ sector 2  (header -> writable data -> status) */
	put_hdr(mem, HDR1_GPA, VIRTIO_BLK_T_IN, READ_SECTOR);
	memset((uint8_t *)mem + RBUF_GPA, 0, HVX_VBLK_SECTOR);
	memcpy((uint8_t *)mem + ST1_GPA, &zero, 1);
	put_desc(mem, 3, HDR1_GPA, 16,             VIRTQ_DESC_F_NEXT, 4);
	put_desc(mem, 4, RBUF_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 5);
	put_desc(mem, 5, ST1_GPA,  1,              VIRTQ_DESC_F_WRITE, 0);

	/* avail ring: two entries, heads 0 and 3 */
	avail[0] = 0;		/* flags */
	avail[1] = 2;		/* idx   */
	avail[2] = 0;		/* ring[0] -> chain 0 head */
	avail[3] = 3;		/* ring[1] -> chain 1 head */
	memcpy((uint8_t *)mem + AVAIL_GPA, avail, sizeof(avail));

	/* used ring cleared */
	memset((uint8_t *)mem + USED_GPA, 0, 8 + 8 * QSIZE);
}

/* ---- canonical fingerprint: regs + device state + guest RAM + disk ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}

static uint64_t fingerprint(int fd, void *mem, uint8_t *disk)
{
	struct hvx_snapshot s;
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk };
	uint64_t ram_len = GUEST_MEM_BYTES, disk_len = DISK_BYTES;
	uint64_t h = 0xcbf29ce484222325ULL;

	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0)     { perror("HVX_GET_DISK"); return 0; }

	h = fnv(h, "hvx-vblk-v1", 11);
	h = fnv(h, &s.regs, sizeof(s.regs));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.vblk_last_avail, sizeof(s.vblk_last_avail));
	h = fnv(h, &s.vblk_isr, sizeof(s.vblk_isr));
	h = fnv(h, &s.vblk_irq_pending, sizeof(s.vblk_irq_pending));
	h = fnv(h, &s.vblk_completion_armed, sizeof(s.vblk_completion_armed));
	h = fnv(h, &s.vblk_completion_due, sizeof(s.vblk_completion_due));
	h = fnv(h, &s.inject_valid, sizeof(s.inject_valid));
	h = fnv(h, &s.inject_at_icount, sizeof(s.inject_at_icount));
	h = fnv(h, &ram_len, sizeof(ram_len));
	h = fnv(h, mem, GUEST_MEM_BYTES);
	h = fnv(h, &disk_len, sizeof(disk_len));
	h = fnv(h, disk, DISK_BYTES);
	return h;
}

static int set_disk(int fd, uint8_t *disk)
{
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk };
	return ioctl(fd, HVX_SET_DISK, &dk) < 0 ? -1 : 0;
}

static int setup(int fd, void *mem, uint8_t *disk)
{
	struct hvx_regs regs;

	if (set_disk(fd, disk) < 0) { perror("HVX_SET_DISK"); return -1; }
	build_queue(mem);
	*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x202;			/* IF=1: required to deliver the completion IRQ */
	return ioctl(fd, HVX_SET_REGS, &regs) < 0 ? -1 : 0;
}

static int run_to_halt(int fd, int reset, struct hvx_run_ctl *out)
{
	struct hvx_run_ctl ctl;
	int throttles = 0;

	for (;;) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (reset ? HVX_RUN_RESET_ICOUNT : 0);
		reset = 0;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE) {
			if (++throttles > 64) {			/* guest not halting: bail, don't hang */
				fprintf(stderr, "hvx_virtio_iso_test: FAIL: guest never HLTed "
					"(icount=%" PRIu64 ") -- completion IRQ likely not delivered\n",
					(uint64_t)ctl.icount);
				*out = ctl;
				return -1;
			}
			continue;
		}
		*out = ctl;
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}

static uint64_t rd64(void *mem, unsigned long gpa)
{
	return *(volatile uint64_t *)((uint8_t *)mem + gpa);
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	uint8_t *disk, *disk_out, *ramsave, *disksave;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl r;
	struct hvx_snapshot mid;
	uint64_t fp_a, fp_b, fp_round, fp_orig, fp_branch;
	unsigned i;
	int ok_data, ok_replay, ok_round, ok_branch;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	disk     = malloc(DISK_BYTES);
	disk_out = malloc(DISK_BYTES);
	ramsave  = malloc(GUEST_MEM_BYTES);
	disksave = malloc(DISK_BYTES);
	if (!disk || !disk_out || !ramsave || !disksave) { perror("malloc"); goto out; }

	/* backing image: sector 2 gets a known pattern (the READ source) */
	memset(disk, 0, DISK_BYTES);
	for (i = 0; i < HVX_VBLK_SECTOR; i++)
		disk[READ_SECTOR * HVX_VBLK_SECTOR + i] = (uint8_t)(0x30 + (i & 0x1f));

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }

	{ uint8_t code[256]; size_t clen = build_guest(code);
	  memcpy((uint8_t *)mem + HVX_GUEST_CODE_BASE, code, clen); }

	/* run #1 */
	if (setup(fd, mem, disk) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) {
		fprintf(stderr, "hvx_virtio_iso_test: FAIL: run #1 did not HLT (reason=%u)\n", r.reason);
		goto out;
	}

	/* data round-trip: WRITE landed on disk sector 5; READ pulled sector 2 in */
	{
		struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk_out };
		int wr_ok = 1, rd_ok = 1;

		if (ioctl(fd, HVX_GET_DISK, &dk) < 0) { perror("HVX_GET_DISK"); goto out; }
		for (i = 0; i < HVX_VBLK_SECTOR; i++) {
			if (disk_out[WRITE_SECTOR * HVX_VBLK_SECTOR + i] != (uint8_t)(0xA0 + (i & 0x1f)))
				wr_ok = 0;
			if (((uint8_t *)mem)[RBUF_GPA + i] != (uint8_t)(0x30 + (i & 0x1f)))
				rd_ok = 0;
		}
		ok_data = wr_ok && rd_ok &&
			  ((uint8_t *)mem)[ST0_GPA] == VIRTIO_BLK_S_OK &&
			  ((uint8_t *)mem)[ST1_GPA] == VIRTIO_BLK_S_OK &&
			  rd64(mem, INTR_CTR_GPA) == 1;
		printf("hvx_virtio_iso_test: data write=%s read=%s status=%u/%u intr=%" PRIu64
		       " icount=%" PRIu64 "\n", wr_ok ? "OK" : "BAD", rd_ok ? "OK" : "BAD",
		       ((uint8_t *)mem)[ST0_GPA], ((uint8_t *)mem)[ST1_GPA],
		       rd64(mem, INTR_CTR_GPA), (uint64_t)r.icount);
		if (!ok_data) {
			fprintf(stderr, "hvx_virtio_iso_test: FAIL: data round-trip / completion\n");
			goto out;
		}
	}
	fp_a = fingerprint(fd, mem, disk_out);

	/* replay: a fresh run hashes identically */
	if (setup(fd, mem, disk) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) goto out;
	fp_b = fingerprint(fd, mem, disk_out);
	ok_replay = (fp_a == fp_b);
	printf("hvx_virtio_iso_test: replay     fp_a=%016" PRIx64 " fp_b=%016" PRIx64 " -> %s\n",
	       fp_a, fp_b, ok_replay ? "OK" : "MISMATCH");

	/* round-trip: snapshot + restore (RAM + disk) -> same fingerprint */
	{
		struct hvx_snapshot snap;
		struct hvx_disk dget = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
		struct hvx_disk dput;

		if (ioctl(fd, HVX_GET_SNAPSHOT, &snap) < 0) { perror("GET_SNAPSHOT"); goto out; }
		memcpy(ramsave, mem, GUEST_MEM_BYTES);
		if (ioctl(fd, HVX_GET_DISK, &dget) < 0) { perror("GET_DISK"); goto out; }
		if (ioctl(fd, HVX_SET_SNAPSHOT, &snap) < 0) { perror("SET_SNAPSHOT"); goto out; }
		memcpy(mem, ramsave, GUEST_MEM_BYTES);
		dput.len = DISK_BYTES; dput.data = (uint64_t)(uintptr_t)disksave;
		if (ioctl(fd, HVX_SET_DISK, &dput) < 0) { perror("SET_DISK"); goto out; }
		fp_round = fingerprint(fd, mem, disk_out);
	}
	ok_round = (fp_round == fp_b);
	printf("hvx_virtio_iso_test: round-trip fp=%016" PRIx64 " (vs %016" PRIx64 ") -> %s\n",
	       fp_round, fp_b, ok_round ? "OK" : "MISMATCH");

	/* branch: step past the kick (completion scheduled, not yet fired), snapshot
	 * RAM+disk, finish -> fp_orig; restore, finish -> fp_branch; assert equal. */
	if (setup(fd, mem, disk) < 0) goto out;
	{
		struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
					    .max_insns = MIDPOINT_INSNS };
		if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); goto out; }
		if (step.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "hvx_virtio_iso_test: FAIL: midpoint stop reason=%u\n", step.reason);
			goto out;
		}
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	{
		struct hvx_disk dget = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
		if (ioctl(fd, HVX_GET_DISK, &dget) < 0) { perror("GET_DISK"); goto out; }
	}
	if (!mid.vblk_completion_armed) {
		fprintf(stderr, "hvx_virtio_iso_test: FAIL: snapshot did not capture a pending completion\n");
		goto out;
	}
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem, disk_out);

	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("SET_SNAPSHOT"); goto out; }
	memcpy(mem, ramsave, GUEST_MEM_BYTES);
	{
		struct hvx_disk dput = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
		if (ioctl(fd, HVX_SET_DISK, &dput) < 0) { perror("SET_DISK"); goto out; }
	}
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem, disk_out);
	ok_branch = (fp_orig == fp_branch && fp_branch == fp_a);
	printf("hvx_virtio_iso_test: branch     fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a,
	       ok_branch ? "OK" : "MISMATCH");

	if (ok_data && ok_replay && ok_round && ok_branch) {
		printf("hvx_virtio_iso_test: MILESTONE 20 PHASE A PASS: deterministic virtio-blk "
		       "core + instruction-timed completion; data round-trips through the disk; "
		       "replay / round-trip / branch all fingerprint-equal\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_virtio_iso_test: FAIL: a determinism check did not hold\n");
	}

out:
	free(disk); free(disk_out); free(ramsave); free(disksave);
	close(fd);
	return rc;
}

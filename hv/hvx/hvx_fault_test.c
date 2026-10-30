// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_fault_test - milestone 22: DETERMINISTIC disk fault injection.
 * =============================================================================
 *
 * Fault injection as in FoundationDB's simulation: a SEEDED policy perturbs the virtio-blk
 * device (delay / error / drop / completion-reorder / torn writes) so that any bug
 * it surfaces replays bit-for-bit. Every fault decision is a PURE FUNCTION of the
 * seed + the per-device request index -- never wall-clock time or host state -- so
 * the same seed yields the same fault schedule yields the same result. The policy
 * and PRNG counters ride in the snapshot, so branch/replay reproduces the exact
 * faults.
 *
 * A hand-written guest (the Phase-A PIO doorbell transport) submits SIX requests in
 * one kick -- three writes (sectors 10/11/12) interleaved with three reads (sectors
 * 2/3/4). This test replicates the module's PRNG (splitmix64 finalizer, keyed by
 * request index) EXACTLY, so it predicts the whole schedule and checks the observed
 * device behavior against it.
 *
 * Proves:
 *   (a) reproducible under a seed -- the observed per-request outcomes (which
 *       errored / dropped / were delayed to which completion icount / the used-ring
 *       completion order) match the predicted deterministic schedule, AND two runs
 *       with the same seed produce byte-identical fingerprints;
 *   (b) the seed controls the schedule -- a different seed yields a DIFFERENT (but
 *       still reproducible) schedule and fingerprint;
 *   (c) snapshot/branch mid-flight reproduces the fault schedule bit-for-bit;
 *   (d) faults DISABLED (default) => the device is byte-for-byte the M20/M21 disk:
 *       every write persists, every read returns disk data, all statuses OK, the
 *       completion is at the un-delayed icount, and the used ring is in submission
 *       order.
 *   + a focused TORN-write check (stretch): a 2-sector write persists only its
 *     first sector.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_fault_test hvx_fault_test.c
 * Run   (inside L1):  sudo ./hvx_fault_test
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

/* guest-RAM layout: one split virtqueue + per-request header/data/status buffers */
#define DESC_GPA        0x8000UL
#define AVAIL_GPA       0x8800UL
#define USED_GPA        0x9000UL
#define REQ_BASE        0xA000UL
#define HDR_GPA(k)      (REQ_BASE + (uint64_t)(k) * 0x1000UL)
#define DATA_GPA(k)     (REQ_BASE + (uint64_t)(k) * 0x1000UL + 0x200UL)
#define ST_GPA(k)       (REQ_BASE + (uint64_t)(k) * 0x1000UL + 0x400UL)
#define QSIZE           32

#define N_REQ           6
#define DISK_SECTORS    32
#define DISK_BYTES      (DISK_SECTORS * HVX_VBLK_SECTOR)
#define MIDPOINT_INSNS  25			/* after the NOTIFY kick, before the completion fires */

/* mirror of the kernel enum vblk_fault -- values are load-bearing (used in selection) */
enum { VBF_NONE = 0, VBF_DELAY, VBF_ERROR, VBF_DROP, VBF_TORN };

struct req { uint32_t type; uint64_t sector; };
static const struct req reqs[N_REQ] = {
	{ VIRTIO_BLK_T_OUT, 10 },
	{ VIRTIO_BLK_T_IN,   2 },
	{ VIRTIO_BLK_T_OUT, 11 },
	{ VIRTIO_BLK_T_IN,   3 },
	{ VIRTIO_BLK_T_OUT, 12 },
	{ VIRTIO_BLK_T_IN,   4 },
};

/* ---- REPLICA of the module's deterministic fault PRNG (must match hvx.c) ---- */
static uint64_t fault_mix(uint64_t x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}
static uint64_t fault_roll(uint64_t seed, uint64_t k)
{
	return fault_mix(seed + (k + 1) * 0x9e3779b97f4a7c15ULL);
}

/* Predict one request's fault class (and DELAY extra) -- mirrors vblk_fault_decide. */
static int predict(uint64_t seed, uint64_t k, uint32_t classes, uint32_t prob,
		   uint32_t max_delay, uint32_t type, uint32_t *extra)
{
	uint64_t h = fault_roll(seed, k);
	uint32_t pct = (uint32_t)(h % 100);
	int cand[4], n = 0, sel;

	*extra = 0;
	if (classes == 0)
		return VBF_NONE;
	if (pct >= prob)
		return VBF_NONE;
	if (classes & HVX_FAULT_DELAY)
		cand[n++] = VBF_DELAY;
	if (classes & HVX_FAULT_ERROR)
		cand[n++] = VBF_ERROR;
	if ((classes & HVX_FAULT_DROP) && type == VIRTIO_BLK_T_OUT)
		cand[n++] = VBF_DROP;
	if ((classes & HVX_FAULT_TORN) && type == VIRTIO_BLK_T_OUT)
		cand[n++] = VBF_TORN;
	if (n == 0)
		return VBF_NONE;
	sel = (int)((h >> 8) % (uint64_t)n);
	if (cand[sel] == VBF_DELAY) {
		uint32_t md = max_delay ? max_delay : 1;

		*extra = 1 + (uint32_t)((h >> 16) % md);
	}
	return cand[sel];
}

/* Predict the per-kick used-ring completion order -- mirrors virtio_blk_process. */
static void predict_reorder(uint64_t seed, uint32_t kick_seq, int n, int *order)
{
	uint64_t pr = fault_mix((seed ^ 0xD15EA5E5C0FFEE11ULL)
				+ ((uint64_t)kick_seq + 1) * 0x9e3779b97f4a7c15ULL);
	int i;

	for (i = 0; i < n; i++)
		order[i] = i;
	for (i = n - 1; i > 0; i--) {
		int j, t;

		pr = fault_mix(pr);
		j = (int)(pr % (uint64_t)(i + 1));
		t = order[i]; order[i] = order[j]; order[j] = t;
	}
}

/* ---- little emitters so the guest program is machine-checked, not hand-hexed ---- */
static size_t emit_out32(uint8_t *p, uint32_t val, uint32_t port)
{
	size_t n = 0;

	p[n++] = 0xb8; memcpy(p + n, &val, 4);  n += 4;	/* mov eax, imm32 */
	p[n++] = 0xba; memcpy(p + n, &port, 4); n += 4;	/* mov edx, imm32 */
	p[n++] = 0xef;					/* out dx, eax */
	return n;
}

/* Program the device registers, NOTIFY, then spin on the ISR counter, then HLT. */
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
	code[n++] = 0x48; code[n++] = 0xbb;			/* movabs rbx, INTR_CTR_GPA */
	{ uint64_t a = INTR_CTR_GPA; memcpy(code + n, &a, 8); n += 8; }
	code[n++] = 0x48; code[n++] = 0x8b; code[n++] = 0x03;	/* mov rax,[rbx] */
	code[n++] = 0x48; code[n++] = 0x85; code[n++] = 0xc0;	/* test rax,rax */
	code[n++] = 0x74; code[n++] = 0xf8;			/* jz wait (-8) */
	code[n++] = 0xf4;					/* hlt */
	return n;
}

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

/* the deterministic write pattern request k stores (writes only) */
static uint8_t wbyte(int k, int i) { return (uint8_t)(0x40 + k * 0x11 + (i & 0x0f)); }
/* the deterministic pattern preloaded on disk read-source sector s */
static uint8_t dbyte(uint64_t s, int i) { return (uint8_t)(0x30 + s + (i & 0x1f)); }

static void init_disk(uint8_t *disk)
{
	unsigned k, i;

	memset(disk, 0, DISK_BYTES);
	for (k = 0; k < N_REQ; k++)
		if (reqs[k].type == VIRTIO_BLK_T_IN)
			for (i = 0; i < HVX_VBLK_SECTOR; i++)
				disk[reqs[k].sector * HVX_VBLK_SECTOR + i] = dbyte(reqs[k].sector, i);
}

/* Lay out the six 3-descriptor chains + the avail ring in guest RAM. */
static void build_queue(void *mem)
{
	uint16_t avail[2 + N_REQ];
	unsigned k, i;
	uint8_t zero = 0;

	for (k = 0; k < N_REQ; k++) {
		int w = (reqs[k].type == VIRTIO_BLK_T_OUT);

		put_hdr(mem, HDR_GPA(k), reqs[k].type, reqs[k].sector);
		if (w)
			for (i = 0; i < HVX_VBLK_SECTOR; i++)
				((uint8_t *)mem)[DATA_GPA(k) + i] = wbyte(k, i);
		else
			memset((uint8_t *)mem + DATA_GPA(k), 0, HVX_VBLK_SECTOR);
		memcpy((uint8_t *)mem + ST_GPA(k), &zero, 1);
		put_desc(mem, 3 * k + 0, HDR_GPA(k),  16,             VIRTQ_DESC_F_NEXT, 3 * k + 1);
		put_desc(mem, 3 * k + 1, DATA_GPA(k), HVX_VBLK_SECTOR,
			 w ? VIRTQ_DESC_F_NEXT : (VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE), 3 * k + 2);
		put_desc(mem, 3 * k + 2, ST_GPA(k),   1,              VIRTQ_DESC_F_WRITE, 0);
		avail[2 + k] = 3 * k;
	}
	avail[0] = 0;			/* flags */
	avail[1] = N_REQ;		/* idx   */
	memcpy((uint8_t *)mem + AVAIL_GPA, avail, sizeof(avail));
	memset((uint8_t *)mem + USED_GPA, 0, 8 + 8 * QSIZE);
}

/* ---- canonical fingerprint: regs + fault/device state + guest RAM + disk ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;

	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}
static uint64_t fingerprint(int fd, void *mem, uint8_t *diskbuf)
{
	struct hvx_snapshot s;
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)diskbuf };
	uint64_t ram_len = GUEST_MEM_BYTES, disk_len = DISK_BYTES;
	uint64_t h = 0xcbf29ce484222325ULL;

	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0)     { perror("HVX_GET_DISK"); return 0; }

	h = fnv(h, "hvx-fault-v1", 12);
	h = fnv(h, &s.regs, sizeof(s.regs));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.vblk_last_avail, sizeof(s.vblk_last_avail));
	h = fnv(h, &s.vblk_completion_armed, sizeof(s.vblk_completion_armed));
	h = fnv(h, &s.vblk_completion_due, sizeof(s.vblk_completion_due));
	/* the fault-injection policy + PRNG state IS part of the deterministic state */
	h = fnv(h, &s.fault_seed, sizeof(s.fault_seed));
	h = fnv(h, &s.fault_classes, sizeof(s.fault_classes));
	h = fnv(h, &s.fault_probability, sizeof(s.fault_probability));
	h = fnv(h, &s.fault_max_delay, sizeof(s.fault_max_delay));
	h = fnv(h, &s.fault_kick_seq, sizeof(s.fault_kick_seq));
	h = fnv(h, &s.fault_req_seq, sizeof(s.fault_req_seq));
	h = fnv(h, &s.fault_count, sizeof(s.fault_count));
	h = fnv(h, &ram_len, sizeof(ram_len));
	h = fnv(h, mem, GUEST_MEM_BYTES);
	h = fnv(h, &disk_len, sizeof(disk_len));
	h = fnv(h, diskbuf, DISK_BYTES);
	return h;
}

static int set_disk(int fd, uint8_t *disk)
{
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk };

	return ioctl(fd, HVX_SET_DISK, &dk) < 0 ? -1 : 0;
}
static int set_fault(int fd, uint64_t seed, uint32_t classes, uint32_t prob, uint32_t max_delay)
{
	struct hvx_fault f = { .seed = seed, .classes = classes,
			       .probability = prob, .max_delay = max_delay };

	return ioctl(fd, HVX_SET_FAULT, &f) < 0 ? -1 : 0;
}

/* Reset disk + guest RAM + regs for a fresh run (does NOT touch the fault policy). */
static int setup(int fd, void *mem, uint8_t *disk)
{
	struct hvx_regs regs;

	init_disk(disk);
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
			if (++throttles > 128) {
				fprintf(stderr, "hvx_fault_test: FAIL: guest never HLTed "
					"(icount=%" PRIu64 ")\n", (uint64_t)ctl.icount);
				*out = ctl;
				return -1;
			}
			continue;
		}
		*out = ctl;
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}

/* Check the observed device behavior against the predicted schedule. Returns 1 on
 * match. Fills *n_faults and *total_delay (predicted) for the caller's reporting. */
static int verify_schedule(void *mem, uint8_t *disk_out, uint64_t seed, uint32_t classes,
			   uint32_t prob, uint32_t max_delay, int *n_faults, uint32_t *total_delay)
{
	int ok = 1, k, order[N_REQ];
	uint32_t td = 0;
	int nf = 0;

	for (k = 0; k < N_REQ; k++) {
		uint32_t extra = 0;
		int fc = predict(seed, k, classes, prob, max_delay, reqs[k].type, &extra);
		uint8_t st = ((uint8_t *)mem)[ST_GPA(k)];
		uint64_t sec = reqs[k].sector;
		int i, data_ok = 1;

		if (fc != VBF_NONE)
			nf++;
		if (fc == VBF_DELAY)
			td += extra;

		if (reqs[k].type == VIRTIO_BLK_T_OUT) {		/* WRITE */
			if (fc == VBF_ERROR) {
				if (st != VIRTIO_BLK_S_IOERR) ok = 0;
				for (i = 0; i < HVX_VBLK_SECTOR; i++)	/* not persisted */
					if (disk_out[sec * HVX_VBLK_SECTOR + i] != 0) data_ok = 0;
			} else if (fc == VBF_DROP) {
				if (st != VIRTIO_BLK_S_OK) ok = 0;
				for (i = 0; i < HVX_VBLK_SECTOR; i++)	/* acked but not persisted */
					if (disk_out[sec * HVX_VBLK_SECTOR + i] != 0) data_ok = 0;
			} else {				/* NORMAL or DELAY: persisted */
				if (st != VIRTIO_BLK_S_OK) ok = 0;
				for (i = 0; i < HVX_VBLK_SECTOR; i++)
					if (disk_out[sec * HVX_VBLK_SECTOR + i] != wbyte(k, i)) data_ok = 0;
			}
		} else {					/* READ */
			uint8_t *rbuf = (uint8_t *)mem + DATA_GPA(k);

			if (fc == VBF_ERROR) {
				if (st != VIRTIO_BLK_S_IOERR) ok = 0;
				for (i = 0; i < HVX_VBLK_SECTOR; i++)	/* buffer untouched (0) */
					if (rbuf[i] != 0) data_ok = 0;
			} else {				/* NORMAL or DELAY: disk data pulled in */
				if (st != VIRTIO_BLK_S_OK) ok = 0;
				for (i = 0; i < HVX_VBLK_SECTOR; i++)
					if (rbuf[i] != dbyte(sec, i)) data_ok = 0;
			}
		}
		if (!data_ok) ok = 0;
		printf("    req%d %-5s sec%-2" PRIu64 " pred=%-6s status=%u data=%s\n",
		       k, reqs[k].type == VIRTIO_BLK_T_OUT ? "WRITE" : "READ", sec,
		       fc == VBF_NONE ? "normal" : fc == VBF_DELAY ? "delay" :
		       fc == VBF_ERROR ? "error" : fc == VBF_DROP ? "drop" : "torn",
		       st, data_ok ? "OK" : "BAD");
	}

	/* completion order: the used-ring `id` sequence == the predicted permutation.
	 * (identity when REORDER is disabled). */
	if (classes & HVX_FAULT_REORDER)
		predict_reorder(seed, 0, N_REQ, order);
	else
		for (k = 0; k < N_REQ; k++) order[k] = k;
	{
		uint16_t uidx;
		int i;

		memcpy(&uidx, (uint8_t *)mem + USED_GPA + 2, 2);
		if (uidx != N_REQ) { ok = 0; printf("    used->idx=%u (expected %d)\n", uidx, N_REQ); }
		printf("    completion order (used-ring id): ");
		for (i = 0; i < N_REQ; i++) {
			uint32_t id;

			memcpy(&id, (uint8_t *)mem + USED_GPA + 4 + 8 * i, 4);
			printf("%u ", id);
			if (id != (uint32_t)(3 * order[i])) ok = 0;
		}
		printf("(predicted: ");
		for (i = 0; i < N_REQ; i++) printf("%d ", 3 * order[i]);
		printf(")\n");
	}
	*n_faults = nf;
	*total_delay = td;
	return ok;
}

/* Run one full scenario to HLT and verify the schedule. Returns fingerprint (0 on fail). */
static uint64_t run_scenario(int fd, void *mem, uint8_t *disk, uint8_t *disk_out,
			     uint64_t seed, uint32_t classes, uint32_t prob, uint32_t max_delay,
			     int *ok_out)
{
	struct hvx_run_ctl r;
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk_out };
	int nf; uint32_t td;

	*ok_out = 0;
	if (setup(fd, mem, disk) < 0) return 0;
	if (set_fault(fd, seed, classes, prob, max_delay) < 0) { perror("HVX_SET_FAULT"); return 0; }
	if (run_to_halt(fd, 1, &r) < 0) return 0;
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0) { perror("HVX_GET_DISK"); return 0; }
	*ok_out = verify_schedule(mem, disk_out, seed, classes, prob, max_delay, &nf, &td);
	printf("    -> %d faults, predicted total delay %u insns, completion icount=%" PRIu64 "\n",
	       nf, td, (uint64_t)r.icount);
	return fingerprint(fd, mem, disk_out);
}

/* Read completion_due at a post-kick midpoint (proves "delayed to which icount"). */
static int midpoint_due(int fd, void *mem, uint8_t *disk, uint64_t seed, uint32_t classes,
			uint32_t prob, uint32_t max_delay, uint64_t *due)
{
	struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
				    .max_insns = MIDPOINT_INSNS };
	struct hvx_snapshot s;

	if (setup(fd, mem, disk) < 0) return -1;
	if (set_fault(fd, seed, classes, prob, max_delay) < 0) return -1;
	if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); return -1; }
	if (step.reason != HVX_EXIT_STEP) {
		fprintf(stderr, "hvx_fault_test: midpoint stop reason=%u\n", step.reason);
		return -1;
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return -1; }
	if (!s.vblk_completion_armed) {
		fprintf(stderr, "hvx_fault_test: midpoint did not capture a pending completion\n");
		return -1;
	}
	*due = s.vblk_completion_due;
	return 0;
}

/* Focused TORN check (stretch): a single 2-sector write persists only sector 0. */
static int torn_check(int fd, void *mem, uint8_t *disk, uint8_t *disk_out)
{
	struct hvx_run_ctl r;
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk_out };
	struct hvx_regs regs;
	const uint64_t SEC = 20;
	unsigned i;
	uint16_t avail[3];
	int ok = 1;

	init_disk(disk);
	if (set_disk(fd, disk) < 0) return -1;
	/* one chain: header -> 1024-byte data (2 sectors) -> status */
	put_hdr(mem, HDR_GPA(0), VIRTIO_BLK_T_OUT, SEC);
	for (i = 0; i < 2 * HVX_VBLK_SECTOR; i++)
		((uint8_t *)mem)[DATA_GPA(0) + i] = (uint8_t)(0x80 + (i & 0x3f));
	((uint8_t *)mem)[ST_GPA(0)] = 0;
	put_desc(mem, 0, HDR_GPA(0),  16,                 VIRTQ_DESC_F_NEXT, 1);
	put_desc(mem, 1, DATA_GPA(0), 2 * HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT, 2);
	put_desc(mem, 2, ST_GPA(0),   1,                  VIRTQ_DESC_F_WRITE, 0);
	avail[0] = 0; avail[1] = 1; avail[2] = 0;
	memcpy((uint8_t *)mem + AVAIL_GPA, avail, sizeof(avail));
	memset((uint8_t *)mem + USED_GPA, 0, 8 + 8 * QSIZE);
	*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip = HVX_GUEST_CODE_BASE; regs.rsp = GUEST_MEM_BYTES - 16; regs.rflags = 0x202;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) return -1;
	if (set_fault(fd, 0x707271, HVX_FAULT_TORN, 100, 0) < 0) return -1;
	if (run_to_halt(fd, 1, &r) < 0) return -1;
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0) return -1;
	for (i = 0; i < HVX_VBLK_SECTOR; i++)			/* first sector persisted */
		if (disk_out[SEC * HVX_VBLK_SECTOR + i] != (uint8_t)(0x80 + (i & 0x3f))) ok = 0;
	for (i = 0; i < HVX_VBLK_SECTOR; i++)			/* second sector torn away (0) */
		if (disk_out[(SEC + 1) * HVX_VBLK_SECTOR + i] != 0) ok = 0;
	printf("hvx_fault_test: TORN write (2 sectors, only 1st persisted) -> %s\n",
	       ok ? "OK" : "BAD");
	return ok ? 0 : -1;
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	uint8_t *disk, *disk_out, *ramsave, *disksave;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl r;
	struct hvx_snapshot mid;
	const uint64_t S1 = 0x1234, S2 = 0x9abcdef;
	const uint32_t CLASSES = HVX_FAULT_DELAY | HVX_FAULT_ERROR | HVX_FAULT_DROP | HVX_FAULT_REORDER;
	const uint32_t PROB = 70, MAXD = 8;
	uint64_t fp_a1, fp_a2, fp_s2a, fp_s2b, fp_dis1, fp_dis2, fp_orig, fp_branch;
	uint64_t due_off, due_on;
	int ok_a1, ok_a2, ok_s2a, ok_s2b, ok_dis1, ok_dis2;
	int ok_repro, ok_seed, ok_branch, ok_disabled, ok_delay, ok_torn;
	int k;
	uint32_t td;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	disk     = malloc(DISK_BYTES);
	disk_out = malloc(DISK_BYTES);
	ramsave  = malloc(GUEST_MEM_BYTES);
	disksave = malloc(DISK_BYTES);
	if (!disk || !disk_out || !ramsave || !disksave) { perror("malloc"); goto out; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	{ uint8_t code[256]; size_t clen = build_guest(code);
	  memcpy((uint8_t *)mem + HVX_GUEST_CODE_BASE, code, clen); }

	/* (a) reproducible under a seed: schedule matches the prediction, two runs equal */
	printf("hvx_fault_test: (a) seed=0x%" PRIx64 " classes=0x%x prob=%u maxd=%u -- run #1\n",
	       S1, CLASSES, PROB, MAXD);
	fp_a1 = run_scenario(fd, mem, disk, disk_out, S1, CLASSES, PROB, MAXD, &ok_a1);
	printf("hvx_fault_test: (a) seed=0x%" PRIx64 " -- run #2 (replay)\n", S1);
	fp_a2 = run_scenario(fd, mem, disk, disk_out, S1, CLASSES, PROB, MAXD, &ok_a2);
	ok_repro = ok_a1 && ok_a2 && (fp_a1 == fp_a2) && fp_a1;
	printf("hvx_fault_test: (a) schedule-match=%d/%d  fp1=%016" PRIx64 " fp2=%016" PRIx64 " -> %s\n",
	       ok_a1, ok_a2, fp_a1, fp_a2, ok_repro ? "REPRODUCIBLE" : "FAIL");

	/* "delayed to which icount": enabling DELAY pushes completion_due out by exactly
	 * the predicted summed delay vs the faults-disabled baseline (same kick icount). */
	if (midpoint_due(fd, mem, disk, 0, 0, 0, 0, &due_off) < 0) goto out;
	if (midpoint_due(fd, mem, disk, S1, CLASSES, PROB, MAXD, &due_on) < 0) goto out;
	/* recompute predicted total delay for S1 */
	td = 0;
	for (k = 0; k < N_REQ; k++) {
		uint32_t ex = 0;
		int fc = predict(S1, k, CLASSES, PROB, MAXD, reqs[k].type, &ex);
		if (fc == VBF_DELAY) td += ex;
	}
	ok_delay = (due_on == due_off + td);
	printf("hvx_fault_test: (a) completion_due off=%" PRIu64 " on=%" PRIu64
	       " (predicted +%u) -> %s\n", due_off, due_on, td, ok_delay ? "OK" : "FAIL");

	/* (b) the seed controls the schedule: S2 differs from S1 but is itself reproducible */
	printf("hvx_fault_test: (b) seed=0x%" PRIx64 " -- run #1\n", S2);
	fp_s2a = run_scenario(fd, mem, disk, disk_out, S2, CLASSES, PROB, MAXD, &ok_s2a);
	printf("hvx_fault_test: (b) seed=0x%" PRIx64 " -- run #2 (replay)\n", S2);
	fp_s2b = run_scenario(fd, mem, disk, disk_out, S2, CLASSES, PROB, MAXD, &ok_s2b);
	{
		int diff = 0;
		for (k = 0; k < N_REQ; k++) {
			uint32_t e1 = 0, e2 = 0;
			int f1 = predict(S1, k, CLASSES, PROB, MAXD, reqs[k].type, &e1);
			int f2 = predict(S2, k, CLASSES, PROB, MAXD, reqs[k].type, &e2);
			if (f1 != f2 || e1 != e2) diff = 1;
		}
		ok_seed = ok_s2a && ok_s2b && (fp_s2a == fp_s2b) && (fp_s2a != fp_a1) && diff;
		printf("hvx_fault_test: (b) S2 reproducible=%d  fp_s2=%016" PRIx64
		       " (!= S1 %016" PRIx64 "), schedule-differs=%d -> %s\n",
		       (fp_s2a == fp_s2b), fp_s2a, fp_a1, diff, ok_seed ? "OK" : "FAIL");
	}

	/* (c) snapshot/branch mid-flight (after the kick, before the completion fires)
	 * reproduces the finish bit-for-bit -- so the pending faults + PRNG state are
	 * captured. Uses seed S1 (fp_a1 is the reference). */
	if (setup(fd, mem, disk) < 0) goto out;
	if (set_fault(fd, S1, CLASSES, PROB, MAXD) < 0) goto out;
	{
		struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
					    .max_insns = MIDPOINT_INSNS };
		if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); goto out; }
		if (step.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "hvx_fault_test: (c) midpoint stop reason=%u\n", step.reason);
			goto out;
		}
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	{ struct hvx_disk dget = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
	  if (ioctl(fd, HVX_GET_DISK, &dget) < 0) { perror("HVX_GET_DISK"); goto out; } }
	if (!mid.vblk_completion_armed || mid.fault_req_seq != N_REQ) {
		fprintf(stderr, "hvx_fault_test: (c) snapshot missed pending completion / req_seq "
			"(armed=%u req_seq=%" PRIu64 ")\n", mid.vblk_completion_armed,
			(uint64_t)mid.fault_req_seq);
		goto out;
	}
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem, disk_out);
	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, ramsave, GUEST_MEM_BYTES);
	{ struct hvx_disk dput = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
	  if (ioctl(fd, HVX_SET_DISK, &dput) < 0) { perror("HVX_SET_DISK"); goto out; } }
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem, disk_out);
	ok_branch = (fp_orig == fp_branch) && (fp_branch == fp_a1) && fp_orig;
	printf("hvx_fault_test: (c) branch fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a1, ok_branch ? "OK" : "FAIL");

	/* (d) faults DISABLED => byte-for-byte the M20/M21 device: everything normal,
	 * completion un-delayed, used ring in submission order; and reproducible. */
	printf("hvx_fault_test: (d) faults DISABLED (classes=0) -- run #1\n");
	fp_dis1 = run_scenario(fd, mem, disk, disk_out, 0, 0, 0, 0, &ok_dis1);
	fp_dis2 = run_scenario(fd, mem, disk, disk_out, 0, 0, 0, 0, &ok_dis2);
	ok_disabled = ok_dis1 && ok_dis2 && (fp_dis1 == fp_dis2) && fp_dis1;
	printf("hvx_fault_test: (d) all-normal=%d/%d fp=%016" PRIx64 " reproducible=%d -> %s\n",
	       ok_dis1, ok_dis2, fp_dis1, (fp_dis1 == fp_dis2), ok_disabled ? "OK" : "FAIL");

	/* stretch: torn write */
	ok_torn = (torn_check(fd, mem, disk, disk_out) == 0);

	if (ok_repro && ok_delay && ok_seed && ok_branch && ok_disabled && ok_torn) {
		printf("hvx_fault_test: MILESTONE 22 PASS: seeded deterministic disk fault "
		       "injection -- per-request delay/error/drop + completion reorder + torn "
		       "writes; schedule matches the prediction; reproducible under a seed; the "
		       "seed controls the schedule; branch reproduces it; disabled == M20/M21\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_fault_test: FAIL: repro=%d delay=%d seed=%d branch=%d "
			"disabled=%d torn=%d\n", ok_repro, ok_delay, ok_seed, ok_branch,
			ok_disabled, ok_torn);
	}

out:
	free(disk); free(disk_out); free(ramsave); free(disksave);
	close(fd);
	return rc;
}

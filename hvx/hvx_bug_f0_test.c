// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_bug_f0_test - milestone 25, phase F0: the durability-oracle loop in a
 * HAND-WRITTEN guest (no Linux; runs in milliseconds). Proves the ENTIRE logical
 * loop of the flagship -- record-committed -> seeded DROP fault -> oracle detects
 * the loss via a device read-back -> reproduce (replay + branch) -- before any
 * real-Linux boot cost. A permanent, fast regression for the M25 mechanism.
 * =============================================================================
 *
 * A tiny guest submits, in ONE virtqueue kick over the Phase-A PIO doorbell
 * transport:
 *   req0: WRITE a 512-byte KV record (magic|key|value|...) to sector S   [the "put"]
 *   req1: READ sector S back into a separate buffer                       [the read-back]
 * then waits for the completion and HLTs. The read-back buffer is the
 * O_DIRECT-equivalent: it is what a real device read returns, NOT a cached copy.
 *
 * The userspace ORACLE inspects that read-back buffer:
 *   - matches the committed record  -> OK        (the key is durable)
 *   - does not match (e.g. zeros)   -> VIOLATION (a committed key was lost)
 *
 * Checks:
 *   (a) NO FAULT   -> oracle OK; the write persisted and the read-back matches;
 *                     two runs are byte-identical (fingerprint).
 *   (b) SEEDED DROP-> oracle VIOLATION; the device acked S_OK (the "put" succeeded)
 *                     but did NOT persist, so the read-back is the pre-write sector
 *                     (zeros) -> the committed key is gone; two runs identical.
 *   (c) BRANCH     -> snapshot after the kick / before the completion, finish twice
 *                     (straight + restored) -> identical failing state, proving the
 *                     pending completion + fault PRNG + disk are all captured.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_bug_f0_test hvx_bug_f0_test.c
 * Run   (inside L1):  sudo ./hvx_bug_f0_test
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

#define DESC_GPA        0x8000UL
#define AVAIL_GPA       0x8800UL
#define USED_GPA        0x9000UL
#define WHDR_GPA        0xA000UL		/* write chain: header / data / status */
#define WDATA_GPA       0xA200UL
#define WST_GPA         0xA400UL
#define RHDR_GPA        0xB000UL		/* read chain:  header / data / status */
#define RDATA_GPA       0xB200UL
#define RST_GPA         0xB400UL
#define QSIZE           32

#define SEC             7			/* the key's sector */
#define DISK_SECTORS    32
#define DISK_BYTES      (DISK_SECTORS * HVX_VBLK_SECTOR)
#define MIDPOINT_INSNS  20			/* after the NOTIFY kick, before the completion */

#define KV_MAGIC        0x4859444F4B565231ULL	/* "HYDOKVR1" -- matches hydokv.c */
#define KV_KEY          0x2aULL
#define KV_VALUE        0xA5A5A5A5DEADBEEFULL

/* ---- little emitters (machine-checked guest program) ---- */
static size_t emit_out32(uint8_t *p, uint32_t val, uint32_t port)
{
	size_t n = 0;
	p[n++] = 0xb8; memcpy(p + n, &val, 4);  n += 4;	/* mov eax, imm32 */
	p[n++] = 0xba; memcpy(p + n, &port, 4); n += 4;	/* mov edx, imm32 */
	p[n++] = 0xef;					/* out dx, eax */
	return n;
}
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
static void put_u64(uint8_t *b, uint64_t v) { memcpy(b, &v, 8); }

/* Lay out the committed record + the write/read chains in guest RAM. */
static void build_kv_queue(void *mem)
{
	uint16_t avail[2 + 2];
	uint8_t *rec = (uint8_t *)mem + WDATA_GPA;

	/* the committed record the store "puts" (matches hydokv's layout) */
	memset(rec, 0, HVX_VBLK_SECTOR);
	put_u64(rec + 0,  KV_MAGIC);
	put_u64(rec + 8,  KV_KEY);
	put_u64(rec + 16, KV_VALUE);
	memset(rec + 32, 0xC0 + (KV_KEY & 0x3f), HVX_VBLK_SECTOR - 32);

	/* read-back buffer starts as a poison pattern so an untouched read is visible */
	memset((uint8_t *)mem + RDATA_GPA, 0xEE, HVX_VBLK_SECTOR);
	((uint8_t *)mem)[WST_GPA] = 0;
	((uint8_t *)mem)[RST_GPA] = 0;

	put_hdr(mem, WHDR_GPA, VIRTIO_BLK_T_OUT, SEC);
	put_hdr(mem, RHDR_GPA, VIRTIO_BLK_T_IN,  SEC);

	/* write chain: desc 0,1,2 */
	put_desc(mem, 0, WHDR_GPA,  16,              VIRTQ_DESC_F_NEXT, 1);
	put_desc(mem, 1, WDATA_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT, 2);
	put_desc(mem, 2, WST_GPA,   1,               VIRTQ_DESC_F_WRITE, 0);
	/* read chain: desc 3,4,5 */
	put_desc(mem, 3, RHDR_GPA,  16,              VIRTQ_DESC_F_NEXT, 4);
	put_desc(mem, 4, RDATA_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 5);
	put_desc(mem, 5, RST_GPA,   1,               VIRTQ_DESC_F_WRITE, 0);

	avail[0] = 0;		/* flags */
	avail[1] = 2;		/* idx   */
	avail[2] = 0;		/* -> write chain head (desc 0) */
	avail[3] = 3;		/* -> read chain head  (desc 3) */
	memcpy((uint8_t *)mem + AVAIL_GPA, avail, sizeof(avail));
	memset((uint8_t *)mem + USED_GPA, 0, 8 + 8 * QSIZE);
}

/* ---- canonical fingerprint: regs + device/fault state + guest RAM + disk ---- */
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

	h = fnv(h, "hvx-bug-f0-v1", 13);
	h = fnv(h, &s.regs, sizeof(s.regs));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.vblk_last_avail, sizeof(s.vblk_last_avail));
	h = fnv(h, &s.fault_seed, sizeof(s.fault_seed));
	h = fnv(h, &s.fault_classes, sizeof(s.fault_classes));
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
static int set_fault(int fd, uint64_t seed, uint32_t classes, uint32_t prob)
{
	struct hvx_fault f = { .seed = seed, .classes = classes, .probability = prob };
	return ioctl(fd, HVX_SET_FAULT, &f) < 0 ? -1 : 0;
}

/* Reset disk (zeroed) + guest RAM + regs for a fresh run. */
static int setup(int fd, void *mem, uint8_t *disk)
{
	struct hvx_regs regs;
	memset(disk, 0, DISK_BYTES);
	if (set_disk(fd, disk) < 0) { perror("HVX_SET_DISK"); return -1; }
	build_kv_queue(mem);
	*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip = HVX_GUEST_CODE_BASE;
	regs.rsp = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x202;		/* IF=1: required to deliver the completion IRQ */
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
			if (++throttles > 128) { *out = ctl; return -1; }
			continue;
		}
		*out = ctl;
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}

/* The ORACLE: does the read-back buffer hold the committed record? */
static int oracle_durable(void *mem, uint64_t *got_value)
{
	const uint8_t *rb = (const uint8_t *)mem + RDATA_GPA;
	uint64_t magic, key, value;
	memcpy(&magic, rb + 0, 8);
	memcpy(&key,   rb + 8, 8);
	memcpy(&value, rb + 16, 8);
	*got_value = value;
	return (magic == KV_MAGIC && key == KV_KEY && value == KV_VALUE);
}

/* Run one scenario to HLT; report oracle verdict + fingerprint. */
static uint64_t run_scenario(int fd, void *mem, uint8_t *disk, uint8_t *disk_out,
			     uint64_t seed, uint32_t classes, uint32_t prob,
			     int *durable_out, const char *label)
{
	struct hvx_run_ctl r;
	struct hvx_disk dk = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disk_out };
	uint64_t gv = 0;
	int durable;

	if (setup(fd, mem, disk) < 0) return 0;
	if (set_fault(fd, seed, classes, prob) < 0) { perror("HVX_SET_FAULT"); return 0; }
	if (run_to_halt(fd, 1, &r) < 0) { fprintf(stderr, "  %s: guest never HLTed\n", label); return 0; }
	if (ioctl(fd, HVX_GET_DISK, &dk) < 0) { perror("HVX_GET_DISK"); return 0; }

	durable = oracle_durable(mem, &gv);
	*durable_out = durable;
	printf("  %-10s put(key=0x%llx,val=0x%llx) committed; read-back value=0x%016llx -> %s\n",
	       label, (unsigned long long)KV_KEY, (unsigned long long)KV_VALUE,
	       (unsigned long long)gv,
	       durable ? "HYDO-ORACLE: OK durable"
		       : "HYDO-ORACLE: VIOLATION (committed key lost)");
	return fingerprint(fd, mem, disk_out);
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	uint8_t *disk, *disk_out, *ramsave, *disksave;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl r;
	struct hvx_snapshot mid;
	const uint64_t SEED = 0xD00D;
	uint64_t fp_nf1, fp_nf2, fp_f1, fp_f2, fp_orig, fp_branch;
	uint64_t gv_dummy = 0;
	int d_nf1, d_nf2, d_f1, d_f2, d_orig, d_branch;
	int ok_nofault, ok_fault, ok_branch;

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

	/* (a) NO FAULT: the committed key is durable; reproducible. */
	printf("hvx_bug_f0_test: (a) NO FAULT -- the app is correct\n");
	fp_nf1 = run_scenario(fd, mem, disk, disk_out, 0, 0, 0, &d_nf1, "no-fault#1");
	fp_nf2 = run_scenario(fd, mem, disk, disk_out, 0, 0, 0, &d_nf2, "no-fault#2");
	ok_nofault = d_nf1 && d_nf2 && fp_nf1 && (fp_nf1 == fp_nf2);
	printf("hvx_bug_f0_test: (a) durable=%d/%d fp=%016" PRIx64 " reproducible=%d -> %s\n",
	       d_nf1, d_nf2, fp_nf1, (fp_nf1 == fp_nf2), ok_nofault ? "OK" : "FAIL");

	/* (b) SEEDED DROP (prob 100 => the single write is dropped): committed key lost;
	 * reproducible. */
	printf("hvx_bug_f0_test: (b) SEEDED DROP -- the fault surfaces the durability bug\n");
	fp_f1 = run_scenario(fd, mem, disk, disk_out, SEED, HVX_FAULT_DROP, 100, &d_f1, "drop#1");
	fp_f2 = run_scenario(fd, mem, disk, disk_out, SEED, HVX_FAULT_DROP, 100, &d_f2, "drop#2");
	ok_fault = !d_f1 && !d_f2 && fp_f1 && (fp_f1 == fp_f2) && (fp_f1 != fp_nf1);
	printf("hvx_bug_f0_test: (b) lost=%d/%d fp=%016" PRIx64 " reproducible=%d (!= no-fault) -> %s\n",
	       !d_f1, !d_f2, fp_f1, (fp_f1 == fp_f2), ok_fault ? "OK" : "FAIL");

	/* (c) BRANCH: snapshot after the kick / before the completion; finish straight
	 * and restored -> identical failing state. */
	printf("hvx_bug_f0_test: (c) BRANCH-FROM-SNAPSHOT across the faulted write\n");
	if (setup(fd, mem, disk) < 0) goto out;
	if (set_fault(fd, SEED, HVX_FAULT_DROP, 100) < 0) goto out;
	{
		struct hvx_run_ctl step = { .flags = HVX_RUN_SINGLE_STEP | HVX_RUN_RESET_ICOUNT,
					    .max_insns = MIDPOINT_INSNS };
		if (ioctl(fd, HVX_RUNC, &step) < 0) { perror("HVX_RUNC"); goto out; }
		if (step.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "hvx_bug_f0_test: (c) midpoint stop reason=%u\n", step.reason);
			goto out;
		}
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	{ struct hvx_disk dget = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
	  if (ioctl(fd, HVX_GET_DISK, &dget) < 0) { perror("HVX_GET_DISK"); goto out; } }
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem, disk_out);
	d_orig = oracle_durable(mem, &gv_dummy);
	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto out; }
	memcpy(mem, ramsave, GUEST_MEM_BYTES);
	{ struct hvx_disk dput = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
	  if (ioctl(fd, HVX_SET_DISK, &dput) < 0) { perror("HVX_SET_DISK"); goto out; } }
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem, disk_out);
	d_branch = oracle_durable(mem, &gv_dummy);
	ok_branch = fp_orig && (fp_orig == fp_branch) && (fp_branch == fp_f1) && !d_orig && !d_branch;
	printf("hvx_bug_f0_test: (c) fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") lost=%d/%d -> %s\n",
	       fp_orig, fp_branch, fp_f1, !d_orig, !d_branch, ok_branch ? "OK" : "FAIL");

	if (ok_nofault && ok_fault && ok_branch) {
		printf("hvx_bug_f0_test: F0 PASS: committed-key durability oracle + seeded DROP + "
		       "branch/replay loop works (no-fault=durable, drop=lost, reproducible, "
		       "branch-identical) -- the flagship logic, at zero boot cost\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_bug_f0_test: FAIL: nofault=%d fault=%d branch=%d\n",
			ok_nofault, ok_fault, ok_branch);
	}

out:
	free(disk); free(disk_out); free(ramsave); free(disksave);
	close(fd);
	return rc;
}

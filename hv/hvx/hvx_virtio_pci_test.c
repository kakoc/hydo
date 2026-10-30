// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_virtio_pci_test - Phase B of milestone 20: the PCI TRANSPORT on top of the
 * (Phase-A-proven) deterministic virtio-blk core.
 * =============================================================================
 *
 * A hand-written guest does the real thing without needing full Linux:
 *   1. enumerates PCI config space (0xCF8/0xCFC) -- finds vendor 0x1af4, device
 *      0x1042 (modern virtio-blk) at bus 0/dev 3/fn 0, reads BAR0;
 *   2. enables the device (PCI command: memory space + bus master);
 *   3. routes its INTx through the IOAPIC (redirection entry HVX_VBLK_GSI ->
 *      vector 0x20);
 *   4. negotiates features + sets up the virtqueue through the MODERN virtio-pci
 *      common-config MMIO in BAR0 (device_status handshake, VIRTIO_F_VERSION_1,
 *      queue_desc/driver/device, queue_enable, DRIVER_OK);
 *   5. submits a WRITE + a READ, kicks via the notify region, and takes the
 *      completion interrupt through the IOAPIC -> M8 injection path. Its ISR acks
 *      the device (reads the virtio ISR register) and EOIs the LAPIC.
 *
 * Same virtio-blk core as Phase A; only the discovery/transport layer differs.
 * Proves the PCI machinery is deterministic + snapshot-reproducible: data
 * round-trips through the disk, the completion is reproducible, and replay /
 * round-trip / branch all fingerprint-equal.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_virtio_pci_test hvx_virtio_pci_test.c
 * Run   (inside L1):  sudo ./hvx_virtio_pci_test
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
#define INTR_CTR_GPA    0x2f00UL		/* the ISR increments this */
#define ISR_GPA         0x2800UL		/* GPA_ISR: built-in gate 0x20 targets here */
#define VECTOR          0x20			/* completion vector (built-in IDT gate) */
#define IOAPIC_MMIO     0xfec00000ULL

/* guest-RAM layout for the virtqueue + buffers + PCI-enumeration scratch */
#define VID_STORE       0x7000UL		/* guest stores config[0x00] here */
#define BAR_STORE       0x7008UL		/* guest stores config[0x10] (BAR0) here */
#define DESC_GPA        0x8000UL
#define AVAIL_GPA       0x8800UL
#define USED_GPA        0x9000UL
#define HDR0_GPA        0xA000UL
#define WBUF_GPA        0xA200UL
#define ST0_GPA         0xA400UL
#define HDR1_GPA        0xA800UL
#define RBUF_GPA        0xAA00UL
#define ST1_GPA         0xAC00UL
#define QSIZE           8

#define DISK_SECTORS    8
#define DISK_BYTES      (DISK_SECTORS * HVX_VBLK_SECTOR)
#define WRITE_SECTOR    5
#define READ_SECTOR     2

/* The custom guest ISR (overwrites GPA_ISR): ack the device (read the virtio ISR
 * register at BAR+0x2000, read-to-clear), bump the completion counter, EOI the
 * LAPIC (x2APIC EOI MSR 0x80b), IRETQ. Longer than the built-in inc/iretq blob. */
static const uint8_t isr_blob[] = {
	0x50, 0x51, 0x52,				/* push rax; push rcx; push rdx */
	0x48, 0xb8, 0x00, 0x20, 0x00, 0xfe, 0x00, 0x00, 0x00, 0x00, /* movabs rax,0xfe002000 */
	0x8b, 0x00,					/* mov eax,[rax]  (ack: read-to-clear) */
	0x48, 0xff, 0x04, 0x25, 0x00, 0x2f, 0x00, 0x00,	/* inc qword [0x2f00] */
	0xb9, 0x0b, 0x08, 0x00, 0x00,			/* mov ecx, 0x80b (x2APIC EOI) */
	0x31, 0xc0,					/* xor eax,eax */
	0x31, 0xd2,					/* xor edx,edx */
	0x0f, 0x30,					/* wrmsr (EOI) */
	0x5a, 0x59, 0x58,				/* pop rdx; pop rcx; pop rax */
	0x48, 0xcf,					/* iretq */
};

/* ---- machine-code emitters (so the guest is checked, not hand-hexed) ---- */
static size_t movabs(uint8_t *c, size_t n, uint8_t reg_op, uint64_t v)
{
	c[n++] = 0x48; c[n++] = reg_op; memcpy(c + n, &v, 8); n += 8; return n;
}
static size_t mmio_w32(uint8_t *c, size_t n, uint64_t addr, uint32_t val)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0xbe; memcpy(c + n, &val, 4); n += 4;	/* mov esi, val   */
	c[n++] = 0x89; c[n++] = 0x37;			/* mov [rdi], esi */
	return n;
}
static size_t mmio_w16(uint8_t *c, size_t n, uint64_t addr, uint16_t val)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x66; c[n++] = 0xbe; memcpy(c + n, &val, 2); n += 2;	/* mov si, val */
	c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x37;	/* mov [rdi], si */
	return n;
}
static size_t mmio_r32_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x8b; c[n++] = 0x37;			/* mov esi, [rdi] */
	return n;
}
static size_t mmio_wr_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x89; c[n++] = 0x37;			/* mov [rdi], esi */
	return n;
}
static size_t pio_out32(uint8_t *c, size_t n, uint32_t val, uint32_t port)
{
	c[n++] = 0xb8; memcpy(c + n, &val, 4); n += 4;	/* mov eax, val  */
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;	/* mov edx, port */
	c[n++] = 0xef;					/* out dx, eax */
	return n;
}
static uint32_t cfg_addr(unsigned reg) { return 0x80000000u | (HVX_VBLK_PCI_DEV << 11) | (reg & 0xfc); }

static size_t pio_cfg_read_store(uint8_t *c, size_t n, unsigned reg, uint64_t store)
{
	uint32_t port = HVX_PCI_CONFIG_DATA;

	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);	/* CONFIG_ADDRESS */
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;		/* mov edx, 0xcfc */
	c[n++] = 0xed;						/* in eax, dx */
	n = movabs(c, n, 0xbf, store);				/* movabs rdi, store */
	c[n++] = 0x89; c[n++] = 0x07;				/* mov [rdi], eax */
	return n;
}
static size_t pio_cfg_write(uint8_t *c, size_t n, unsigned reg, uint32_t val)
{
	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);
	n = pio_out32(c, n, val, HVX_PCI_CONFIG_DATA);
	return n;
}

static size_t build_guest(uint8_t *c)
{
	const uint64_t B = HVX_VBLK_BAR;
	size_t n = 0;

	/* 1. enumerate: read vendor|device and BAR0 into guest RAM */
	n = pio_cfg_read_store(c, n, 0x00, VID_STORE);
	n = pio_cfg_read_store(c, n, 0x10, BAR_STORE);
	/* 2. PCI command: memory space (bit1) + bus master (bit2) */
	n = pio_cfg_write(c, n, 0x04, 0x6);
	/* 3. route INTx: IOAPIC redirection entry HVX_VBLK_GSI -> vector 0x20, unmasked */
	n = mmio_w32(c, n, IOAPIC_MMIO, 0x10 + 2 * HVX_VBLK_GSI);	/* ioregsel */
	n = mmio_w32(c, n, IOAPIC_MMIO + 0x10, VECTOR);			/* iowin: vector, unmasked */
	/* 4. virtio modern common-config handshake */
	n = mmio_w32(c, n, B + 0x14, 1);	/* device_status = ACKNOWLEDGE */
	n = mmio_w32(c, n, B + 0x14, 3);	/* |= DRIVER */
	n = mmio_w32(c, n, B + 0x00, 1);	/* device_feature_select = 1 (high dword) */
	n = mmio_r32_esi(c, n, B + 0x04);	/* esi = device_feature (VIRTIO_F_VERSION_1) */
	n = mmio_w32(c, n, B + 0x08, 1);	/* driver_feature_select = 1 */
	n = mmio_wr_esi(c, n, B + 0x0c);	/* driver_feature = esi (accept VERSION_1) */
	n = mmio_w32(c, n, B + 0x14, 0xB);	/* |= FEATURES_OK (3|8) */
	n = mmio_w16(c, n, B + 0x16, 0);	/* queue_select = 0 */
	n = mmio_w16(c, n, B + 0x18, QSIZE);	/* queue_size = 8 */
	n = mmio_w32(c, n, B + 0x20, DESC_GPA);	/* queue_desc lo */
	n = mmio_w32(c, n, B + 0x24, 0);	/* queue_desc hi */
	n = mmio_w32(c, n, B + 0x28, AVAIL_GPA);/* queue_driver lo */
	n = mmio_w32(c, n, B + 0x2c, 0);
	n = mmio_w32(c, n, B + 0x30, USED_GPA);	/* queue_device lo */
	n = mmio_w32(c, n, B + 0x34, 0);
	n = mmio_w16(c, n, B + 0x1c, 1);	/* queue_enable = 1 */
	n = mmio_w32(c, n, B + 0x14, 0xF);	/* |= DRIVER_OK */
	/* 5. NOTIFY: write queue index 0 to the notify region */
	n = mmio_w16(c, n, B + HVX_VPCI_NOTIFY, 0);
	/* 6. wait for the completion ISR, then HLT */
	n = movabs(c, n, 0xbb, INTR_CTR_GPA);		/* movabs rbx, 0x2f00 */
	c[n++] = 0x48; c[n++] = 0x8b; c[n++] = 0x03;	/* wait: mov rax,[rbx] */
	c[n++] = 0x48; c[n++] = 0x85; c[n++] = 0xc0;	/* test rax,rax */
	c[n++] = 0x74; c[n++] = 0xf8;			/* jz wait (-8) */
	c[n++] = 0xf4;					/* hlt */
	return n;
}

/* ---- one split-queue descriptor + queue layout (identical to Phase A) ---- */
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
static void build_queue(void *mem)
{
	unsigned i;
	uint8_t zero = 0;
	uint16_t avail[4];

	put_hdr(mem, HDR0_GPA, VIRTIO_BLK_T_OUT, WRITE_SECTOR);
	for (i = 0; i < HVX_VBLK_SECTOR; i++)
		((uint8_t *)mem)[WBUF_GPA + i] = (uint8_t)(0xA0 + (i & 0x1f));
	memcpy((uint8_t *)mem + ST0_GPA, &zero, 1);
	put_desc(mem, 0, HDR0_GPA, 16,              VIRTQ_DESC_F_NEXT, 1);
	put_desc(mem, 1, WBUF_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT, 2);
	put_desc(mem, 2, ST0_GPA,  1,               VIRTQ_DESC_F_WRITE, 0);

	put_hdr(mem, HDR1_GPA, VIRTIO_BLK_T_IN, READ_SECTOR);
	memset((uint8_t *)mem + RBUF_GPA, 0, HVX_VBLK_SECTOR);
	memcpy((uint8_t *)mem + ST1_GPA, &zero, 1);
	put_desc(mem, 3, HDR1_GPA, 16,              VIRTQ_DESC_F_NEXT, 4);
	put_desc(mem, 4, RBUF_GPA, HVX_VBLK_SECTOR, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 5);
	put_desc(mem, 5, ST1_GPA,  1,               VIRTQ_DESC_F_WRITE, 0);

	avail[0] = 0; avail[1] = 2; avail[2] = 0; avail[3] = 3;
	memcpy((uint8_t *)mem + AVAIL_GPA, avail, sizeof(avail));
	memset((uint8_t *)mem + USED_GPA, 0, 8 + 8 * QSIZE);
}

/* ---- fingerprint (regs + device + RAM + disk) ---- */
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

	h = fnv(h, "hvx-vpci-v1", 11);
	h = fnv(h, &s.regs, sizeof(s.regs));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.lapic_in_service, sizeof(s.lapic_in_service));
	h = fnv(h, &s.vblk_last_avail, sizeof(s.vblk_last_avail));
	h = fnv(h, &s.vblk_isr, sizeof(s.vblk_isr));
	h = fnv(h, &s.vblk_status, sizeof(s.vblk_status));
	h = fnv(h, &s.vblk_irq_pending, sizeof(s.vblk_irq_pending));
	h = fnv(h, &s.vblk_completion_armed, sizeof(s.vblk_completion_armed));
	h = fnv(h, &s.vblk_completion_due, sizeof(s.vblk_completion_due));
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
	memcpy((uint8_t *)mem + ISR_GPA, isr_blob, sizeof(isr_blob));	/* custom ack+EOI ISR */
	*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x202;			/* IF=1: required to deliver the INTx */
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
				fprintf(stderr, "hvx_virtio_pci_test: FAIL: guest never HLTed "
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
/* Single-step until the device has a completion scheduled but not yet fired
 * (vblk_completion_armed set) -- the branch midpoint. Robust to instruction
 * counts. Returns 0 on success. */
static int step_until_armed(int fd)
{
	struct hvx_run_ctl ctl;
	struct hvx_snapshot s;
	int k;

	for (k = 0; k < 2000; k++) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (k == 0 ? HVX_RUN_RESET_ICOUNT : 0);
		ctl.max_insns = 1;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE) { k--; continue; }
		if (ctl.reason == EXIT_HLT) return -1;		/* fired already -> too late */
		if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); return -1; }
		if (s.vblk_completion_armed)
			return 0;
	}
	return -1;
}
static uint64_t rd64(void *mem, unsigned long gpa)
{
	return *(volatile uint64_t *)((uint8_t *)mem + gpa);
}
static uint32_t rd32(void *mem, unsigned long gpa)
{
	return *(volatile uint32_t *)((uint8_t *)mem + gpa);
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
	int ok_enum, ok_data, ok_replay, ok_round, ok_branch;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	disk     = malloc(DISK_BYTES);
	disk_out = malloc(DISK_BYTES);
	ramsave  = malloc(GUEST_MEM_BYTES);
	disksave = malloc(DISK_BYTES);
	if (!disk || !disk_out || !ramsave || !disksave) { perror("malloc"); goto out; }

	memset(disk, 0, DISK_BYTES);
	for (i = 0; i < HVX_VBLK_SECTOR; i++)
		disk[READ_SECTOR * HVX_VBLK_SECTOR + i] = (uint8_t)(0x30 + (i & 0x1f));

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }

	{ uint8_t code[512]; size_t clen = build_guest(code);
	  memcpy((uint8_t *)mem + HVX_GUEST_CODE_BASE, code, clen); }

	/* run #1 */
	if (setup(fd, mem, disk) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) {
		fprintf(stderr, "hvx_virtio_pci_test: FAIL: run #1 did not HLT (reason=%u)\n", r.reason);
		goto out;
	}

	/* enumeration: the guest read vendor|device and BAR0 out of config space */
	ok_enum = (rd32(mem, VID_STORE) == (((uint32_t)HVX_VBLK_DEVICE << 16) | HVX_VBLK_VENDOR)) &&
		  (rd32(mem, BAR_STORE) == (uint32_t)HVX_VBLK_BAR);
	printf("hvx_virtio_pci_test: enum vendor:device=0x%08x BAR0=0x%08x -> %s\n",
	       rd32(mem, VID_STORE), rd32(mem, BAR_STORE), ok_enum ? "OK" : "BAD");

	/* data round-trip + completion */
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
		printf("hvx_virtio_pci_test: data write=%s read=%s intr=%" PRIu64 " icount=%" PRIu64 "\n",
		       wr_ok ? "OK" : "BAD", rd_ok ? "OK" : "BAD",
		       rd64(mem, INTR_CTR_GPA), (uint64_t)r.icount);
		if (!ok_enum || !ok_data) {
			fprintf(stderr, "hvx_virtio_pci_test: FAIL: enumeration / data / completion\n");
			goto out;
		}
	}
	fp_a = fingerprint(fd, mem, disk_out);

	/* replay */
	if (setup(fd, mem, disk) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) goto out;
	fp_b = fingerprint(fd, mem, disk_out);
	ok_replay = (fp_a == fp_b);
	printf("hvx_virtio_pci_test: replay     fp_a=%016" PRIx64 " fp_b=%016" PRIx64 " -> %s\n",
	       fp_a, fp_b, ok_replay ? "OK" : "MISMATCH");

	/* round-trip: snapshot + restore (RAM + disk) */
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
	printf("hvx_virtio_pci_test: round-trip fp=%016" PRIx64 " (vs %016" PRIx64 ") -> %s\n",
	       fp_round, fp_b, ok_round ? "OK" : "MISMATCH");

	/* branch: step to the armed-but-not-fired point, snapshot RAM+disk, finish;
	 * restore, finish; assert identical. */
	if (setup(fd, mem, disk) < 0) goto out;
	if (step_until_armed(fd) < 0) {
		fprintf(stderr, "hvx_virtio_pci_test: FAIL: could not reach the armed midpoint\n");
		goto out;
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	{
		struct hvx_disk dget = { .len = DISK_BYTES, .data = (uint64_t)(uintptr_t)disksave };
		if (ioctl(fd, HVX_GET_DISK, &dget) < 0) { perror("GET_DISK"); goto out; }
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
	printf("hvx_virtio_pci_test: branch     fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a,
	       ok_branch ? "OK" : "MISMATCH");

	if (ok_enum && ok_data && ok_replay && ok_round && ok_branch) {
		printf("hvx_virtio_pci_test: MILESTONE 20 PHASE B PASS: hand-written guest enumerated "
		       "the virtio-blk PCI function, negotiated features, set up the virtqueue, did "
		       "read+write, and took the completion INTx through the IOAPIC -- deterministic + "
		       "snapshot-reproducible\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_virtio_pci_test: FAIL: a check did not hold\n");
	}

out:
	free(disk); free(disk_out); free(ramsave); free(disksave);
	close(fd);
	return rc;
}

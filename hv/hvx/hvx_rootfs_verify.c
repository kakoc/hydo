// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_rootfs_verify - milestone 29 (Rung 3, phases R3.4 + R3.5): boot a REAL ext4
 * READ-ONLY root disk via switch_root (instead of the in-RAM initramfs), with a
 * small ext2 READ-WRITE data disk, and prove determinism AT SCALE by snapshotting
 * ONLY guest RAM + the small rw disk -- the big static ro-root is EXCLUDED (it is
 * re-attached identically; its hash is asserted stable).
 * =============================================================================
 *
 * The backing image (build/rootfs-disk.img) is ONE MBR-partitioned virtio-blk disk:
 *   p1 @ LBA 2048       = ext4 ro ROOT (busybox + /sbin/init), the big static image
 *   p2 @ LBA 1050624    = ext2 rw DATA disk (the workload's scratch)
 * The tiny initramfs (build/initramfs-rootfs.cpio.gz) loads the virtio-blk + ext4 +
 * ext2 module stack, mounts /dev/vda1 READ-ONLY, and `switch_root`s into it. The
 * real root's /sbin/init mounts /dev/vda2 rw on /data, writes NKEYS committed
 * records, `sync` + drops the page cache (so the oracle reads the DEVICE, not the
 * cache), then an oracle re-reads them and prints:
 *     HYDO-ORACLE: OK keys=<K> durable
 *     HYDO-ORACLE: VIOLATION key=<k> expected=<v> got=<g>
 * then the stop marker  hydo-rootfs-marker: workload complete.
 *
 * WHY snapshot only RAM + the small rw disk (the R3.5 scale point): a GB-scale
 * ro-root cannot be double-buffered in L1's 4 GiB. Because the root is mounted RO,
 * the guest NEVER writes p1, so it is a static image -- on a branch/round-trip we
 * re-attach the identical bytes (they are already in the disk page array, untouched)
 * and only save/restore guest RAM + p2. We ASSERT the ro-root is static by hashing
 * p1 after every boot and comparing to the offline image's p1 bytes; if a byte of p1
 * ever changed, the assertion (and thus the branch's correctness) would fail.
 *
 * Fingerprint tractability: RAM and the rw partition are hashed ZERO-COPY over the
 * shared mmaps (RAM mmap at offset 0, disk mmap at HVX_DISK_MMAP_PGOFF*PAGE) -- no
 * whole-image GB copy. The disk is LOADED zero-copy too (HVX_SET_DISK data=0 => a
 * zeroed page array, filled by memcpy into the disk mmap).
 *
 * The four hydo-verify checks (now at GB-ish scale on a real ext4 root):
 *   (1) REPLAY:      two fresh boots are bit-identical (console + snapshot + RAM +
 *                    rw-disk); verdict OK; ro-root hash stable.
 *   (2) ROUND-TRIP:  GET_SNAPSHOT + save(RAM, rw) then SET_SNAPSHOT + restore ==
 *                    same fingerprint (the rw-disk-only snapshot is complete).
 *   (3) BRANCH:      snapshot at a mid-boot marker (before the rw workload), continue
 *                    twice (straight + restored) -> identical; the rw-disk workload +
 *                    RAM are reproduced from the RAM+rw snapshot with the ro-root
 *                    excluded.
 *   (4) SEED-SENSITIVITY: a SEEDED virtio-blk DROP fault on the rw-disk writes loses
 *                    a committed record (oracle VIOLATION); seed A and seed B lose
 *                    different records, each reproducible.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_rootfs_verify hvx_rootfs_verify.c
 * Run   (inside L1):  sudo ./hvx_rootfs_verify /tmp/bzImage /tmp/initramfs-rootfs.cpio.gz /tmp/rootfs-disk.img
 */

#include "hvx_boot.h"

/* Printed by the real root's /sbin/init right after the oracle -- the stop point. */
#define FINAL_MARKER "hydo-rootfs-marker: workload complete"
/* A clean, deep-in-kernel-init marker BEFORE the initramfs even unpacks -- so no
 * virtio-blk / disk I/O is in flight at the snapshot boundary (the same MID point the
 * SQLite/disk harnesses use). Branching here forces the snapshot to reproduce the
 * WHOLE rest: the module loads, the ext4 ro-root mount, switch_root, AND the ext2 rw
 * workload -- a strong branch test whose endpoint equals a fresh boot's. */
#define MID_MARKER   "Trying to unpack rootfs image as initramfs"

#define ROOTFS_CMDLINE \
	"root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

#define ROOTFS_MEM_MIB 512UL            /* moderate: exercises M28 >256 MiB, fits L1 */
#define BOOT_TSC_STEP  0x10000ULL
#define UBUF_CAP       (1UL << 20)
#define MAX_RUN_ITERS  400000000ULL

/* Seeded disk-fault policy for the seed-sensitivity check. DROP is WRITE-only, and
 * the root is mounted RO (no root writes), so DROP faults hit ONLY the rw-data
 * writes; a moderate probability loses a committed record with the fs still
 * readable, and the exact lost record is a pure function of the seed. */
#define FAULT_PROB  12u
#define SEED_A      0x2905A0ULL
#define SEED_B      0x2905A1ULL

/* ---- canonical fingerprint (FNV-1a; matches the other hvx verify harnesses) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}

struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *disk_mm;	/* zero-copy disk window (shared with the kernel pages) */
	uint64_t disk_len;
	uint8_t *ubuf;		/* reconstructed console (this timeline) */
	size_t ulen;
	size_t drained;
	const uint8_t *image;	/* the on-disk image to (re)load into the page array */
	/* MBR-derived partition byte ranges (p1 = ro root, p2 = rw data) */
	uint64_t p1_off, p1_len, p2_off, p2_len;
	/* seeded disk-fault policy (0 classes => faults disabled) */
	uint64_t fault_seed;
	uint32_t fault_classes;
	uint32_t fault_prob;
};

static long mem_find(const uint8_t *hay, size_t hlen, const char *needle)
{
	size_t nlen = strlen(needle), i;
	if (nlen == 0 || hlen < nlen)
		return -1;
	for (i = 0; i + nlen <= hlen; i++)
		if (memcmp(hay + i, needle, nlen) == 0)
			return (long)i;
	return -1;
}

static int oracle_verdict(const struct boot *b, char *out, size_t outsz)
{
	long at = mem_find(b->ubuf, b->ulen, "HYDO-ORACLE:");
	size_t i = 0;

	if (at < 0)
		return 0;
	while ((size_t)at + i < b->ulen && b->ubuf[at + i] != '\n' && i + 1 < outsz) {
		out[i] = (char)b->ubuf[at + i];
		i++;
	}
	out[i] = '\0';
	return 1;
}
static int verdict_is_ok(const char *v)        { return strstr(v, "OK") != NULL; }
static int verdict_is_violation(const char *v) { return strstr(v, "VIOLATION") != NULL; }

/* Parse the MBR (build-linux.sh §2i layout) to recover p1/p2 byte ranges. */
static int mbr_partitions(const uint8_t *img, size_t img_len, struct boot *b)
{
	uint32_t p1_lba, p1_n, p2_lba, p2_n;

	if (img_len < 512 || img[0x1fe] != 0x55 || img[0x1ff] != 0xaa) {
		fprintf(stderr, "hvx_rootfs_verify: image has no MBR boot signature\n");
		return -1;
	}
	p1_lba = (uint32_t)img[0x1be + 8] | (uint32_t)img[0x1be + 9] << 8 |
		 (uint32_t)img[0x1be + 10] << 16 | (uint32_t)img[0x1be + 11] << 24;
	p1_n   = (uint32_t)img[0x1be + 12] | (uint32_t)img[0x1be + 13] << 8 |
		 (uint32_t)img[0x1be + 14] << 16 | (uint32_t)img[0x1be + 15] << 24;
	p2_lba = (uint32_t)img[0x1ce + 8] | (uint32_t)img[0x1ce + 9] << 8 |
		 (uint32_t)img[0x1ce + 10] << 16 | (uint32_t)img[0x1ce + 11] << 24;
	p2_n   = (uint32_t)img[0x1ce + 12] | (uint32_t)img[0x1ce + 13] << 8 |
		 (uint32_t)img[0x1ce + 14] << 16 | (uint32_t)img[0x1ce + 15] << 24;
	if (p1_n == 0 || p2_n == 0) {
		fprintf(stderr, "hvx_rootfs_verify: MBR partition table empty\n");
		return -1;
	}
	b->p1_off = (uint64_t)p1_lba * 512; b->p1_len = (uint64_t)p1_n * 512;
	b->p2_off = (uint64_t)p2_lba * 512; b->p2_len = (uint64_t)p2_n * 512;
	if (b->p2_off + b->p2_len > b->disk_len || b->p1_off + b->p1_len > b->disk_len) {
		fprintf(stderr, "hvx_rootfs_verify: partition ranges exceed disk size\n");
		return -1;
	}
	return 0;
}

static struct hvx_console con_scratch;
static void console_drain(struct boot *b)
{
	size_t modlen, add;

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0) {
		/* never ignore a failed read: it would move the stop point (M58) */
		perror("HVX_GET_CONSOLE");
		exit(1);
	}
	modlen = con_scratch.len;
	if (modlen > HVX_CONSOLE_SZ)
		modlen = HVX_CONSOLE_SZ;
	if (modlen <= b->drained)
		return;
	add = modlen - b->drained;
	if (b->ulen + add > UBUF_CAP)
		add = UBUF_CAP - b->ulen;
	memcpy(b->ubuf + b->ulen, con_scratch.buf + b->drained, add);
	b->ulen += add;
	b->drained = modlen;
}

static void console_rebase(struct boot *b)
{
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0) {
		/* never ignore a failed read: it would move the stop point (M58) */
		perror("HVX_GET_CONSOLE");
		exit(1);
	}
	b->drained = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

static int run_until_marker(struct boot *b, const char *marker)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;

	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_rootfs_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 2048) == 0)
				fprintf(stderr, "[hvx_rootfs_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 240 ? b->ulen - 240 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_rootfs_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_rootfs_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_rootfs_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Hash the meaningful snapshot fields (identical field set + order to
 * hvx_sqlite_verify.c, so the model matches the rest of the harness family). */
static uint64_t fnv_snapshot(uint64_t h, const struct hvx_snapshot *s)
{
	h = fnv(h, &s->regs, sizeof(s->regs));
	h = fnv(h, &s->tsc, sizeof(s->tsc));
	h = fnv(h, &s->tsc_step, sizeof(s->tsc_step));
	h = fnv(h, &s->tsc_aux, sizeof(s->tsc_aux));
	h = fnv(h, &s->tsc_mode, sizeof(s->tsc_mode));
	h = fnv(h, &s->icount, sizeof(s->icount));
	h = fnv(h, &s->rdtsc_count, sizeof(s->rdtsc_count));
	h = fnv(h, &s->inject_at_icount, sizeof(s->inject_at_icount));
	h = fnv(h, &s->inject_vector, sizeof(s->inject_vector));
	h = fnv(h, &s->inject_valid, sizeof(s->inject_valid));
	h = fnv(h, s->sw_msr, sizeof(s->sw_msr));
	h = fnv(h, &s->sregs, sizeof(s->sregs));
	h = fnv(h, &s->lapic_apic_base, sizeof(s->lapic_apic_base));
	h = fnv(h, &s->lapic_tsc_deadline, sizeof(s->lapic_tsc_deadline));
	h = fnv(h, s->lapic_reg, sizeof(s->lapic_reg));
	h = fnv(h, &s->lapic_in_service, sizeof(s->lapic_in_service));
	h = fnv(h, &s->ioapic_ioregsel, sizeof(s->ioapic_ioregsel));
	h = fnv(h, &s->ioapic_id, sizeof(s->ioapic_id));
	h = fnv(h, s->ioapic_redir, sizeof(s->ioapic_redir));
	h = fnv(h, s->pic_mask, sizeof(s->pic_mask));
	h = fnv(h, &s->uart_ier, 1); h = fnv(h, &s->uart_lcr, 1);
	h = fnv(h, &s->uart_mcr, 1); h = fnv(h, &s->uart_fcr, 1);
	h = fnv(h, &s->uart_scr, 1); h = fnv(h, &s->uart_dll, 1); h = fnv(h, &s->uart_dlm, 1);
	h = fnv(h, &s->vblk_desc, sizeof(s->vblk_desc));
	h = fnv(h, &s->vblk_avail, sizeof(s->vblk_avail));
	h = fnv(h, &s->vblk_used, sizeof(s->vblk_used));
	h = fnv(h, &s->vblk_qsize, sizeof(s->vblk_qsize));
	h = fnv(h, &s->vblk_qready, sizeof(s->vblk_qready));
	h = fnv(h, &s->vblk_last_avail, sizeof(s->vblk_last_avail));
	h = fnv(h, &s->vblk_isr, sizeof(s->vblk_isr));
	h = fnv(h, &s->vblk_irq_pending, sizeof(s->vblk_irq_pending));
	h = fnv(h, &s->vblk_completion_armed, sizeof(s->vblk_completion_armed));
	h = fnv(h, &s->vblk_completion_due, sizeof(s->vblk_completion_due));
	h = fnv(h, &s->vblk_status, sizeof(s->vblk_status));
	h = fnv(h, &s->vblk_pci_command, sizeof(s->vblk_pci_command));
	h = fnv(h, &s->vblk_pci_bar0, sizeof(s->vblk_pci_bar0));
	h = fnv(h, &s->vblk_msix_control, sizeof(s->vblk_msix_control));
	h = fnv(h, &s->vblk_config_msix, sizeof(s->vblk_config_msix));
	h = fnv(h, &s->vblk_queue_msix, sizeof(s->vblk_queue_msix));
	h = fnv(h, s->vblk_msix_tbl, sizeof(s->vblk_msix_tbl));
	h = fnv(h, &s->fault_seed, sizeof(s->fault_seed));
	h = fnv(h, &s->fault_classes, sizeof(s->fault_classes));
	h = fnv(h, &s->fault_probability, sizeof(s->fault_probability));
	h = fnv(h, &s->fault_req_seq, sizeof(s->fault_req_seq));
	h = fnv(h, &s->fault_count, sizeof(s->fault_count));
	return h;
}

/* Full-state fingerprint of the SNAPSHOTTED state: console + snapshot + all guest
 * RAM + ONLY the small rw partition (p2). The big ro-root (p1) is EXCLUDED -- it is
 * static; its stability is asserted separately (ro_root_fp). */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size, dlen = b->p2_len;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }

	h = fnv(h, "hvx-rootfs-state-v1", 19);
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
	h = fnv_snapshot(h, s);
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);			/* all guest RAM (zero-copy mmap) */
	h = fnv(h, &dlen, sizeof(dlen));
	h = fnv(h, b->disk_mm + b->p2_off, b->p2_len);		/* rw data disk ONLY (zero-copy) */
	free(s);
	return h;
}

/* Hash the ro-root partition (p1) in place -- proves it is static across boots. */
static uint64_t ro_root_fp(struct boot *b)
{
	return fnv(0xcbf29ce484222325ULL, b->disk_mm + b->p1_off, b->p1_len);
}

static int boot_setup(struct boot *b, uint64_t mem_size,
		      const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const uint8_t *disk, uint64_t disk_len, const char *cmdline,
		      uint64_t fault_seed, uint32_t fault_classes, uint32_t fault_prob)
{
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_disk dk;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->disk_mm = MAP_FAILED;
	b->mem_size = mem_size;
	b->image = disk;
	b->disk_len = disk_len;
	b->fault_seed = fault_seed;
	b->fault_classes = fault_classes;
	b->fault_prob = fault_prob;
	b->ubuf = malloc(UBUF_CAP);
	if (!b->ubuf) { perror("malloc"); return -1; }
	if (mbr_partitions(disk, disk_len, b) < 0)
		return -1;

	b->fd = open("/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open /dev/hvx"); return -1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }

	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap RAM"); return -1; }

	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }

	/* Attach a ZEROED page-array disk of disk_len, then LOAD the image zero-copy
	 * through the disk mmap (no whole-image copy_from_user). */
	memset(&dk, 0, sizeof(dk));
	dk.len = disk_len;
	dk.data = 0;				/* zeroed page array; fill via the mmap below */
	dk.flags = HVX_DISK_NO_IDMAP;		/* real Linux builds its own page tables */
	if (ioctl(b->fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return -1; }

	b->disk_mm = mmap(NULL, disk_len, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd,
			  (off_t)HVX_DISK_MMAP_PGOFF * 4096);
	if (b->disk_mm == MAP_FAILED) { perror("mmap disk window"); return -1; }
	memcpy(b->disk_mm, disk, disk_len);	/* load the partitioned image zero-copy */

	if (fault_classes) {
		struct hvx_fault f;
		memset(&f, 0, sizeof(f));
		f.seed = fault_seed;
		f.classes = fault_classes;
		f.probability = fault_prob;
		if (ioctl(b->fd, HVX_SET_FAULT, &f) < 0) { perror("HVX_SET_FAULT"); return -1; }
	}

	build_page_tables_and_gdt(b->mem, mem_size);
	if (load_bzimage(b->mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0)
		return -1;

	fill_sregs(&sregs);
	if (ioctl(b->fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return -1; }

	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(b->fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	return 0;
}

static void boot_teardown(struct boot *b)
{
	if (b->disk_mm != MAP_FAILED && b->disk_mm)
		munmap(b->disk_mm, b->disk_len);
	if (b->mem != MAP_FAILED && b->mem)
		munmap(b->mem, b->mem_size);
	if (b->fd >= 0)
		close(b->fd);
	free(b->ubuf);
	b->ubuf = NULL;
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->disk_mm = MAP_FAILED;
}

static const char *hx(uint64_t v)
{
	static char bufs[8][17];
	static int i;
	char *b = bufs[i++ & 7];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

struct result { uint64_t fp; uint64_t ro_fp; int ok; char verdict[192]; };

/* Boot to FINAL_MARKER and fingerprint the full end state (RAM + rw disk). */
static int run_scenario(uint64_t mem_size, const uint8_t *image, size_t image_len,
			const uint8_t *initrd, size_t initrd_len,
			const uint8_t *disk, uint64_t disk_len, const char *cmdline,
			uint64_t seed, uint32_t classes, uint32_t prob, struct result *r)
{
	struct boot b;
	int rc = -1;

	memset(r, 0, sizeof(*r));
	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, seed, classes, prob) == 0 &&
	    run_until_marker(&b, FINAL_MARKER) == 0) {
		r->fp = fingerprint(&b);
		r->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, r->verdict, sizeof(r->verdict));
		r->ok = (r->fp != 0);
		rc = 0;
	}
	boot_teardown(&b);
	return rc;
}

/* ROUND-TRIP: boot to FINAL, fingerprint (r0); GET_SNAPSHOT + save(RAM, rw); then
 * SET_SNAPSHOT + restore(RAM, rw); re-fingerprint (r1). r0.fp == r1.fp proves the
 * rw-disk-only snapshot is complete (the ro-root is untouched and excluded). */
static int run_roundtrip(uint64_t mem_size, const uint8_t *image, size_t image_len,
			 const uint8_t *initrd, size_t initrd_len,
			 const uint8_t *disk, uint64_t disk_len, const char *cmdline,
			 struct result *r0, struct result *r1)
{
	struct boot b;
	struct hvx_snapshot *snap = calloc(1, sizeof(*snap));
	uint8_t *ram_save = malloc(mem_size);
	uint8_t *rw_save = NULL;
	int rc = -1;

	memset(r0, 0, sizeof(*r0));
	memset(r1, 0, sizeof(*r1));
	if (!snap || !ram_save) { perror("alloc"); goto done; }

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, 0, 0, 0) != 0 ||
	    run_until_marker(&b, FINAL_MARKER) != 0)
		goto teardown;

	rw_save = malloc(b.p2_len);
	if (!rw_save) { perror("alloc rw"); goto teardown; }

	r0->fp = fingerprint(&b);
	r0->ro_fp = ro_root_fp(&b);
	oracle_verdict(&b, r0->verdict, sizeof(r0->verdict));
	r0->ok = (r0->fp != 0);

	if (ioctl(b.fd, HVX_GET_SNAPSHOT, snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	memcpy(ram_save, b.mem, mem_size);
	memcpy(rw_save, b.disk_mm + b.p2_off, b.p2_len);

	/* perturb live RAM/rw to prove the restore truly rewrites them */
	memset(b.mem + 0x400000, 0xa5, 0x1000);
	memset(b.disk_mm + b.p2_off, 0x5a, 64);

	if (ioctl(b.fd, HVX_SET_SNAPSHOT, snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	memcpy(b.mem, ram_save, mem_size);
	memcpy(b.disk_mm + b.p2_off, rw_save, b.p2_len);

	r1->fp = fingerprint(&b);
	r1->ro_fp = ro_root_fp(&b);
	r1->ok = (r1->fp != 0);
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(snap); free(ram_save); free(rw_save);
	return rc;
}

/* BRANCH: boot to MID_MARKER (before the rw workload), snapshot RAM + rw disk (NOT
 * the ro-root), continue to FINAL -> ra; restore snapshot + RAM + rw, continue -> rb.
 * ra.fp == rb.fp proves the branch reproduces the whole rw workload from the RAM+rw
 * snapshot with the ro-root excluded (re-attached identically, untouched). */
static int run_branch(uint64_t mem_size, const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const uint8_t *disk, uint64_t disk_len, const char *cmdline,
		      uint64_t seed, uint32_t classes, uint32_t prob,
		      struct result *ra, struct result *rb)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
	uint8_t *ram_save = malloc(mem_size);
	uint8_t *rw_save = NULL;
	size_t branch_ulen = 0;
	int rc = -1;

	memset(ra, 0, sizeof(*ra));
	memset(rb, 0, sizeof(*rb));
	if (!mid || !ram_save) { perror("alloc"); goto done; }

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, seed, classes, prob) != 0 ||
	    run_until_marker(&b, MID_MARKER) != 0)
		goto teardown;

	rw_save = malloc(b.p2_len);
	if (!rw_save) { perror("alloc rw"); goto teardown; }

	fprintf(stderr, "hvx_rootfs_verify: reached mid marker at %zu bytes; snapshotting (RAM + rw disk only)\n", b.ulen);
	branch_ulen = b.ulen;
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	memcpy(ram_save, b.mem, mem_size);
	memcpy(rw_save, b.disk_mm + b.p2_off, b.p2_len);

	/* straight continuation -> ra */
	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		ra->fp = fingerprint(&b);
		ra->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
		ra->ok = (ra->fp != 0);
	}

	/* restore snapshot + RAM + rw disk (ro-root left untouched), rewind console */
	if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	memcpy(b.mem, ram_save, mem_size);
	memcpy(b.disk_mm + b.p2_off, rw_save, b.p2_len);
	b.ulen = branch_ulen;
	console_rebase(&b);

	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		rb->fp = fingerprint(&b);
		rb->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, rb->verdict, sizeof(rb->verdict));
		rb->ok = (rb->fp != 0);
	}
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(mid); free(ram_save); free(rw_save);
	return rc;
}

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *disk_path, *cmdline;
	uint64_t mem_size, disk_len = 0;
	uint8_t *image = NULL, *initrd = NULL, *disk = NULL;
	size_t image_len = 0, initrd_len = 0, dl = 0;
	uint64_t offline_p1_fp;
	struct boot geom;
	int rc = 1;
	struct result nf1, nf2, rt0, rt1, nfa, nfb;	/* no-fault: replay/round-trip/branch */
	struct result fa1, fa2, fb1;			/* seeded DROP: seed-A x2, seed-B */
	int ok_verdict, ok_replay, ok_roundtrip, ok_branch, ok_seed, ok_roroot;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-rootfs> <rootfs-disk.img> [\"cmdline\"] [memMiB]\n",
			argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	disk_path   = argv[3];
	cmdline     = (argc >= 5 && argv[4][0]) ? argv[4] : ROOTFS_CMDLINE;
	mem_size    = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0)
				   : ROOTFS_MEM_MIB) * 1024ULL * 1024ULL;

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	disk = read_file(disk_path, &dl);
	if (!disk) { free(image); free(initrd); return 1; }
	disk_len = dl;

	/* derive the partition ranges once + the offline ro-root hash */
	memset(&geom, 0, sizeof(geom));
	geom.disk_len = disk_len;
	if (mbr_partitions(disk, disk_len, &geom) < 0) { free(image); free(initrd); free(disk); return 1; }
	offline_p1_fp = fnv(0xcbf29ce484222325ULL, disk + geom.p1_off, geom.p1_len);

	fprintf(stderr, "hvx_rootfs_verify: bzImage=%zuB initramfs=%zuB rootfs-disk=%zuB mem=%llu MiB\n",
		image_len, initrd_len, dl, (unsigned long long)(mem_size / (1024 * 1024)));
	fprintf(stderr, "hvx_rootfs_verify: partitions: p1(ro root) off=%#llx len=%llu MiB ; "
		"p2(rw data) off=%#llx len=%llu MiB ; ro-root hash=%s\n",
		(unsigned long long)geom.p1_off, (unsigned long long)(geom.p1_len >> 20),
		(unsigned long long)geom.p2_off, (unsigned long long)(geom.p2_len >> 20),
		hx(offline_p1_fp));

	/* ===================== (1) REPLAY (two fresh boots, no fault) =========== */
	fprintf(stderr, "\n=== (1) REPLAY -- boot #1 (establish the OK baseline) ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nf1);
	fprintf(stderr, "\n=== (1) REPLAY -- boot #2 ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nf2);

	/* ===================== (2) ROUND-TRIP =================================== */
	fprintf(stderr, "\n=== (2) ROUND-TRIP -- snapshot then restore in place (RAM + rw disk only) ===\n");
	run_roundtrip(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, &rt0, &rt1);

	/* ===================== (3) BRANCH-FROM-SNAPSHOT ========================= */
	fprintf(stderr, "\n=== (3) BRANCH-FROM-SNAPSHOT (snapshot at mid marker, continue twice) ===\n");
	run_branch(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nfa, &nfb);

	/* ===================== (4) SEED-SENSITIVITY (DROP on rw disk) =========== */
	fprintf(stderr, "\n=== (4) SEED-SENSITIVITY -- seed=0x%llx DROP prob=%u%% -- boot #1 (failing) ===\n",
		(unsigned long long)SEED_A, FAULT_PROB);
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_A, HVX_FAULT_DROP, FAULT_PROB, &fa1);
	fprintf(stderr, "\n=== (4) SEED-SENSITIVITY -- seed=0x%llx -- boot #2 (replay bit-for-bit) ===\n",
		(unsigned long long)SEED_A);
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_A, HVX_FAULT_DROP, FAULT_PROB, &fa2);
	fprintf(stderr, "\n=== (4) SEED-SENSITIVITY -- seed=0x%llx (a different reproducible failure) ===\n",
		(unsigned long long)SEED_B);
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_B, HVX_FAULT_DROP, FAULT_PROB, &fb1);

	/* ===================== verdicts ======================================== */
	ok_verdict    = nf1.ok && verdict_is_ok(nf1.verdict);
	ok_replay     = nf1.ok && nf2.ok && nf1.fp == nf2.fp;
	ok_roundtrip  = rt0.ok && rt1.ok && rt0.fp == rt1.fp && rt0.fp == nf1.fp;
	ok_branch     = nfa.ok && nfb.ok && nfa.fp == nfb.fp && nfa.fp == nf1.fp;
	ok_seed       = fa1.ok && fa2.ok && fb1.ok &&
			verdict_is_violation(fa1.verdict) && verdict_is_violation(fb1.verdict) &&
			fa1.fp == fa2.fp && fa1.fp != fb1.fp;
	ok_roroot     = nf1.ok && nf2.ok &&
			nf1.ro_fp == offline_p1_fp && nf2.ro_fp == offline_p1_fp &&
			nfa.ro_fp == offline_p1_fp && rt0.ro_fp == offline_p1_fp &&
			fa1.ro_fp == offline_p1_fp && fb1.ro_fp == offline_p1_fp;

	printf("\n");
	printf("ROOTFS verdict     |%s|\n", nf1.verdict);
	printf("(1) REPLAY         fp1=%s fp2=%s -> %s\n",
	       hx(nf1.fp), hx(nf2.fp), ok_replay ? "IDENTICAL" : "MISMATCH");
	printf("(2) ROUND-TRIP     fp0=%s fp1=%s -> %s\n",
	       hx(rt0.fp), hx(rt1.fp), ok_roundtrip ? "IDENTICAL" : "MISMATCH");
	printf("(3) BRANCH         fp_a=%s fp_b=%s -> %s\n",
	       hx(nfa.fp), hx(nfb.fp), ok_branch ? "IDENTICAL" : "MISMATCH");
	printf("(4) SEED-SENSITIVE seedA=%s (x2 %s) seedB=%s -> %s  |A:%s| |B:%s|\n",
	       hx(fa1.fp), (fa1.fp == fa2.fp) ? "repro" : "FLAKY", hx(fb1.fp),
	       ok_seed ? "DIFFERENT+REPRODUCIBLE+VIOLATION" : "UNEXPECTED",
	       fa1.verdict, fb1.verdict);
	printf("ro-root static     offline=%s live(all boots)=%s -> %s\n",
	       hx(offline_p1_fp),
	       (nf1.ro_fp == offline_p1_fp) ? hx(nf1.ro_fp) : "CHANGED",
	       ok_roroot ? "STABLE (excluded from snapshot, re-attached identically)" : "DRIFTED");

	if (ok_verdict && ok_replay && ok_roundtrip && ok_branch && ok_seed && ok_roroot) {
		printf("hvx_rootfs_verify: MILESTONE 29 PASS: real Linux boots a byte-reproducible ext4 "
		       "READ-ONLY root via switch_root + a small ext2 rw data disk; determinism holds AT "
		       "SCALE with the snapshot capturing ONLY guest RAM + the small rw disk (ro-root "
		       "EXCLUDED + hash-stable): replay/round-trip/branch all fingerprint %s, and a seeded "
		       "DROP fault on the rw disk loses a committed record differently per seed "
		       "(A=%s B=%s), reproducibly.\n",
		       hx(nf1.fp), hx(fa1.fp), hx(fb1.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_rootfs_verify: FAIL: verdict=%d replay=%d roundtrip=%d branch=%d "
			"seed=%d ro-root=%d\n",
			ok_verdict, ok_replay, ok_roundtrip, ok_branch, ok_seed, ok_roroot);
	}

	free(image); free(initrd); free(disk);
	return rc;
}

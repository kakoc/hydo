// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_jvm_verify - milestone 31 (Rung 5): run a REAL, unmodified OpenJDK JRE and a
 * real multi-threaded Java workload DETERMINISTICALLY under hvx, on the M29 ext4
 * READ-ONLY root (the big JRE closure lives on the ro-root, which is EXCLUDED from
 * the snapshot, so a hundreds-of-MiB JRE does not bloat the fingerprint), with a
 * small ext2 rw data disk as the durability-fault target.
 * =============================================================================
 *
 * The JVM is the single hardest nondeterminism surface in the whole stack. This
 * harness proves hvx tames it: two boots from scratch reach a BIT-IDENTICAL full
 * state (guest RAM + rw disk + console), and branch-from-snapshot reproduces it.
 * The Java workload (hvx/HydoWork.java) deliberately exercises EVERY JVM
 * nondeterminism source -- heap allocation + SerialGC, many threads on the single
 * deterministic vCPU contending on an ORDER-DEPENDENT lock, System.nanoTime /
 * currentTimeMillis (-> the deterministic virtual TSC), SecureRandom (default:
 * reads /dev/urandom, seeded by the M27 CRNG credit), java.util.Random, and
 * System.identityHashCode -- and PRINTS every observed value to the console (which
 * is part of the fingerprint). So if ANY source is not deterministic under hvx, the
 * two-boot fingerprint diverges and this harness catches it (honest wall-reporting).
 *
 * The backing image (build/jvm-disk.img) is ONE MBR-partitioned virtio-blk disk:
 *   p1 @ LBA 2048   = ext4 ro ROOT (busybox + hvx_seedrng + /app + the FULL JRE
 *                     closure under /nix/store), the big static image
 *   p2              = ext2 rw DATA disk (the JVM writes 32 committed key records here;
 *                     the M25/M26/M29 durability-fault target)
 * The tiny initramfs (build/initramfs-jvm.cpio.gz) loads the virtio-blk + ext4 + ext2
 * module stack, mounts /dev/vda1 READ-ONLY, and `switch_root`s into it. The real
 * root's /sbin/init seeds the CRNG (M27), mounts /dev/vda2 rw on /data, runs a REAL
 * JVM (interpreter-only -Xint: no JIT/compiler threads; single-threaded SerialGC;
 * fixed heap -Xms==-Xmx; perfdata off), then re-reads every committed record from the
 * DEVICE (page cache dropped) and prints:
 *     HYDO-ORACLE: OK keys=<K> durable
 *     HYDO-ORACLE: VIOLATION key=<k> expected=<v> got=<g>
 * then the stop marker  hydo-jvm-marker: workload complete.
 *
 * Snapshot model + fingerprint tractability + the four checks are IDENTICAL to the
 * M29 harness (only RAM + the small rw disk are snapshotted; the big ro-root is
 * EXCLUDED + hash-asserted-stable; RAM/rw are hashed zero-copy over the mmaps):
 *   (1) REPLAY:      two fresh JVM boots are bit-identical.
 *   (2) ROUND-TRIP:  GET_SNAPSHOT + save(RAM, rw) then SET_SNAPSHOT + restore == same.
 *   (3) BRANCH:      snapshot at a pre-initramfs kernel marker, continue twice ->
 *                    identical; the WHOLE JVM boot + run is reproduced from the
 *                    RAM+rw snapshot with the ro-root excluded.
 *   (4) SEED-SENSITIVITY: a seeded virtio-blk DROP on the rw-disk writes loses a
 *                    committed record (oracle VIOLATION); seed A and seed B lose
 *                    different records, each reproducible.
 *
 * A trailing arg selects which checks to run (default "all"): replay | roundtrip |
 * branch | seed | all -- so bring-up can prove determinism cheaply (a JVM boot is
 * slow) before the full run. Fault seeds/prob are env-overridable
 * (HVX_JVM_SEED_A / _SEED_B / _PROB) for tuning the DROP onto a key write.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_jvm_verify hvx_jvm_verify.c
 * Run   (inside L1):  sudo ./hvx_jvm_verify /tmp/bzImage /tmp/initramfs-jvm.cpio.gz /tmp/jvm-disk.img "" 640 all
 */

/* M32 Part 2: posix_fadvise/fdatasync live behind these feature-test macros; define
 * them BEFORE hvx_boot.h pulls in <fcntl.h>/<unistd.h> so the declarations appear. */
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE
#endif
#ifndef _POSIX_C_SOURCE
#define _POSIX_C_SOURCE 200112L
#endif
#include "hvx_boot.h"
#include <stdlib.h>

/* Printed by the real root's /sbin/init right after the oracle -- the stop point. */
#define FINAL_MARKER "hydo-jvm-marker: workload complete"
/* A clean, deep-in-kernel-init marker BEFORE the initramfs even unpacks -- so no
 * virtio-blk / disk I/O is in flight at the snapshot boundary (the same MID point the
 * SQLite/disk/rootfs harnesses use). Branching here forces the snapshot to reproduce
 * the WHOLE rest: the module loads, the ext4 ro-root mount, switch_root, the JVM boot,
 * AND the ext2 rw workload -- a strong branch test whose endpoint equals a fresh boot. */
#define MID_MARKER   "Trying to unpack rootfs image as initramfs"

#define ROOTFS_CMDLINE \
	"root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

#define ROOTFS_MEM_MIB 640UL            /* room for a -Xmx128m JVM; fits L1's 4 GiB */
#define BOOT_TSC_STEP  0x10000ULL
#define UBUF_CAP       (1UL << 20)
#define MAX_RUN_ITERS  4000000000ULL    /* JVM free-run boots take many throttle windows */

/* Seeded disk-fault policy for the seed-sensitivity check. DROP is WRITE-only, and
 * the root is mounted RO (no root writes), so DROP faults hit ONLY the rw-data
 * writes; a moderate probability loses a committed record with the fs still
 * readable, and the exact lost record is a pure function of the seed. Overridable via
 * env so the DROP can be tuned onto a key-file write for this workload. */
#define DEF_FAULT_PROB  20u
#define DEF_SEED_A      0x31A0ULL
#define DEF_SEED_B      0x31A1ULL
static uint32_t FAULT_PROB = DEF_FAULT_PROB;
static uint64_t SEED_A = DEF_SEED_A;
static uint64_t SEED_B = DEF_SEED_B;

/* ---- canonical fingerprint (FNV-1a; matches the other hvx verify harnesses) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}

/* ---- file-backed snapshot save buffer (M32 Part 2) ------------------------------
 * The verify harness must double-buffer the snapshot (guest RAM + the small rw disk)
 * to prove round-trip/branch. Buffering guest RAM in a malloc() presses L1's fixed
 * ~3.4 GiB (a 640 MiB JVM RAM save; ~1 GiB for a Kafka-scale guest). Back the save
 * with a FILE on a real DISK-backed path in L1 instead (default /var/tmp, which is
 * ext4 on /dev/vda1 here -- NOT a tmpfs, verified with `df -T`) so the bytes live on
 * disk / in evictable page cache, not anonymous process RAM. The FINGERPRINT IS
 * UNCHANGED -- this only changes WHERE the save bytes live, so the JVM baseline
 * 4b8ffacde50f1169 is preserved. Env HVX_SNAP_DIR overrides the path;
 * HVX_SNAP_MALLOC=1 restores the old in-RAM buffer (for an A/B RAM comparison). */
struct savebuf {
	int fd;
	int use_file;
	uint8_t *mem;		/* malloc fallback (HVX_SNAP_MALLOC=1) */
	uint64_t size;
	char path[288];
};

static const char *snap_dir(void)
{
	const char *d = getenv("HVX_SNAP_DIR");
	return (d && d[0]) ? d : "/var/tmp/hvx-snap";
}

static int savebuf_open(struct savebuf *sb, uint64_t size, const char *tag)
{
	static unsigned seq;

	memset(sb, 0, sizeof(*sb));
	sb->fd = -1;
	sb->size = size;
	if (getenv("HVX_SNAP_MALLOC")) {
		sb->use_file = 0;
		sb->mem = malloc(size);
		return sb->mem ? 0 : -1;
	}
	sb->use_file = 1;
	mkdir(snap_dir(), 0700);		/* ignore EEXIST */
	snprintf(sb->path, sizeof(sb->path), "%.240s/save-%d-%.16s-%u.bin",
		 snap_dir(), (int)getpid(), tag, seq++);
	sb->fd = open(sb->path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (sb->fd < 0) { perror("savebuf open (HVX_SNAP_DIR on a real disk?)"); return -1; }
	if (ftruncate(sb->fd, (off_t)size) < 0) { perror("ftruncate"); return -1; }
	unlink(sb->path);			/* keep the open fd; auto-cleanup on close/exit */
	return 0;
}

static int savebuf_store(struct savebuf *sb, const void *src, uint64_t size)
{
	uint64_t off = 0;
	const uint8_t *p = src;

	if (!sb->use_file) { memcpy(sb->mem, src, size); return 0; }
	while (off < size) {
		ssize_t w = pwrite(sb->fd, p + off, size - off, (off_t)off);
		if (w < 0) { if (errno == EINTR) continue; perror("pwrite save"); return -1; }
		off += (uint64_t)w;
	}
	/* flush to the backing disk, then drop the page cache so the save is not RAM-resident */
	fdatasync(sb->fd);
	posix_fadvise(sb->fd, 0, (off_t)size, POSIX_FADV_DONTNEED);
	return 0;
}

static int savebuf_load(struct savebuf *sb, void *dst, uint64_t size)
{
	uint64_t off = 0;
	uint8_t *p = dst;

	if (!sb->use_file) { memcpy(dst, sb->mem, size); return 0; }
	while (off < size) {
		ssize_t r = pread(sb->fd, p + off, size - off, (off_t)off);
		if (r < 0) { if (errno == EINTR) continue; perror("pread restore"); return -1; }
		if (r == 0) break;
		off += (uint64_t)r;
	}
	return 0;
}

static void savebuf_close(struct savebuf *sb)
{
	if (sb->use_file) { if (sb->fd >= 0) close(sb->fd); }
	else free(sb->mem);
	sb->fd = -1;
	sb->mem = NULL;
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

/* M32 Part 1: capture the workload's observed-values line (HYDO-JVM: threads=... sum=...
 * state=... secrand=... ...). Under a JIT config, if the thread interleaving or any
 * nondeterminism source diverges across two boots, the differing field is visible here
 * -- so a replay MISMATCH can be localized (e.g. state=<lock interleave> vs a compiled-
 * value field) instead of only knowing the whole fingerprint differs. */
static void jvm_line(const struct boot *b, char *out, size_t outsz)
{
	long at = mem_find(b->ubuf, b->ulen, "HYDO-JVM: threads=");
	size_t i = 0;

	out[0] = '\0';
	if (at < 0)
		return;
	while ((size_t)at + i < b->ulen && b->ubuf[at + i] != '\n' && i + 1 < outsz) {
		out[i] = (char)b->ubuf[at + i];
		i++;
	}
	out[i] = '\0';
}

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

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
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
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
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
	const char *ev;

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
	/* M31: advertise x86-64-v2 CPUID so the real glibc JRE loads, and enable the M27
	 * idle heartbeat so a tickless JVM idle (parked threads / safepoints) cannot stall
	 * the run. Both opt-in flags -> prior fingerprints unaffected. */
	cfg.flags = HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT;
	/* Timing-perturbation experiment knobs (all unset by default -> unchanged runs):
	 * HVX_TSC_STEP overrides the virtual-TSC step per exit; HVX_PMU_CLOCK=1 turns on
	 * the PMU branch clock, with HVX_PMU_PERIOD / HVX_PMU_TICK_TSC setting its tick. */
	if ((ev = getenv("HVX_TSC_STEP")))
		tsc.step = strtoull(ev, NULL, 0);
	if ((ev = getenv("HVX_PMU_CLOCK")) && !strcmp(ev, "1"))
		cfg.flags |= HVX_VM_PMU_CLOCK;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }
	if (cfg.flags & HVX_VM_PMU_CLOCK) {
		struct hvx_pmu pm;
		memset(&pm, 0, sizeof(pm));
		if ((ev = getenv("HVX_PMU_PERIOD")))   pm.period = strtoull(ev, NULL, 0);
		if ((ev = getenv("HVX_PMU_TICK_TSC"))) pm.tick_tsc = strtoull(ev, NULL, 0);
		if ((pm.period || pm.tick_tsc) && ioctl(b->fd, HVX_SET_PMU, &pm) < 0) {
			perror("HVX_SET_PMU");
			return -1;
		}
	}

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

struct result { uint64_t fp; uint64_t ro_fp; int ok; char verdict[192]; char jvmline[192]; };

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
		jvm_line(&b, r->jvmline, sizeof(r->jvmline));
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
	struct savebuf sb_ram, sb_rw;
	int have_ram = 0, have_rw = 0;
	int rc = -1;

	memset(r0, 0, sizeof(*r0));
	memset(r1, 0, sizeof(*r1));
	if (!snap) { perror("alloc"); goto done; }
	if (savebuf_open(&sb_ram, mem_size, "rt-ram") != 0) goto done;
	have_ram = 1;

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, 0, 0, 0) != 0 ||
	    run_until_marker(&b, FINAL_MARKER) != 0)
		goto teardown;

	if (savebuf_open(&sb_rw, b.p2_len, "rt-rw") != 0) goto teardown;
	have_rw = 1;

	r0->fp = fingerprint(&b);
	r0->ro_fp = ro_root_fp(&b);
	oracle_verdict(&b, r0->verdict, sizeof(r0->verdict));
	r0->ok = (r0->fp != 0);

	if (ioctl(b.fd, HVX_GET_SNAPSHOT, snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	savebuf_store(&sb_ram, b.mem, mem_size);
	savebuf_store(&sb_rw, b.disk_mm + b.p2_off, b.p2_len);

	/* perturb live RAM/rw to prove the restore truly rewrites them */
	memset(b.mem + 0x400000, 0xa5, 0x1000);
	memset(b.disk_mm + b.p2_off, 0x5a, 64);

	if (ioctl(b.fd, HVX_SET_SNAPSHOT, snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	savebuf_load(&sb_ram, b.mem, mem_size);
	savebuf_load(&sb_rw, b.disk_mm + b.p2_off, b.p2_len);

	r1->fp = fingerprint(&b);
	r1->ro_fp = ro_root_fp(&b);
	r1->ok = (r1->fp != 0);
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(snap);
	if (have_ram) savebuf_close(&sb_ram);
	if (have_rw) savebuf_close(&sb_rw);
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
	struct savebuf sb_ram, sb_rw;
	int have_ram = 0, have_rw = 0;
	size_t branch_ulen = 0;
	int rc = -1;

	memset(ra, 0, sizeof(*ra));
	memset(rb, 0, sizeof(*rb));
	if (!mid) { perror("alloc"); goto done; }
	if (savebuf_open(&sb_ram, mem_size, "br-ram") != 0) goto done;
	have_ram = 1;

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, seed, classes, prob) != 0 ||
	    run_until_marker(&b, MID_MARKER) != 0)
		goto teardown;

	if (savebuf_open(&sb_rw, b.p2_len, "br-rw") != 0) goto teardown;
	have_rw = 1;

	fprintf(stderr, "hvx_rootfs_verify: reached mid marker at %zu bytes; snapshotting (RAM + rw disk only)\n", b.ulen);
	branch_ulen = b.ulen;
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	savebuf_store(&sb_ram, b.mem, mem_size);
	savebuf_store(&sb_rw, b.disk_mm + b.p2_off, b.p2_len);

	/* straight continuation -> ra */
	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		ra->fp = fingerprint(&b);
		ra->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
		jvm_line(&b, ra->jvmline, sizeof(ra->jvmline));
		ra->ok = (ra->fp != 0);
	}

	/* restore snapshot + RAM + rw disk (ro-root left untouched), rewind console */
	if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	savebuf_load(&sb_ram, b.mem, mem_size);
	savebuf_load(&sb_rw, b.disk_mm + b.p2_off, b.p2_len);
	b.ulen = branch_ulen;
	console_rebase(&b);

	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		rb->fp = fingerprint(&b);
		rb->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, rb->verdict, sizeof(rb->verdict));
		jvm_line(&b, rb->jvmline, sizeof(rb->jvmline));
		rb->ok = (rb->fp != 0);
	}
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(mid);
	if (have_ram) savebuf_close(&sb_ram);
	if (have_rw) savebuf_close(&sb_rw);
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
	int do_replay = 1, do_roundtrip = 1, do_branch = 1, do_seed = 1;
	const char *mode = "all", *e;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-jvm> <jvm-disk.img> [\"cmdline\"] [memMiB] "
			"[all|replay|roundtrip|branch|seed]\n", argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	disk_path   = argv[3];
	cmdline     = (argc >= 5 && argv[4][0]) ? argv[4] : ROOTFS_CMDLINE;
	mem_size    = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0)
				   : ROOTFS_MEM_MIB) * 1024ULL * 1024ULL;
	if (argc >= 7 && argv[6][0])
		mode = argv[6];

	/* which checks to run (a JVM boot is slow -> bring-up can prove determinism cheaply) */
	if (strcmp(mode, "all") != 0) {
		do_replay = strcmp(mode, "replay") == 0;
		do_roundtrip = strcmp(mode, "roundtrip") == 0;
		do_branch = strcmp(mode, "branch") == 0;
		do_seed = strcmp(mode, "seed") == 0;
	}
	/* env-overridable fault seeds/prob (tune the DROP onto a key-file write) */
	if ((e = getenv("HVX_JVM_SEED_A"))) SEED_A = strtoull(e, NULL, 0);
	if ((e = getenv("HVX_JVM_SEED_B"))) SEED_B = strtoull(e, NULL, 0);
	if ((e = getenv("HVX_JVM_PROB")))   FAULT_PROB = (uint32_t)strtoul(e, NULL, 0);
	fprintf(stderr, "hvx_jvm_verify: mode=%s seedA=0x%llx seedB=0x%llx prob=%u%%\n",
		mode, (unsigned long long)SEED_A, (unsigned long long)SEED_B, FAULT_PROB);
	fprintf(stderr, "hvx_jvm_verify: snapshot save = %s\n",
		getenv("HVX_SNAP_MALLOC") ? "in-RAM malloc (HVX_SNAP_MALLOC)"
					 : snap_dir());

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

	fprintf(stderr, "hvx_jvm_verify: bzImage=%zuB initramfs=%zuB jvm-disk=%zuB mem=%llu MiB\n",
		image_len, initrd_len, dl, (unsigned long long)(mem_size / (1024 * 1024)));
	fprintf(stderr, "hvx_jvm_verify: partitions: p1(ro root) off=%#llx len=%llu MiB ; "
		"p2(rw data) off=%#llx len=%llu MiB ; ro-root hash=%s\n",
		(unsigned long long)geom.p1_off, (unsigned long long)(geom.p1_len >> 20),
		(unsigned long long)geom.p2_off, (unsigned long long)(geom.p2_len >> 20),
		hx(offline_p1_fp));

	memset(&nf1, 0, sizeof(nf1)); memset(&nf2, 0, sizeof(nf2));
	memset(&rt0, 0, sizeof(rt0)); memset(&rt1, 0, sizeof(rt1));
	memset(&nfa, 0, sizeof(nfa)); memset(&nfb, 0, sizeof(nfb));
	memset(&fa1, 0, sizeof(fa1)); memset(&fa2, 0, sizeof(fa2)); memset(&fb1, 0, sizeof(fb1));

	/* Always run one clean boot -> nf1: the OK-verdict baseline AND the reference
	 * fingerprint that round-trip/branch compare against. */
	fprintf(stderr, "\n=== BASELINE / REPLAY -- JVM boot #1 (establish the OK baseline) ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nf1);

	if (do_replay) {
		fprintf(stderr, "\n=== (1) REPLAY -- JVM boot #2 ===\n");
		run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nf2);
	}
	if (do_roundtrip) {
		fprintf(stderr, "\n=== (2) ROUND-TRIP -- snapshot then restore in place (RAM + rw disk only) ===\n");
		run_roundtrip(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, &rt0, &rt1);
	}
	if (do_branch) {
		fprintf(stderr, "\n=== (3) BRANCH-FROM-SNAPSHOT (snapshot at a pre-initramfs marker, continue twice) ===\n");
		run_branch(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline, 0, 0, 0, &nfa, &nfb);
	}
	if (do_seed) {
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
	}

	/* ===================== verdicts (only the checks that ran are required) ==== */
	ok_verdict    = nf1.ok && verdict_is_ok(nf1.verdict);
	ok_replay     = !do_replay    || (nf1.ok && nf2.ok && nf1.fp == nf2.fp);
	ok_roundtrip  = !do_roundtrip || (rt0.ok && rt1.ok && rt0.fp == rt1.fp && rt0.fp == nf1.fp);
	ok_branch     = !do_branch    || (nfa.ok && nfb.ok && nfa.fp == nfb.fp && nfa.fp == nf1.fp);
	ok_seed       = !do_seed      || (fa1.ok && fa2.ok && fb1.ok &&
				verdict_is_violation(fa1.verdict) && verdict_is_violation(fb1.verdict) &&
				fa1.fp == fa2.fp && fa1.fp != fb1.fp);
	ok_roroot     = nf1.ok && nf1.ro_fp == offline_p1_fp &&
			(!do_replay    || nf2.ro_fp == offline_p1_fp) &&
			(!do_branch    || nfa.ro_fp == offline_p1_fp) &&
			(!do_roundtrip || rt0.ro_fp == offline_p1_fp) &&
			(!do_seed      || (fa1.ro_fp == offline_p1_fp && fb1.ro_fp == offline_p1_fp));

	printf("\n");
	printf("JVM oracle verdict |%s|\n", nf1.verdict);
	printf("BASELINE           fp=%s (OK=%d)\n", hx(nf1.fp), ok_verdict);
	if (do_replay) {
		printf("(1) REPLAY         fp1=%s fp2=%s -> %s\n",
		       hx(nf1.fp), hx(nf2.fp), ok_replay ? "IDENTICAL" : "MISMATCH");
		/* Part-1 JIT-determinism diagnostics: the workload's observed values. If a
		 * JIT config is nondeterministic, the differing field localizes the source. */
		printf("    boot1 |%s|\n", nf1.jvmline);
		printf("    boot2 |%s|\n", nf2.jvmline);
	}
	if (do_roundtrip)
		printf("(2) ROUND-TRIP     fp0=%s fp1=%s -> %s\n",
		       hx(rt0.fp), hx(rt1.fp), ok_roundtrip ? "IDENTICAL" : "MISMATCH");
	if (do_branch) {
		printf("(3) BRANCH         fp_a=%s fp_b=%s -> %s\n",
		       hx(nfa.fp), hx(nfb.fp), ok_branch ? "IDENTICAL" : "MISMATCH");
		if (!ok_branch) {
			printf("    straight |%s|\n", nfa.jvmline);
			printf("    restored |%s|\n", nfb.jvmline);
		}
	}
	if (do_seed)
		printf("(4) SEED-SENSITIVE seedA=%s (x2 %s) seedB=%s -> %s  |A:%s| |B:%s|\n",
		       hx(fa1.fp), (fa1.fp == fa2.fp) ? "repro" : "FLAKY", hx(fb1.fp),
		       ok_seed ? "DIFFERENT+REPRODUCIBLE+VIOLATION" : "UNEXPECTED",
		       fa1.verdict, fb1.verdict);
	printf("ro-root static     offline=%s live=%s -> %s\n",
	       hx(offline_p1_fp),
	       (nf1.ro_fp == offline_p1_fp) ? hx(nf1.ro_fp) : "CHANGED",
	       ok_roroot ? "STABLE (excluded from snapshot, re-attached identically)" : "DRIFTED");

	if (ok_verdict && ok_replay && ok_roundtrip && ok_branch && ok_seed && ok_roroot) {
		if (strcmp(mode, "all") == 0)
			printf("hvx_jvm_verify: MILESTONE 31 PASS: a REAL, unmodified OpenJDK JRE runs a real "
			       "multi-threaded Java workload DETERMINISTICALLY under hvx -- replay/round-trip/branch "
			       "all fingerprint %s (guest RAM + rw disk + console, incl. the JVM's observed "
			       "nanoTime/SecureRandom/identityHashCode/thread-interleaving), the big JRE ro-root "
			       "EXCLUDED + hash-stable, and a seeded DROP fault on the rw disk loses a committed "
			       "record differently per seed (A=%s B=%s), reproducibly.\n",
			       hx(nf1.fp), hx(fa1.fp), hx(fb1.fp));
		else
			printf("hvx_jvm_verify: mode=%s PASS (baseline fp=%s)\n", mode, hx(nf1.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_jvm_verify: FAIL: verdict=%d replay=%d roundtrip=%d branch=%d "
			"seed=%d ro-root=%d\n",
			ok_verdict, ok_replay, ok_roundtrip, ok_branch, ok_seed, ok_roroot);
	}

	free(image); free(initrd); free(disk);
	return rc;
}

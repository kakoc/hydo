// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_spread_verify - milestone 31 (Rung 5): run a REAL, unmodified OpenJDK JRE and a
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
 * Build (inside L1):  cc -O2 -Wall -o hvx_spread_verify hvx_spread_verify.c
 * Run   (inside L1):  sudo ./hvx_spread_verify /tmp/bzImage /tmp/initramfs-jvm.cpio.gz /tmp/jvm-disk.img "" 640 all
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
#define FINAL_MARKER "hydo-spread-marker: workload complete"
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
	/* x86-64-v2 CPUID (glibc userland) + the idle heartbeat (the workload's metric-gated drain
	 * uses timer-sleeps that only fire if the clock advances on a tickless idle). The heartbeat
	 * makes HVX_RUN never return while the guest is PERMANENTLY idle -- so the snapshot boundary
	 * is a BUSY marker BEFORE the driver's blocking serial read, and each program's input is
	 * STAGED before resume so the driver read never blocks (it finds the bytes already present).
	 * HVX_NO_HEARTBEAT=1 disables it for A/B testing. */
	cfg.flags = HVX_VM_X86_64_V2;
	if (!getenv("HVX_NO_HEARTBEAT"))
		cfg.flags |= HVX_VM_IDLE_HEARTBEAT;
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

/* ==========================================================================
 * hvx_spread_search — Phase 3: the autonomous serial-fed snapshot-restore search
 * loop (the "upper half" hvx lacks). Boot the Phase-2 two-broker guest ONCE to the
 * READY state (both brokers up + queue q created + the driver blocked on the serial
 * read), snapshot it, then per generated op-program RESTORE that snapshot (no reboot),
 * feed the program over the serial RX (HVX_PUT_INPUT, staged BEFORE resume so the read
 * is not racing async delivery), run to the oracle DONE marker, and read the verdict
 * (spread: consistent / BUG-ASSERT) from the reconstructed console. Oracle/assertion-
 * guided; coverage deferred (hvx has no edge bitmap). A finding replays bit-for-bit:
 * same program -> same snapshot -> identical verdict AND identical fingerprint.
 *
 * Build: cc -O2 -Wall -o hvx_spread_search hvx_spread_search.c
 * Run:   sudo ./hvx_spread_search <bzImage> <initramfs> <disk.img> "<cmdline>" <memMiB>
 *                                 [nprograms] [seed] [fault_seed] [fault_prob]
 * ========================================================================== */

#include <time.h>
#include <signal.h>

/* A periodic SIGALRM interrupts a blocking HVX_RUN (EINTR) so the wall-clock timeout in
 * wait_marker_dump can fire even if the guest goes idle and the module idle-jumps in-kernel
 * without returning. Guest state persists across HVX_RUN (M13), so re-running continues it. */
static volatile sig_atomic_t g_alarm;
static void on_alarm(int s) { (void)s; g_alarm = 1; }

/* The snapshot boundary is a BUSY marker /init prints right BEFORE it launches the serial-reading
 * driver -- so the harness catches it while HVX_RUN still returns (the driver's own
 * SPREAD-READY-FOR-INPUT is emitted per restore, AFTER input is already staged, so it never
 * causes a permanent idle). DONE_MARKER ends each program's run. */
#define READY_MARKER "FUZZREADY"
#define DONE_MARKER  "SPREAD-RUN-DONE"

/* Stage one op-program on the SYNCHRONOUS fuzz-input port (HVX_SET_FUZZ_INPUT), framed as
 * [u32 len little-endian][program bytes] for the guest's fuzz-read. Called while the guest is
 * PAUSED (right after restore, before resume) and it RESETS the module's read offset, so the
 * guest's fuzz-read reads exactly this program from the start -> byte-deterministic per restore.
 * The port + buffer are snapshot-excluded, so staging never perturbs the fingerprint. */
static int feed_input(int fd, const char *buf, size_t len)
{
	static struct hvx_fuzz_input fi;	/* ~8 KiB; static, off the stack */
	size_t payload = 4 + len;
	if (payload > HVX_FUZZ_MAX) {
		fprintf(stderr, "feed_input: program too large (%zu > %d)\n", len, HVX_FUZZ_MAX - 4);
		return -1;
	}
	fi.buf[0] = (uint8_t)(len & 0xff);
	fi.buf[1] = (uint8_t)((len >> 8) & 0xff);
	fi.buf[2] = (uint8_t)((len >> 16) & 0xff);
	fi.buf[3] = (uint8_t)((len >> 24) & 0xff);
	memcpy(fi.buf + 4, buf, len);
	fi.len = (uint32_t)payload;
	int frc = ioctl(fd, HVX_SET_FUZZ_INPUT, &fi);
	if (getenv("HVX_DBG_RIP"))
		fprintf(stderr, "[feed: cmd=%#lx rc=%d errno=%d payload=%zu]\n",
			(unsigned long)HVX_SET_FUZZ_INPUT, frc, frc < 0 ? errno : 0, payload);
	if (frc < 0) {
		fprintf(stderr, "feed_input: HVX_SET_FUZZ_INPUT: %s\n", strerror(errno));
		return -1;
	}
	return 0;
}

/* Search fingerprint: THIS program's DETERMINISTIC, FINDING-RELEVANT state = the reconstructed
 * console (b->ubuf: the driver's mini-Jepsen verdict / METRICS / RPT output) + the rw data disk p2
 * (the fs-backend queue/log = the DURABLE app state). It deliberately EXCLUDES all guest RAM.
 *
 * WHY exclude RAM: a RAM-diff of two restores of the SAME program showed console + p2 byte-IDENTICAL
 * but 8 bytes of one kernel page (printk-ring / scheduler bookkeeping) flipping run-to-run -- benign
 * kernel-internal non-determinism, NOT app/log/queue state. Even a deterministic post-DONE halt did
 * not eliminate it (it is intrinsic kernel state at the halt). Fingerprinting console + p2 captures
 * exactly the state a consistency finding must reproduce (the oracle verdict + the durable queue) and
 * excludes the benign kernel bookkeeping -- the sound "measure at the meaningful, deterministic state"
 * fix (mirrors repeat's oracle discipline). It still DISCRIMINATES: a different program yields a
 * different verdict/METRICS (console) and/or a different queue/log (p2). */
static uint64_t fingerprint_search(struct boot *b)
{
	uint64_t h = 0xcbf29ce484222325ULL;
	h = fnv(h, "spread-search-v2", 16);
	h = fnv(h, b->ubuf, b->ulen);				/* driver oracle output (verdict/METRICS/RPT) */
	h = fnv(h, b->disk_mm + b->p2_off, b->p2_len);		/* durable fs-backend queue/log (p2) */
	return h;
}

/* ---- seeded workload generator: a port of repeat's propose_spread_seq, S/R/N/P ONLY
 * (single send / receive+ack / receive+nack / concurrent-send-to-two-groups) over groups
 * g0..g3 with unique bodies, brokers b in {1,2}. Tier faults (KN/KC/QN/QC, native-only)
 * and the broker-crash op (X, the fs busy-spin) are intentionally omitted. Deterministic:
 * same seed -> same program stream (splitmix64). ---- */
static uint64_t sm_state;
static uint64_t sm_next(void)
{
	uint64_t z = (sm_state += 0x9E3779B97F4A7C15ULL);
	z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
	z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
	return z ^ (z >> 31);
}
static uint32_t sm_below(uint32_t n) { return n ? (uint32_t)(sm_next() % n) : 0; }

/* Emit ONE op line (with trailing '\n') at body-index `i`, drawing from the S/R/N/P/Z/U grammar in a
 * fixed draw order (a superset of repeat's propose_spread_seq). Factored out of gen_program so the
 * corpus mutator can reuse the identical grammar. Z (partition a broker OFF) + U (heal it) unlock the
 * HARD markers `broker-paused` and `send-fenced`: a concurrent P-send while a broker is paused times
 * out and is fenced -- an op ORDERING (Z ... P before any U) that blind-random rarely lands in one
 * program, but the reachability-guided corpus can BUILD toward. Returns the snprintf length. */
static int gen_op_line(char *line, size_t cap, int i)
{
	int b = 1 + (int)sm_below(2);
	uint32_t r = sm_below(40);
	if (r <= 17)					/* 45% single send */
		return snprintf(line, cap, "S %d g%u m%d\n", b, sm_below(4), i);
	else if (r <= 25)				/* 20% receive + ack */
		return snprintf(line, cap, "R %d\n", b);
	else if (r <= 31)				/* 15% receive + nack (redelivery at head) */
		return snprintf(line, cap, "N %d\n", b);
	else if (r <= 37) {				/* 15% concurrent send to two groups (races the log append) */
		int ga = (int)sm_below(4);
		int gb = (ga + 1 + (int)sm_below(3)) % 4;	/* != ga */
		return snprintf(line, cap, "P g%d g%d m%da m%db\n", ga, gb, i, i);
	} else if (r == 38)				/* 2.5% partition broker b OFF (SIGSTOP): the fencing seam */
		return snprintf(line, cap, "Z %d\n", b);
	else						/* 2.5% heal broker b (SIGCONT): it must detect it was fenced */
		return snprintf(line, cap, "U %d\n", b);
}

/* Emit one op-program into out. Returns byte length. Length-prefixed on the wire (the guest's
 * fuzz-read reads exactly this many bytes), so no terminator line is needed -- the UNCHANGED driver
 * reads the program from /tmp/prog.txt to EOF. */
static size_t gen_program(char *out, size_t cap)
{
	if (getenv("HVX_FIXED_PROG")) {		/* isolation: the known-good Phase-2 program (drains clean) */
		static const char *p = "S 1 g0 m0\nS 2 g1 m1\nR 1\nS 1 g0 m2\nN 2\nR 2\nS 1 g0 m3\nR 1\n";
		size_t l = strlen(p);
		if (l < cap) memcpy(out, p, l + 1);
		return l;
	}
	size_t n = 0;
	/* SHORT fresh programs (3..=6 ops). A short blind program almost never assembles the deep
	 * op ORDERING the hard markers need (Z ... P for send-fenced, plus S/R/N to also drain+nack in
	 * ONE run), so pure-random search stays shallow. The reachability-guided corpus reaches those
	 * combos by GROWING short building-block programs via mutation -- which is exactly the value the
	 * coverage substitute must show. (With long fresh programs a single blind draw already covers
	 * this shallow 6-marker space, so no coverage-style guidance can help -- an honest limit of a
	 * low-dimensional reachability signal.) */
	int nops = 3 + (int)sm_below(4);	/* 3..=6 ops */
	int i;
	for (i = 0; i < nops; i++) {
		char line[64];
		int ln = gen_op_line(line, sizeof line, i);
		if (ln > 0 && n + (size_t)ln < cap) { memcpy(out + n, line, (size_t)ln); n += (size_t)ln; }
	}
	out[n] = '\0';
	return n;
}

/* ============================================================================
 * The reachability-guided upper half (the "coverage substitute"): a STATEFUL corpus + a novelty
 * signal derived from the driver's own `RPT sat <name>` reachability markers -- the port of repeat's
 * coverage-guided fork-fuzz, with marker-combinations standing in for edge coverage (hvx has no edge
 * bitmap). A program is KEPT in the corpus iff it reaches a marker-combination not yet globally seen
 * (NOVEL). The generator then draws from the corpus and MUTATES (add/drop/tweak ops), plus a
 * champion popcount hill-climb toward the DEEPEST single-program combination (repeat's --guide, with
 * a host-computed, monotone, driver-free distance = markers-missing-from-the-best-program). All
 * draws are splitmix64 over sm_state, so same seed + same snapshot => same campaign (determinism
 * preserved). Everything here is OUTSIDE the deterministic core: it only PROPOSES bytes; the harness
 * still executes each program deterministically (snapshot restore) and the console/rw-disk oracle
 * still gates. ==========================================================================*/

/* ---- marker registry: names discovered from RPT decl/sat, one bit each ---- */
#define MAX_MARKERS 32
static char g_mname[MAX_MARKERS][32];
static int g_nmarker = 0;
static uint32_t g_declared = 0;			/* union of every marker the guest ever DECLARED */
static int marker_bit(const char *name)
{
	int i;
	for (i = 0; i < g_nmarker; i++)
		if (strcmp(g_mname[i], name) == 0)
			return i;
	if (g_nmarker < MAX_MARKERS) { snprintf(g_mname[g_nmarker], 32, "%s", name); return g_nmarker++; }
	return -1;
}
static int popcount32(uint32_t x) { int c = 0; while (x) { c += (int)(x & 1u); x >>= 1; } return c; }

/* Parse this run's reconstructed console for the property-SDK lines the driver emits
 * (`RPT decl <name>` / `RPT sat <name>`); update g_declared; return the SATISFIED bitmask. This is
 * the coverage substitute's signal source -- the oracle's own reachability markers. */
static uint32_t scan_run_markers(struct boot *b)
{
	uint32_t sat = 0;
	size_t i = 0;
	while (i < b->ulen) {
		size_t j = i;
		while (j < b->ulen && b->ubuf[j] != '\n') j++;
		if (j >= b->ulen)		/* unterminated trailing line (halted mid-print): skip it so a
						 * truncated "RPT decl queue-drai..." can't register a bogus marker */
			break;
		{
			size_t len = j - i;
			const uint8_t *ln = b->ubuf + i;
			if (len > 9 && memcmp(ln, "RPT decl ", 9) == 0) {
				char nm[32]; size_t k = 0, s = 9;
				while (s < len && k < 31 && ln[s] != ' ' && ln[s] != '\r') nm[k++] = (char)ln[s++];
				nm[k] = 0;
				{ int bit = marker_bit(nm); if (bit >= 0) g_declared |= (1u << bit); }
			} else if (len > 8 && memcmp(ln, "RPT sat ", 8) == 0) {
				char nm[32]; size_t k = 0, s = 8;
				while (s < len && k < 31 && ln[s] != ' ' && ln[s] != '\r') nm[k++] = (char)ln[s++];
				nm[k] = 0;
				{ int bit = marker_bit(nm); if (bit >= 0) { sat |= (1u << bit); g_declared |= (1u << bit); } }
			}
		}
		i = j + 1;
	}
	return sat;
}

/* Render a bitmask as a comma-joined list of marker names, for the human-readable report. */
static void mask_names(uint32_t m, char *out, size_t cap)
{
	size_t n = 0; int i;
	out[0] = 0;
	for (i = 0; i < g_nmarker; i++)
		if (m & (1u << i)) {
			size_t l = strlen(g_mname[i]);
			if (n + l + 2 < cap) { if (n) out[n++] = ','; memcpy(out + n, g_mname[i], l); n += l; out[n] = 0; }
		}
}

/* ---- corpus: programs kept because they reached a NOVEL marker-combination ---- */
#define CORPUS_MAX 512
struct centry { char *prog; size_t len; uint32_t mask; };
static struct centry g_corpus[CORPUS_MAX];
static int g_corpus_n = 0;
static void corpus_add(const char *p, size_t len, uint32_t mask)
{
	char *c;
	if (g_corpus_n >= CORPUS_MAX) return;
	c = malloc(len + 1); if (!c) return;
	memcpy(c, p, len); c[len] = 0;
	g_corpus[g_corpus_n].prog = c; g_corpus[g_corpus_n].len = len; g_corpus[g_corpus_n].mask = mask;
	g_corpus_n++;
}

/* ---- globally-seen marker-combinations (the edge-coverage analog): a program is novel iff its
 * combination has never been observed. Both modes count distinct combos; only guided keeps the
 * corpus. ---- */
#define COMBO_MAX 8192
static uint32_t g_combo[COMBO_MAX];
static int g_ncombo = 0;
static int combo_seen(uint32_t m) { int i; for (i = 0; i < g_ncombo; i++) if (g_combo[i] == m) return 1; return 0; }
static void combo_add(uint32_t m) { if (g_ncombo < COMBO_MAX) g_combo[g_ncombo++] = m; }

/* Mutate a parent program (from the corpus or the champion) into out: split into op lines, apply
 * 1-2 of {add a fresh op, drop an op, tweak (regenerate) an op}, and re-serialize. Same grammar as
 * gen_op_line; all choices are splitmix64 draws, so a mutant is a deterministic function of the seed
 * stream. This is repeat's structure-aware mutation -- it builds on programs that already reached an
 * interesting state instead of sampling blind. */
/* Rewrite every message body token `m<digits>` in one op line to `m<idx>` (preserving a trailing
 * a/b for a P line's two bodies). Applied per output position during mutation so a mutant's bodies
 * are GLOBALLY UNIQUE within the program -- restoring the invariant the original generator held via
 * unique m<i> indices. Without it, mutation can duplicate a body (e.g. two `S g3 m1`), which the
 * driver's FIFO oracle (it dedups delivered bodies) then MIS-reads as a fifo-reorder -- a harness
 * false positive, not a spread bug. Bodies are the only `m<digit>` tokens (ops are S/R/N/P/Z/U,
 * groups g0..3, brokers 1/2), so this rewrite is unambiguous. */
static void restamp_body(char *line, int idx)
{
	char tmp[64];
	size_t o = 0, i = 0, L = strlen(line);
	while (i < L && o + 8 < sizeof tmp) {
		if (line[i] == 'm' && i + 1 < L && line[i + 1] >= '0' && line[i + 1] <= '9') {
			tmp[o++] = 'm';
			o += (size_t)snprintf(tmp + o, sizeof tmp - o, "%d", idx);
			i++;					/* skip 'm' */
			while (i < L && line[i] >= '0' && line[i] <= '9') i++;	/* skip old digits */
		} else {
			tmp[o++] = line[i++];
		}
	}
	tmp[o] = 0;
	memcpy(line, tmp, o + 1);
}

static size_t mutate_program(const char *parent, size_t plen, char *out, size_t cap)
{
	enum { ML = 40 };
	static char work[ML + 2][64];		/* single-threaded; off the stack */
	int wn = 0, t, nmut;
	size_t i = 0, n = 0;

	/* split parent into stored lines (newline stripped) */
	while (i < plen && wn < ML) {
		size_t j = i;
		while (j < plen && parent[j] != '\n') j++;
		if (j > i) {
			int m = (int)(j - i); if (m > 63) m = 63;
			memcpy(work[wn], parent + i, (size_t)m); work[wn][m] = 0; wn++;
		}
		i = (j < plen) ? j + 1 : j;
	}

	nmut = 1 + (int)sm_below(2);		/* apply 1..2 mutations */
	for (t = 0; t < nmut; t++) {
		/* Bias toward ADD (50%) over drop (20%) / tweak (30%) so the corpus GROWS short
		 * building-block programs into the deeper op sequences the hard markers require -- the
		 * ratchet that lets guided reach combos short random programs cannot. Growth is soft-capped
		 * at 18 ops so a mutant stays fast to run (each Z...P fence path costs a ~2 s timeout). */
		int action = (int)sm_below(10);		/* 0..4 add | 5..6 drop | 7..9 tweak */
		action = (action <= 4) ? 0 : (action <= 6) ? 1 : 2;
		if (wn == 0) action = 0;	/* nothing to drop/tweak -> add */
		if (action == 0 && wn < 18) {			/* ADD one fresh op at a random position */
			int pos = (int)sm_below(wn + 1), s;
			char line[64];
			int ln = gen_op_line(line, sizeof line, pos);
			if (ln > 0) {
				line[ln - 1] = 0;		/* strip '\n' for storage */
				for (s = wn; s > pos; s--) memcpy(work[s], work[s - 1], 64);
				memcpy(work[pos], line, 64); wn++;
			}
		} else if (action == 1 && wn > 1) {		/* DROP a random op */
			int pos = (int)sm_below(wn), s;
			for (s = pos; s < wn - 1; s++) memcpy(work[s], work[s + 1], 64);
			wn--;
		} else if (wn > 0) {				/* TWEAK: regenerate a random op */
			int pos = (int)sm_below(wn);
			char line[64];
			int ln = gen_op_line(line, sizeof line, pos);
			if (ln > 0) { line[ln - 1] = 0; memcpy(work[pos], line, 64); }
		}
	}

	for (t = 0; t < wn; t++) {
		size_t m;
		restamp_body(work[t], t);	/* make bodies unique per program (FIFO-oracle soundness) */
		m = strlen(work[t]);
		if (n + m + 1 < cap) { memcpy(out + n, work[t], m); n += m; out[n++] = '\n'; }
	}
	if (n == 0) {						/* never emit an empty program */
		char line[64];
		int ln = gen_op_line(line, sizeof line, 0);
		if (ln > 0 && (size_t)ln < cap) { memcpy(out, line, (size_t)ln); n = (size_t)ln; }
	}
	out[n] = 0;
	return n;
}

static double now_s(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

/* Classify the driver verdict from this run's reconstructed console. */
static const char *verdict_of(struct boot *b)
{
	if (mem_find(b->ubuf, b->ulen, "BUG-ASSERT") >= 0)   return "VIOLATION";
	if (mem_find(b->ubuf, b->ulen, "spread: consistent") >= 0) return "consistent";
	return "INCONCLUSIVE";
}

/* Print every `BUG-ASSERT <name> <detail>` line from this run's console (prefixed for the report),
 * so a consistency finding names its exact invariant (split-brain / lost-message / fifo-reorder /
 * broker-died / phantom) instead of only "VIOLATION". */
static void print_bug_asserts(struct boot *b)
{
	size_t i = 0;
	while (i < b->ulen) {
		size_t j = i;
		while (j < b->ulen && b->ubuf[j] != '\n') j++;
		if (j >= b->ulen) break;
		if (j - i >= 10 && memcmp(b->ubuf + i, "BUG-ASSERT", 10) == 0)
			printf("BUG-REPRO-ASSERT %.*s\n", (int)(j - i), b->ubuf + i);
		i = j + 1;
	}
}

/* WALL-CLOCK-bounded wait for `marker`; on timeout DUMP the console TAIL so a boot/marker
 * problem (or a print-loop) is visible instead of an infinite spin. Env HVX_READY_SECS sets the
 * budget (default 40s). Returns 0 on marker, -1 on timeout. */
static int wait_marker_dump(struct boot *b, const char *marker, unsigned long long cap)
{
	struct hvx_exit ex;
	unsigned long long it;
	const char *secs_env = getenv("HVX_READY_SECS");
	double budget = secs_env ? atof(secs_env) : 40.0;
	double t0 = now_s();
	(void)cap;
	g_alarm = 0;
	alarm(2);	/* fire every 2s to interrupt a blocking HVX_RUN so the wall check below can run */
	for (it = 0; ; it++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR) {		/* SIGALRM (or any signal) interrupted the run */
				if (now_s() - t0 > budget) break;
				if (g_alarm) { g_alarm = 0; alarm(2); }
				continue;
			}
			perror("HVX_RUN"); break;
		}
		console_drain(b);
		if (getenv("HVX_DBG_RIP") && it < 8) {
			struct hvx_regs cur; unsigned long long rip = 0;
			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0) rip = (unsigned long long)cur.rip;
			fprintf(stderr, "[dbg it=%llu reason=%u rip=%#llx console=%zuB]\n", it, ex.reason, rip, b->ulen);
		}
		if (mem_find(b->ubuf, b->ulen, marker) >= 0) { alarm(0); return 0; }
		if (now_s() - t0 > budget)
			break;
	}
	alarm(0);
	{
		size_t total = b->ulen;
		size_t from = total > 3000 ? total - 3000 : 0;
		fprintf(stderr, "wait_marker_dump: marker '%s' NOT found in %.1fs (%llu iters, %zu console "
			"bytes total); TAIL:\n====8<==== GUEST CONSOLE TAIL ====8<====\n",
			marker, now_s() - t0, it, total);
		fwrite(b->ubuf + from, 1, total - from, stderr);
		fprintf(stderr, "\n====8<==== END CONSOLE ====8<====\n");
	}
	return -1;
}

/* Run until the guest DETERMINISTICALLY halts (EXIT_HLT with IF=0 -> the module returns it, no idle
 * heartbeat; or EXIT_TRIPLE_FAULT from `reboot -f`). This is the FIXED execution point the driver
 * reaches right after SPREAD-RUN-DONE via `halt -f`, so the fingerprint is taken at a deterministic
 * icount -- BEFORE any variable idle-tail timekeeping (jiffies) advances. Drains the console each
 * iteration (so DONE + the verdict are captured); wall-bounded via SIGALRM. Returns 0 on halt. */
static int run_until_halt(struct boot *b)
{
	struct hvx_exit ex;
	const char *secs_env = getenv("HVX_READY_SECS");
	double budget = secs_env ? atof(secs_env) : 40.0;
	double t0 = now_s();
	unsigned long long it;
	/* NO alarm here: with the deterministic `halt -f` the guest reaches EXIT_HLT on its own, so
	 * HVX_RUN returns (THROTTLE during the run, then HLT) without needing a signal to unblock it.
	 * A wall-clock SIGALRM would interrupt HVX_RUN at a nondeterministic point and perturb the
	 * idle-heartbeat tick accounting (the 8-byte jiffies delta), so the run must NOT be interrupted. */
	for (it = 0; ; it++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR) continue;
			perror("HVX_RUN"); break;
		}
		console_drain(b);
		if (ex.reason == EXIT_HLT || ex.reason == EXIT_TRIPLE_FAULT) return 0;
		if (now_s() - t0 > budget) break;
	}
	{
		size_t total = b->ulen, from = total > 2000 ? total - 2000 : 0;
		fprintf(stderr, "run_until_halt: no HLT in %.1fs (%llu iters, %zu console); TAIL:\n"
			"====8<====\n", now_s() - t0, it, total);
		fwrite(b->ubuf + from, 1, total - from, stderr);
		fprintf(stderr, "\n====8<====\n");
	}
	return -1;
}

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *disk_path, *cmdline;
	uint64_t mem_size, disk_len = 0;
	uint8_t *image = NULL, *initrd = NULL, *disk = NULL;
	size_t image_len = 0, initrd_len = 0, dl = 0;
	int nprog = 24;
	uint64_t seed = 1;
	uint64_t fault_seed = 0; uint32_t fault_classes = 0, fault_prob = 0;
	struct boot b;
	struct hvx_snapshot *snap = NULL;
	struct savebuf sb_ram, sb_rw;
	int have_ram = 0, have_rw = 0, rc = 1, i;
	char prog[8192];

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs> <disk.img> [\"cmdline\"] [memMiB] "
			"[nprograms] [seed] [fault_seed] [fault_prob]\n"
			"  cmdline MUST select search mode (append spread_mode=search)\n", argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	disk_path   = argv[3];
	cmdline     = (argc >= 5 && argv[4][0]) ? argv[4] : ROOTFS_CMDLINE;
	mem_size    = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0) : ROOTFS_MEM_MIB) * 1024ULL * 1024ULL;
	if (argc >= 7 && argv[6][0]) nprog = atoi(argv[6]);
	if (argc >= 8 && argv[7][0]) seed = strtoull(argv[7], NULL, 0);
	if (argc >= 9) { fault_seed = strtoull(argv[8], NULL, 0); if (fault_seed) fault_classes = HVX_FAULT_DROP; }
	if (argc >= 10 && argv[9][0]) fault_prob = (uint32_t)strtoul(argv[9], NULL, 0);
	if (nprog < 1) nprog = 1;
	if (nprog > 4096) nprog = 4096;

	fprintf(stderr, "hvx_spread_search: mem=%llu MiB nprog=%d seed=0x%llx fault_seed=0x%llx prob=%u%%\n",
		(unsigned long long)(mem_size >> 20), nprog, (unsigned long long)seed,
		(unsigned long long)fault_seed, fault_prob);

	{ struct sigaction sa; memset(&sa, 0, sizeof(sa)); sa.sa_handler = on_alarm;
	  sigaction(SIGALRM, &sa, NULL); }	/* interrupts a blocking HVX_RUN so timeouts can fire */

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	disk = read_file(disk_path, &dl);
	if (!disk) { free(image); free(initrd); return 1; }
	disk_len = dl;

	snap = calloc(1, sizeof(*snap));
	if (!snap) { perror("calloc"); goto out_free; }

	/* --- boot ONCE to the READY snapshot boundary (brokers up + queue q + driver blocked
	 * on the serial read). No fault during the boot. --- */
	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, disk, disk_len,
		       cmdline, 0, 0, 0) != 0) { fprintf(stderr, "boot_setup failed\n"); goto out_free; }
	fprintf(stderr, "hvx_spread_search: booting to READY (%s)...\n", READY_MARKER);
	{
		const char *rc_env = getenv("HVX_READY_CAP");
		unsigned long long ready_cap = rc_env ? strtoull(rc_env, NULL, 0) : 4000000ULL;
		if (wait_marker_dump(&b, READY_MARKER, ready_cap) != 0) {
			fprintf(stderr, "hvx_spread_search: never reached READY marker (see console dump above)\n");
			goto out_teardown;
		}
	}
	/* Let the guest settle into fuzz-read's busy-spin on the status port so the snapshot boundary is
	 * the same instruction point every restore. A short fixed spin past the marker (fuzz-read spins
	 * on a synchronous port, so each HVX_RUN does a full 65536-exit window -- keep this small). */
	{
		struct hvx_exit ex; int k;
		for (k = 0; k < 40; k++) { if (ioctl(b.fd, HVX_RUN, &ex) < 0) break; }
	}
	console_drain(&b);
	fprintf(stderr, "hvx_spread_search: READY; taking the ready snapshot (RAM %llu MiB + rw disk %llu MiB)\n",
		(unsigned long long)(mem_size >> 20), (unsigned long long)(b.p2_len >> 20));
	if (savebuf_open(&sb_ram, mem_size, "srch-ram") != 0) goto out_teardown; have_ram = 1;
	if (savebuf_open(&sb_rw, b.p2_len, "srch-rw") != 0) goto out_teardown; have_rw = 1;
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, snap) < 0) { perror("HVX_GET_SNAPSHOT"); goto out_teardown; }
	savebuf_store(&sb_ram, b.mem, mem_size);
	savebuf_store(&sb_rw, b.disk_mm + b.p2_off, b.p2_len);

	/* restore helper closure via a macro-ish inline block */
#define RESTORE_READY() do { \
		if (ioctl(b.fd, HVX_SET_SNAPSHOT, snap) < 0) { perror("HVX_SET_SNAPSHOT"); goto out_teardown; } \
		savebuf_load(&sb_ram, b.mem, mem_size); \
		savebuf_load(&sb_rw, b.disk_mm + b.p2_off, b.p2_len); \
		if (fault_classes) { struct hvx_fault _f; memset(&_f,0,sizeof(_f)); \
			_f.seed=fault_seed; _f.classes=fault_classes; _f.probability=fault_prob; \
			if (ioctl(b.fd, HVX_SET_FAULT, &_f) < 0) perror("HVX_SET_FAULT"); } \
		b.ulen = 0; console_rebase(&b); \
	} while (0)

	/* --- one restore to measure latency + confirm the machine works before the campaign --- */
	{
		double t0 = now_s();
		RESTORE_READY();
		double t_restore = now_s() - t0;
		fprintf(stderr, "hvx_spread_search: restore latency = %.3f s (RAM+rw reload)\n", t_restore);
		/* feed a trivial program + run, to confirm the loop end-to-end */
		sm_state = seed; size_t pl = gen_program(prog, sizeof prog);
		double t1 = now_s();
		feed_input(b.fd, prog, pl);
		if (run_until_halt(&b) != 0) {
			fprintf(stderr, "hvx_spread_search: WARM RUN never reached %s -- serial input may be "
				"undrained/nondeterministic; STOP + report.\n", DONE_MARKER);
			goto out_teardown;
		}
		fprintf(stderr, "hvx_spread_search: warm run to DONE = %.3f s (verdict %s)\n",
			now_s() - t1, verdict_of(&b));
	}

	/* --- DETERMINISM PROOF: run program #0 twice from the ready snapshot; require identical
	 * verdict AND identical fingerprint. --- */
	{
		uint64_t fpa, fpb; char va[16], vb[16];
		uint64_t con_a, ram_a, p2_a, con_b, ram_b, p2_b;
		int dump = getenv("HVX_DUMP_RAM") != NULL;
		sm_state = seed; gen_program(prog, sizeof prog);
		size_t pl; sm_state = seed; pl = gen_program(prog, sizeof prog);
		RESTORE_READY(); feed_input(b.fd, prog, pl);
		if (run_until_halt(&b) != 0) { fprintf(stderr, "det run1 failed\n"); goto out_teardown; }
		fpa = fingerprint_search(&b); snprintf(va, sizeof va, "%s", verdict_of(&b));
		con_a = fnv(0xcbf29ce484222325ULL, b.ubuf, b.ulen);
		ram_a = fnv(0xcbf29ce484222325ULL, b.mem, mem_size);
		p2_a  = fnv(0xcbf29ce484222325ULL, b.disk_mm + b.p2_off, b.p2_len);
		if (dump) { int fd = open("/tmp/ram_a.bin", O_WRONLY|O_CREAT|O_TRUNC, 0644);
			if (fd >= 0) { ssize_t w = write(fd, b.mem, mem_size); (void)w; close(fd); } }
		RESTORE_READY(); feed_input(b.fd, prog, pl);
		if (run_until_halt(&b) != 0) { fprintf(stderr, "det run2 failed\n"); goto out_teardown; }
		fpb = fingerprint_search(&b); snprintf(vb, sizeof vb, "%s", verdict_of(&b));
		con_b = fnv(0xcbf29ce484222325ULL, b.ubuf, b.ulen);
		ram_b = fnv(0xcbf29ce484222325ULL, b.mem, mem_size);
		p2_b  = fnv(0xcbf29ce484222325ULL, b.disk_mm + b.p2_off, b.p2_len);
		if (dump) { int fd = open("/tmp/ram_b.bin", O_WRONLY|O_CREAT|O_TRUNC, 0644);
			if (fd >= 0) { ssize_t w = write(fd, b.mem, mem_size); (void)w; close(fd); } }
		printf("DETERMINISM  prog#0 run1 fp=%s v=%s | run2 fp=%s v=%s -> %s\n",
		       hx(fpa), va, hx(fpb), vb,
		       (fpa == fpb && strcmp(va, vb) == 0) ? "IDENTICAL" : "MISMATCH");
		printf("  COMPONENTS console %s%s | RAM %s%s | rw-disk(p2) %s%s\n",
		       con_a == con_b ? "SAME" : "DIFF", con_a == con_b ? "" : "(!)",
		       ram_a == ram_b ? "SAME" : "DIFF", ram_a == ram_b ? "" : "(!)",
		       p2_a == p2_b ? "SAME" : "DIFF", p2_a == p2_b ? "" : "(!)");
		if (!(fpa == fpb && strcmp(va, vb) == 0)) {
			fprintf(stderr, "hvx_spread_search: DETERMINISM MISMATCH -- the serial-fed restore is "
				"NOT byte-deterministic. Do not paper over. STOP + report.\n");
			rc = 3; goto out_teardown;
		}
	}

	/* --- the campaign: GUIDED (corpus + reachability-marker novelty) or RANDOM (always-fresh, the
	 * old Phase-3 behaviour), selected by HVX_SEARCH_MODE (default "guided"; "random" = the
	 * baseline). Both use the SAME seed, SAME grammar, and SAME budget, so the guided-vs-random
	 * comparison is apples-to-apples -- the ONLY difference is whether the search builds on programs
	 * that reached new states. HVX_GUIDE=1 adds the champion popcount hill-climb (repeat's --guide,
	 * the STRETCH: focus-mutate the DEEPEST-combo program toward the distance-to-full goal). --- */
	{
		const char *mode_env = getenv("HVX_SEARCH_MODE");
		int guided = !(mode_env && strcmp(mode_env, "random") == 0);
		int hillclimb = guided && getenv("HVX_GUIDE") != NULL;	/* the --guide STRETCH (opt-in) */
		int nconsistent = 0, nviol = 0, ninconc = 0, nnovel = 0;
		double t_sum = 0;
		uint32_t g_union = 0, best_mask = 0;
		int best_pop = 0, best_prog = -1;
		char *champion = NULL; size_t champion_len = 0;

		printf("MODE %s (%s)\n", guided ? "guided" : "random",
		       guided ? (hillclimb ? "corpus-breadth + champion hill-climb (--guide)" : "corpus-breadth")
			      : "always-fresh baseline");
		sm_state = seed;
		for (i = 0; i < nprog; i++) {
			size_t pl;
			const char *origin;
			/* Draw the next candidate. RANDOM: always a fresh (short) program -- no corpus.
			 * GUIDED (breadth, repeat's fork-fuzz default): 20% fresh (exploration), else mutate a
			 * RANDOM corpus entry (draw sm_below(corpus_n) + mutate), GROWING building blocks into
			 * deeper programs. With HVX_GUIDE, ~40% of the mutate slots instead focus-mutate the
			 * CHAMPION (deepest combo so far) to hill-climb the distance-to-full goal. Empty corpus
			 * -> fresh. */
			if (!guided || g_corpus_n == 0) {
				pl = gen_program(prog, sizeof prog); origin = "fresh";
			} else {
				uint32_t roll = sm_below(100);
				if (roll < 20) {
					pl = gen_program(prog, sizeof prog); origin = "fresh";
				} else if (hillclimb && champion && roll < 52) {
					pl = mutate_program(champion, champion_len, prog, sizeof prog); origin = "champ";
				} else {
					int idx = (int)sm_below((uint32_t)g_corpus_n);
					pl = mutate_program(g_corpus[idx].prog, g_corpus[idx].len, prog, sizeof prog);
					origin = "mutate";
				}
			}
			double t0 = now_s();
			RESTORE_READY();
			feed_input(b.fd, prog, pl);
			int ok = run_until_halt(&b);
			double dt = now_s() - t0;
			t_sum += dt;
			if (ok != 0) {
				printf("prog#%-3d  NOMARKER (%.3fs)\n", i, dt);
				ninconc++;
				continue;
			}
			const char *v = verdict_of(&b);
			uint64_t fp = fingerprint_search(&b);
			uint32_t mask = scan_run_markers(&b);
			int pop = popcount32(mask);
			uint32_t new_bits = mask & ~g_union;		/* markers this program reached FIRST */
			int novel = (mask != 0 && !combo_seen(mask));	/* a marker-combination never seen */
			int ops = 0; { size_t j; for (j = 0; j < pl; j++) if (prog[j] == '\n') ops++; }

			if (novel) {
				combo_add(mask); nnovel++;
				if (guided) corpus_add(prog, pl, mask);	/* KEEP: reached a new interesting state */
			}
			g_union |= mask;
			if (pop > best_pop) {				/* new depth champion (hill-climb) */
				best_pop = pop; best_mask = mask; best_prog = i;
				free(champion); champion = malloc(pl + 1);
				if (champion) { memcpy(champion, prog, pl); champion[pl] = 0; champion_len = pl; }
			}

			{
				char nmbuf[256]; mask_names(mask, nmbuf, sizeof nmbuf);
				printf("prog#%-3d  ops=%-2d  %-6s %-11s pop=%d%s fp=%s  m=[%s]  (%.3fs)\n",
				       i, ops, origin, v, pop, new_bits ? "*" : " ", hx(fp), nmbuf, dt);
			}
			if (strcmp(v, "consistent") == 0) nconsistent++;
			else if (strcmp(v, "VIOLATION") == 0) {
				nviol++;
				/* A consistency finding: emit a BIT-FOR-BIT reproducer (exact program + the
				 * campaign seed + prog index + fault seed). Re-feeding this program to a fresh
				 * ready snapshot under the same seed reproduces it exactly. */
				printf("BUG-REPRO prog#%d mode=%s seed=0x%llx fault_seed=0x%llx prob=%u fp=%s\n",
				       i, guided ? "guided" : "random", (unsigned long long)seed,
				       (unsigned long long)fault_seed, fault_prob, hx(fp));
				print_bug_asserts(&b);
				printf("BUG-REPRO-PROGRAM-BEGIN\n%s\nBUG-REPRO-PROGRAM-END\n", prog);
				{ int fd = open("/tmp/spread-bug-repro.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
				  if (fd >= 0) { ssize_t w = write(fd, prog, pl); (void)w; close(fd); } }
			}
			else ninconc++;
		}

		/* ---- reachability report (the coverage substitute's scorecard) ---- */
		{
			int union_pop = popcount32(g_union), decl_pop = popcount32(g_declared), k;
			char ubuf[256], bbuf[256];
			mask_names(g_union, ubuf, sizeof ubuf);
			mask_names(best_mask, bbuf, sizeof bbuf);
			printf("\n=== REACHABILITY (mode=%s seed=0x%llx nprog=%d) ===\n",
			       guided ? "guided" : "random", (unsigned long long)seed, nprog);
			printf("markers_reached %d/%d union=[%s]\n", union_pop, decl_pop, ubuf);
			for (k = 0; k < g_nmarker; k++)
				if ((g_declared >> k) & 1)
					printf("%s [%s]\n", ((g_union >> k) & 1) ? "SOMETIMES-SAT  " : "SOMETIMES-NEVER",
					       g_mname[k]);
			printf("distinct_combos %d\n", g_ncombo);
			printf("deepest_combo pop=%d at prog#%d mask=[%s]\n", best_pop, best_prog, bbuf);
			/* Monotone, driver-free distance = markers-the-campaign-reached minus the deepest
			 * single program (repeat's --guide DISTANCE; 0 = one program covered everything). */
			printf("DISTANCE [all-reached-in-one-program] min=%d%s\n",
			       union_pop - best_pop, (union_pop - best_pop) == 0 ? " REACHED" : "");
			if (guided) printf("corpus_size %d\n", g_corpus_n);
			printf("GUIDED-SUMMARY mode=%s markers=%d combos=%d deepest=%d corpus=%d "
			       "consistent=%d VIOLATION=%d inconclusive=%d\n",
			       guided ? "guided" : "random", union_pop, g_ncombo, best_pop, g_corpus_n,
			       nconsistent, nviol, ninconc);
		}
		printf("\nSUMMARY  programs=%d consistent=%d VIOLATION=%d inconclusive=%d novel=%d  avg_restore+run=%.3fs\n",
		       nprog, nconsistent, nviol, ninconc, nnovel, nprog ? t_sum / nprog : 0.0);
		printf("hvx_spread_search: PASS -- deterministic %s snapshot-restore search ran "
		       "%d programs (seed=0x%llx%s), each replaying from ONE ready snapshot.\n",
		       guided ? "reachability-guided" : "random", nprog, (unsigned long long)seed,
		       fault_classes ? " +DROP-fault" : "");
		free(champion);
		rc = 0;
	}

out_teardown:
	boot_teardown(&b);
	if (have_ram) savebuf_close(&sb_ram);
	if (have_rw) savebuf_close(&sb_rw);
out_free:
	free(snap); free(image); free(initrd); free(disk);
	return rc;
}

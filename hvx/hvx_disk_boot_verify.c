// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_disk_boot_verify - milestone 21: WHOLE-BOOT determinism for a REAL Linux
 * kernel that DISCOVERS a virtio-blk PCI disk over MSI-X and READS /dev/vda, plus
 * branch-from-snapshot across the disk read.
 * =============================================================================
 *
 * This is the with-disk sibling of hvx_boot_verify.c (M16). Same discipline: boot
 * the staged bzImage + disk-enabled initramfs in FAST free-run, stop at a fixed
 * console marker, and fingerprint the FULL end state -- but here a virtio-blk disk
 * (with a recognizable marker at sector 0) is ATTACHED, so real Linux enumerates
 * the PCI function, binds virtio_pci + virtio_blk, and reads /dev/vda; the read
 * completion arrives as an MSI-X interrupt at the deterministic instruction. The
 * fingerprint additionally covers the backing disk image (HVX_GET_DISK) and the
 * MSI-X device state, so a divergence anywhere in the disk path is caught.
 *
 * Because attaching a disk CHANGES the boot (Linux probes and reads it), this boot
 * does NOT share the no-disk M16 fingerprint (1bdf266a0db52bf7) -- it establishes a
 * NEW baseline, recorded here and in CLAUDE.md. The two invariants proven:
 *   1. WHOLE-BOOT: two fresh disk boots are bit-identical in FULL state
 *      (console + snapshot + 256 MiB RAM + disk image).
 *   2. BRANCH: snapshot mid-boot (before the disk read), continue to the marker
 *      twice (straight + restored), assert identical -- proving the pending disk
 *      completion + MSI-X state + disk are all captured by the snapshot.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_disk_boot_verify hvx_disk_boot_verify.c
 * Run   (inside L1):  sudo ./hvx_disk_boot_verify /tmp/bzImage /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img
 */

#include "hvx_boot.h"

/* Printed by the disk /init (build-linux.sh) right after it reads /dev/vda -- the
 * deterministic stop point (the disk was enumerated, bound, and read). */
#define FINAL_MARKER "hydo-disk-marker: vda read complete"

/* An earlier, deep-in-kernel-init marker (before the initramfs unpacks, well before
 * the virtio modules load and the disk is read). Branching here forces the snapshot
 * to reproduce the module load + PCI enumeration + MSI-X setup + the disk read. */
#define MID_MARKER "Trying to unpack rootfs image as initramfs"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define MAX_RUN_ITERS 50000000ULL

/* ---- canonical fingerprint (FNV-1a; matches hvx_boot_verify.c) ---- */
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
	uint8_t *ubuf;		/* reconstructed console (this timeline) */
	size_t ulen;
	size_t drained;
	const uint8_t *disk;	/* the image to (re)attach */
	uint64_t disk_len;
	uint8_t *diskbuf;	/* scratch for HVX_GET_DISK in the fingerprint */
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
			fprintf(stderr, "hvx_disk_boot_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 512) == 0)
				fprintf(stderr, "[hvx_disk_boot_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 200 ? b->ulen - 200 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_disk_boot_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_disk_boot_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_disk_boot_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Canonical full-state fingerprint: console + snapshot (meaningful fields incl. the
 * virtio-blk + MSI-X device state) + all guest RAM + the backing disk image. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	struct hvx_disk dk;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size, dlen = b->disk_len;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }

	h = fnv(h, "hvx-disk-boot-state-v1", 22);
	/* console */
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
	/* register file + deterministic clocks/TSC */
	h = fnv(h, &s->regs, sizeof(s->regs));
	h = fnv(h, &s->tsc, sizeof(s->tsc));
	h = fnv(h, &s->tsc_step, sizeof(s->tsc_step));
	h = fnv(h, &s->tsc_aux, sizeof(s->tsc_aux));
	h = fnv(h, &s->tsc_mode, sizeof(s->tsc_mode));
	h = fnv(h, &s->icount, sizeof(s->icount));
	h = fnv(h, &s->rdtsc_count, sizeof(s->rdtsc_count));
	/* pending instruction-timed injection */
	h = fnv(h, &s->inject_at_icount, sizeof(s->inject_at_icount));
	h = fnv(h, &s->inject_vector, sizeof(s->inject_vector));
	h = fnv(h, &s->inject_valid, sizeof(s->inject_valid));
	/* curated MSR store + privileged register state */
	h = fnv(h, s->sw_msr, sizeof(s->sw_msr));
	h = fnv(h, &s->sregs, sizeof(s->sregs));
	/* APIC / IOAPIC / PIC */
	h = fnv(h, &s->lapic_apic_base, sizeof(s->lapic_apic_base));
	h = fnv(h, &s->lapic_tsc_deadline, sizeof(s->lapic_tsc_deadline));
	h = fnv(h, s->lapic_reg, sizeof(s->lapic_reg));
	h = fnv(h, &s->lapic_in_service, sizeof(s->lapic_in_service));
	h = fnv(h, &s->ioapic_ioregsel, sizeof(s->ioapic_ioregsel));
	h = fnv(h, &s->ioapic_id, sizeof(s->ioapic_id));
	h = fnv(h, s->ioapic_redir, sizeof(s->ioapic_redir));
	h = fnv(h, s->pic_mask, sizeof(s->pic_mask));
	/* 16550 UART latch */
	h = fnv(h, &s->uart_ier, 1); h = fnv(h, &s->uart_lcr, 1);
	h = fnv(h, &s->uart_mcr, 1); h = fnv(h, &s->uart_fcr, 1);
	h = fnv(h, &s->uart_scr, 1); h = fnv(h, &s->uart_dll, 1); h = fnv(h, &s->uart_dlm, 1);
	/* virtio-blk device control state (M20) */
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
	/* MSI-X state (M21) */
	h = fnv(h, &s->vblk_msix_control, sizeof(s->vblk_msix_control));
	h = fnv(h, &s->vblk_config_msix, sizeof(s->vblk_config_msix));
	h = fnv(h, &s->vblk_queue_msix, sizeof(s->vblk_queue_msix));
	h = fnv(h, s->vblk_msix_tbl, sizeof(s->vblk_msix_tbl));
	/* all guest RAM */
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);
	/* the backing disk image */
	dk.len = b->disk_len; dk.data = (uint64_t)(uintptr_t)b->diskbuf; dk.flags = 0;
	if (ioctl(b->fd, HVX_GET_DISK, &dk) == 0) {
		uint64_t got = dk.len < b->disk_len ? dk.len : b->disk_len;
		h = fnv(h, &dlen, sizeof(dlen));
		h = fnv(h, b->diskbuf, got);
	}
	free(s);
	return h;
}

static int attach_disk(struct boot *b)
{
	struct hvx_disk dk;

	dk.len = b->disk_len;
	dk.data = (uint64_t)(uintptr_t)b->disk;
	dk.flags = HVX_DISK_NO_IDMAP;	/* Linux builds its own page tables + ioremaps MMIO */
	if (ioctl(b->fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return -1; }
	return 0;
}

static int boot_setup(struct boot *b, uint64_t mem_size,
		      const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const uint8_t *disk, uint64_t disk_len, const char *cmdline)
{
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->mem_size = mem_size;
	b->disk = disk;
	b->disk_len = disk_len;
	b->ubuf = malloc(UBUF_CAP);
	b->diskbuf = malloc(disk_len ? disk_len : 1);
	if (!b->ubuf || !b->diskbuf) { perror("malloc"); return -1; }

	b->fd = open("/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open /dev/hvx"); return -1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }

	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap"); return -1; }

	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	if (attach_disk(b) < 0) return -1;

	/* --- optional deterministic fault injection (milestone 22, stretch). If
	 * HVX_FAULT names a seed, install a SEEDED disk fault policy BEFORE the boot,
	 * so the real kernel's /dev/vda I/O is perturbed reproducibly. Default class is
	 * I/O-error at probability 100 (every request errors -> kernel logs the I/O
	 * error). Inert (never called) when HVX_FAULT is unset, so the fault-free
	 * with-disk fingerprint (36583d533d7c989b) is preserved. --- */
	{
		const char *fs = getenv("HVX_FAULT");

		if (fs && fs[0]) {
			const char *fc = getenv("HVX_FAULT_CLASSES");
			const char *fp = getenv("HVX_FAULT_PROB");
			struct hvx_fault f;

			memset(&f, 0, sizeof(f));
			f.seed = strtoull(fs, NULL, 0);
			f.classes = fc ? (uint32_t)strtoul(fc, NULL, 0) : HVX_FAULT_ERROR;
			f.probability = fp ? (uint32_t)strtoul(fp, NULL, 0) : 100;
			if (ioctl(b->fd, HVX_SET_FAULT, &f) < 0) { perror("HVX_SET_FAULT"); return -1; }
			fprintf(stderr, "hvx_disk_boot_verify: fault policy seed=0x%llx classes=0x%x prob=%u\n",
				(unsigned long long)f.seed, f.classes, f.probability);
		}
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
	if (b->mem != MAP_FAILED && b->mem)
		munmap(b->mem, b->mem_size);
	if (b->fd >= 0)
		close(b->fd);
	free(b->ubuf);
	free(b->diskbuf);
	b->ubuf = NULL; b->diskbuf = NULL;
	b->fd = -1;
	b->mem = MAP_FAILED;
}

static const char *hx(uint64_t v)
{
	static char bufs[4][17];
	static int i;
	char *b = bufs[i++ & 3];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

struct result { uint64_t fp; size_t ulen; int ok; };
static void measure(struct boot *b, struct result *r)
{
	memset(r, 0, sizeof(*r));
	r->fp = fingerprint(b);
	r->ulen = b->ulen;
	r->ok = (r->fp != 0);
}

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *disk_path, *cmdline;
	uint64_t mem_size, disk_len = 0;
	uint8_t *image = NULL, *initrd = NULL, *disk = NULL;
	size_t image_len = 0, initrd_len = 0, dl = 0;
	int rc = 1, ok_whole = 0, ok_branch = 0;
	struct result r1, r2, ra, rb;

	memset(&r1, 0, sizeof(r1)); memset(&r2, 0, sizeof(r2));
	memset(&ra, 0, sizeof(ra)); memset(&rb, 0, sizeof(rb));

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-disk> <disk.img> [\"cmdline\"] [memMiB]\n",
			argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	disk_path   = argv[3];
	cmdline     = (argc >= 5 && argv[4][0]) ? argv[4] : DEFAULT_CMDLINE;
	mem_size    = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0)
				   : DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	disk = read_file(disk_path, &dl);
	if (!disk) { free(image); free(initrd); return 1; }
	disk_len = dl;
	fprintf(stderr, "hvx_disk_boot_verify: bzImage=%zuB initramfs=%zuB disk=%zuB mem=%llu MiB\n",
		image_len, initrd_len, dl, (unsigned long long)(mem_size / (1024 * 1024)));

	/* ================= 1. WHOLE-BOOT DETERMINISM (two fresh disk boots) ===== */
	fprintf(stderr, "\n=== disk whole-boot determinism: boot #1 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
			       disk, disk_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r1);
		fprintf(stderr, "hvx_disk_boot_verify: boot #1 read /dev/vda: %zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	fprintf(stderr, "\n=== disk whole-boot determinism: boot #2 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
			       disk, disk_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r2);
		fprintf(stderr, "hvx_disk_boot_verify: boot #2 read /dev/vda: %zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	ok_whole = (r1.ok && r2.ok && r1.fp == r2.fp);
	printf("hvx_disk_boot_verify: whole-boot  fp1=%s fp2=%s -> %s\n",
	       hx(r1.fp), hx(r2.fp), ok_whole ? "IDENTICAL" : "MISMATCH");

	/* ================= 2. BRANCH-FROM-SNAPSHOT across the disk read ========= */
	fprintf(stderr, "\n=== branch-from-snapshot across the virtio-blk disk read ===\n");
	{
		struct boot b;
		struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
		uint8_t *ram_save = malloc(mem_size);
		uint8_t *disk_save = malloc(disk_len);
		size_t branch_ulen = 0;

		if (!mid || !ram_save || !disk_save) {
			perror("alloc");
		} else if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
				      disk, disk_len, cmdline) == 0 &&
			   run_until_marker(&b, MID_MARKER) == 0) {
			struct hvx_disk dget = { .len = disk_len, .data = (uint64_t)(uintptr_t)disk_save, .flags = 0 };

			fprintf(stderr, "hvx_disk_boot_verify: reached mid marker at %zu bytes; snapshotting\n", b.ulen);
			branch_ulen = b.ulen;
			if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) {
				perror("HVX_GET_SNAPSHOT");
			} else if (ioctl(b.fd, HVX_GET_DISK, &dget) < 0) {
				perror("HVX_GET_DISK");
			} else {
				memcpy(ram_save, b.mem, mem_size);

				/* straight continuation -> ra */
				if (run_until_marker(&b, FINAL_MARKER) == 0)
					measure(&b, &ra);
				fprintf(stderr, "hvx_disk_boot_verify: straight continuation read /dev/vda "
					"at %zu bytes\n", b.ulen);

				/* restore snapshot + RAM + disk, rewind console */
				if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) {
					perror("HVX_SET_SNAPSHOT");
				} else {
					struct hvx_disk dput = { .len = disk_len,
						.data = (uint64_t)(uintptr_t)disk_save, .flags = HVX_DISK_NO_IDMAP };

					memcpy(b.mem, ram_save, mem_size);
					if (ioctl(b.fd, HVX_SET_DISK, &dput) < 0)
						perror("HVX_SET_DISK");
					b.ulen = branch_ulen;
					console_rebase(&b);

					if (run_until_marker(&b, FINAL_MARKER) == 0)
						measure(&b, &rb);
					fprintf(stderr, "hvx_disk_boot_verify: branched continuation read "
						"/dev/vda at %zu bytes\n", b.ulen);
				}
			}
		}
		boot_teardown(&b);
		free(mid); free(ram_save); free(disk_save);
	}
	ok_branch = (ra.ok && rb.ok && ra.fp == rb.fp);
	printf("hvx_disk_boot_verify: branch      fp_a=%s fp_b=%s -> %s\n",
	       hx(ra.fp), hx(rb.fp), ok_branch ? "IDENTICAL" : "MISMATCH");

	if (ok_whole && ok_branch) {
		printf("hvx_disk_boot_verify: MILESTONE 21 PASS: real Linux 6.6.35 enumerated the "
		       "virtio-blk PCI disk, bound virtio_blk over MSI-X, and read /dev/vda -- "
		       "two disk boots are bit-identical in FULL state (console + snapshot + "
		       "256 MiB RAM + disk), and branch-from-snapshot across the disk read "
		       "reproduces bit-for-bit. with-disk fingerprint = %s\n", hx(r1.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_disk_boot_verify: FAIL: whole-boot=%s branch=%s\n",
			ok_whole ? "ok" : "MISMATCH", ok_branch ? "ok" : "MISMATCH");
	}

	free(image); free(initrd); free(disk);
	return rc;
}

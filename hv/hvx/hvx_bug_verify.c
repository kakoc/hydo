// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_bug_verify - milestone 25 (FLAGSHIP): find a REAL durability bug in a REAL
 * application via a SEEDED disk fault, and prove the failure reproduces bit-for-bit.
 * =============================================================================
 *
 * This is the payoff of the whole "be your own hypervisor" path: 24 milestones of
 * deterministic machinery (real-Linux boot, virtio-blk disk, the M22 seeded
 * disk-fault engine, snapshot/branch, the canonical fingerprint) finally do what
 * deterministic simulation testing is for -- find a bug through seeded fault injection, then
 * reproduce it perfectly.
 *
 * The software-under-test is `hydokv` (hvx/hydokv.c): a small raw-block, O_DIRECT
 * key/value store with ONE honest, documented durability bug -- it ACKS a write as
 * committed before the write is durably barriered ("ack-before-fsync"). Its /init
 * (build/initramfs-kv.cpio.gz) runs a workload that records a set of *committed*
 * writes, then an oracle checks the invariant READ-COMMITTED-DURABILITY (every key
 * the store said was committed reads back with its committed value, via O_DIRECT so
 * the read bypasses the page cache). It prints one of:
 *     HYDO-ORACLE: OK keys=<K> durable
 *     HYDO-ORACLE: VIOLATION key=<k> expected=<v> got=<g>
 * then the stop marker  hydo-flagship-marker: oracle complete.
 *
 * The four checks (the same four `hydo verify` / src/verify.rs runs, now on a REAL
 * bug in REAL software):
 *   (F1) NO FAULT: the oracle reports OK (the app is correct) AND the run is
 *        deterministic -- two fresh boots + a branch-from-snapshot are bit-identical
 *        in FULL state (console + snapshot + 256 MiB RAM + disk). New OK baseline.
 *   (F2) SEEDED DROP: a genuinely-committed key is LOST -- the oracle reports
 *        VIOLATION -- and the failing run (a) replays bit-for-bit (two fault-injected
 *        boots identical), and (b) branch-from-snapshots bit-for-bit (rewind to
 *        before hydokv runs, restore, finish -> identical failing state). New
 *        VIOLATION baseline.
 *   (F3) SEED-SENSITIVE: a DIFFERENT seed produces a DIFFERENT (still reproducible)
 *        failing state -- proving the fault is really consumed, so the determinism
 *        is real rather than the mere absence of variation.
 *   (F4) does NOT fire with faults disabled -- that IS F1 (OK), proving the fault is
 *        what surfaces the latent bug, not a broken app.
 *
 * Determinism / honesty: guest-userspace nondeterminism (getrandom/urandom/time) is
 * *proven* absent by fingerprint equality (F1/F2 replays are bit-identical), exactly
 * as M16 proved the whole boot is bit-identical. The DROP fault is a faithful
 * power-loss model (the device acks S_OK but the un-barriered sector is not
 * persisted -- "an un-synced write lost on power failure").
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_bug_verify hvx_bug_verify.c
 * Run   (inside L1):  sudo ./hvx_bug_verify /tmp/bzImage /tmp/initramfs-kv.cpio.gz /tmp/kv-disk.img
 */

#include "hvx_boot.h"

/* Printed by hydokv right after the oracle -- the deterministic stop point. */
#define FINAL_MARKER "hydo-flagship-marker: oracle complete"
/* Deep-in-kernel-init marker, BEFORE the virtio modules load and hydokv runs.
 * Branching here forces the snapshot to reproduce the whole KV workload + the
 * faulted write, so the branch is a real test of fault-schedule capture. */
#define MID_MARKER "Trying to unpack rootfs image as initramfs"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define MAX_RUN_ITERS 60000000ULL

/* Fault policy for the demonstration. prob=25% across hydokv's 48 writes loses
 * ~12 committed keys (P(zero losses) ~ 1e-6, so a violation is deterministic for
 * essentially any seed); the exact set -- and the first lost key the oracle reports
 * -- is a pure function of the seed. */
#define FAULT_PROB   25u
#define SEED_A       0xB5EEDULL
#define SEED_B       0x1CE5EEDULL

/* ---- canonical fingerprint (FNV-1a; matches hvx_disk_boot_verify.c) ---- */
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

/* Extract the HYDO-ORACLE verdict line from the reconstructed console. Returns 1
 * if found, copying the line (without trailing newline) into out. */
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
static int verdict_is_ok(const char *v)      { return strstr(v, "OK") != NULL; }
static int verdict_is_violation(const char *v) { return strstr(v, "VIOLATION") != NULL; }

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
			fprintf(stderr, "hvx_bug_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 512) == 0)
				fprintf(stderr, "[hvx_bug_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 240 ? b->ulen - 240 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_bug_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_bug_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_bug_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Canonical full-state fingerprint: console + snapshot (meaningful fields incl. the
 * virtio-blk + MSI-X device state AND the seeded fault policy/PRNG state) + all
 * guest RAM + the backing disk image. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	struct hvx_disk dk;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size, dlen = b->disk_len;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }

	h = fnv(h, "hvx-bug-state-v1", 16);
	/* console (includes the HYDO-ORACLE verdict line -- a different lost key => a
	 * different console => a different fingerprint, so the verdict IS hashed) */
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
	/* seeded disk-fault policy + PRNG state (M22) -- part of the deterministic state */
	h = fnv(h, &s->fault_seed, sizeof(s->fault_seed));
	h = fnv(h, &s->fault_classes, sizeof(s->fault_classes));
	h = fnv(h, &s->fault_probability, sizeof(s->fault_probability));
	h = fnv(h, &s->fault_req_seq, sizeof(s->fault_req_seq));
	h = fnv(h, &s->fault_count, sizeof(s->fault_count));
	/* all guest RAM */
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);
	/* the backing disk image (the ground truth of what was actually persisted) */
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
	dk.flags = HVX_DISK_NO_IDMAP;	/* real Linux builds its own page tables + ioremaps MMIO */
	if (ioctl(b->fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return -1; }
	return 0;
}

static int install_fault(struct boot *b)
{
	struct hvx_fault f;

	if (b->fault_classes == 0)
		return 0;		/* faults disabled: device is byte-identical to M21 */
	memset(&f, 0, sizeof(f));
	f.seed = b->fault_seed;
	f.classes = b->fault_classes;
	f.probability = b->fault_prob;
	if (ioctl(b->fd, HVX_SET_FAULT, &f) < 0) { perror("HVX_SET_FAULT"); return -1; }
	return 0;
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
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->mem_size = mem_size;
	b->disk = disk;
	b->disk_len = disk_len;
	b->fault_seed = fault_seed;
	b->fault_classes = fault_classes;
	b->fault_prob = fault_prob;
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
	if (install_fault(b) < 0) return -1;

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
	static char bufs[6][17];
	static int i;
	char *b = bufs[i++ % 6];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

/* A full run to the oracle marker: boot, run, fingerprint, capture the verdict. */
struct result { uint64_t fp; int ok; char verdict[160]; };

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
		oracle_verdict(&b, r->verdict, sizeof(r->verdict));
		r->ok = (r->fp != 0);
		rc = 0;
	}
	boot_teardown(&b);
	return rc;
}

/* Branch-from-snapshot: boot to MID_MARKER (before hydokv runs), snapshot RAM +
 * disk + state, continue straight to the oracle -> ra; restore the snapshot,
 * continue again -> rb. ra.fp == rb.fp proves the fault schedule + disk + state are
 * all captured, so the failing run rewinds and reproduces bit-for-bit. */
static int run_branch(uint64_t mem_size, const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const uint8_t *disk, uint64_t disk_len, const char *cmdline,
		      uint64_t seed, uint32_t classes, uint32_t prob,
		      struct result *ra, struct result *rb)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
	uint8_t *ram_save = malloc(mem_size);
	uint8_t *disk_save = malloc(disk_len);
	size_t branch_ulen = 0;
	int rc = -1;

	memset(ra, 0, sizeof(*ra));
	memset(rb, 0, sizeof(*rb));
	if (!mid || !ram_save || !disk_save) { perror("alloc"); goto done; }

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
		       disk, disk_len, cmdline, seed, classes, prob) != 0 ||
	    run_until_marker(&b, MID_MARKER) != 0)
		goto teardown;

	{
		struct hvx_disk dget = { .len = disk_len, .data = (uint64_t)(uintptr_t)disk_save, .flags = 0 };

		fprintf(stderr, "hvx_bug_verify: reached mid marker at %zu bytes; snapshotting\n", b.ulen);
		branch_ulen = b.ulen;
		if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
		if (ioctl(b.fd, HVX_GET_DISK, &dget) < 0) { perror("HVX_GET_DISK"); goto teardown; }
		memcpy(ram_save, b.mem, mem_size);

		/* straight continuation -> ra */
		if (run_until_marker(&b, FINAL_MARKER) == 0) {
			ra->fp = fingerprint(&b);
			oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
			ra->ok = (ra->fp != 0);
		}

		/* restore snapshot + RAM + disk, rewind console, continue -> rb */
		if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
		{
			struct hvx_disk dput = { .len = disk_len,
				.data = (uint64_t)(uintptr_t)disk_save, .flags = HVX_DISK_NO_IDMAP };
			memcpy(b.mem, ram_save, mem_size);
			if (ioctl(b.fd, HVX_SET_DISK, &dput) < 0) { perror("HVX_SET_DISK"); goto teardown; }
		}
		b.ulen = branch_ulen;
		console_rebase(&b);
		if (run_until_marker(&b, FINAL_MARKER) == 0) {
			rb->fp = fingerprint(&b);
			oracle_verdict(&b, rb->verdict, sizeof(rb->verdict));
			rb->ok = (rb->fp != 0);
		}
	}
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(mid); free(ram_save); free(disk_save);
	return rc;
}

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *disk_path, *cmdline;
	uint64_t mem_size, disk_len = 0;
	uint8_t *image = NULL, *initrd = NULL, *disk = NULL;
	size_t image_len = 0, initrd_len = 0, dl = 0;
	int rc = 1;
	struct result nf1, nf2, nfa, nfb;		/* F1: no-fault */
	struct result fa1, fa2, faa, fab;		/* F2: seeded DROP */
	struct result fb1;				/* F3: different seed */
	int ok_f1_repro, ok_f1_branch, ok_f1_verdict;
	int ok_f2_repro, ok_f2_branch, ok_f2_verdict;
	int ok_seed;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-kv> <kv-disk.img> [\"cmdline\"] [memMiB]\n",
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
	fprintf(stderr, "hvx_bug_verify: bzImage=%zuB initramfs-kv=%zuB kv-disk=%zuB mem=%llu MiB\n",
		image_len, initrd_len, dl, (unsigned long long)(mem_size / (1024 * 1024)));

	/* =================== F1: NO FAULT (app correct + deterministic) ========= */
	fprintf(stderr, "\n=== F1: NO FAULT -- boot #1 (establish the OK baseline) ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     0, 0, 0, &nf1);
	fprintf(stderr, "\n=== F1: NO FAULT -- boot #2 (replay) ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     0, 0, 0, &nf2);
	fprintf(stderr, "\n=== F1: NO FAULT -- branch-from-snapshot ===\n");
	run_branch(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		   0, 0, 0, &nfa, &nfb);
	ok_f1_verdict = nf1.ok && verdict_is_ok(nf1.verdict);
	ok_f1_repro   = nf1.ok && nf2.ok && nf1.fp == nf2.fp;
	ok_f1_branch  = nfa.ok && nfb.ok && nfa.fp == nfb.fp && nfa.fp == nf1.fp;
	printf("F1 NO-FAULT   verdict=|%s|\n", nf1.verdict);
	printf("F1 NO-FAULT   fp1=%s fp2=%s -> replay %s ; branch fp_a=%s fp_b=%s -> %s\n",
	       hx(nf1.fp), hx(nf2.fp), ok_f1_repro ? "IDENTICAL" : "MISMATCH",
	       hx(nfa.fp), hx(nfb.fp), ok_f1_branch ? "IDENTICAL" : "MISMATCH");

	/* =================== F2: SEEDED DROP (the demonstration) =============== */
	fprintf(stderr, "\n=== F2: SEEDED DROP seed=0x%llx prob=%u%% -- boot #1 (the failing run) ===\n",
		(unsigned long long)SEED_A, FAULT_PROB);
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_A, HVX_FAULT_DROP, FAULT_PROB, &fa1);
	fprintf(stderr, "\n=== F2: SEEDED DROP -- boot #2 (replay bit-for-bit) ===\n");
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_A, HVX_FAULT_DROP, FAULT_PROB, &fa2);
	fprintf(stderr, "\n=== F2: SEEDED DROP -- branch-from-snapshot across the faulted writes ===\n");
	run_branch(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		   SEED_A, HVX_FAULT_DROP, FAULT_PROB, &faa, &fab);
	ok_f2_verdict = fa1.ok && verdict_is_violation(fa1.verdict);
	ok_f2_repro   = fa1.ok && fa2.ok && fa1.fp == fa2.fp;
	ok_f2_branch  = faa.ok && fab.ok && faa.fp == fab.fp && faa.fp == fa1.fp;
	printf("F2 DROP       verdict=|%s|\n", fa1.verdict);
	printf("F2 DROP       fp1=%s fp2=%s -> replay %s ; branch fp_a=%s fp_b=%s -> %s\n",
	       hx(fa1.fp), hx(fa2.fp), ok_f2_repro ? "IDENTICAL" : "MISMATCH",
	       hx(faa.fp), hx(fab.fp), ok_f2_branch ? "IDENTICAL" : "MISMATCH");

	/* =================== F3: SEED-SENSITIVITY ============================== */
	fprintf(stderr, "\n=== F3: DIFFERENT SEED seed=0x%llx (a different reproducible failure) ===\n",
		(unsigned long long)SEED_B);
	run_scenario(mem_size, image, image_len, initrd, initrd_len, disk, disk_len, cmdline,
		     SEED_B, HVX_FAULT_DROP, FAULT_PROB, &fb1);
	ok_seed = fb1.ok && verdict_is_violation(fb1.verdict) && fb1.fp != fa1.fp;
	printf("F3 SEED-B     verdict=|%s|\n", fb1.verdict);
	printf("F3 SEED-B     fp=%s (seed-A fp=%s) -> %s\n",
	       hx(fb1.fp), hx(fa1.fp), ok_seed ? "DIFFERENT+REPRODUCIBLE" : "UNEXPECTED");

	/* =================== verdict ========================================== */
	if (ok_f1_verdict && ok_f1_repro && ok_f1_branch &&
	    ok_f2_verdict && ok_f2_repro && ok_f2_branch && ok_seed) {
		printf("hvx_bug_verify: MILESTONE 25 PASS: a REAL durability bug in a REAL app "
		       "(hydokv: ack-before-fsync) found by a SEEDED disk DROP fault; without the "
		       "fault the app is correct (OK) and deterministic (baseline %s); under seed "
		       "0x%llx a committed key is LOST (VIOLATION, baseline %s) and the failing run "
		       "REPLAYS + BRANCHES bit-for-bit; a different seed loses a different key (%s). "
		       "The four hydo-verify checks, now on a real bug in real software.\n",
		       hx(nf1.fp), (unsigned long long)SEED_A, hx(fa1.fp), hx(fb1.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_bug_verify: FAIL: F1[verdict=%d repro=%d branch=%d] "
			"F2[verdict=%d repro=%d branch=%d] seed=%d\n",
			ok_f1_verdict, ok_f1_repro, ok_f1_branch,
			ok_f2_verdict, ok_f2_repro, ok_f2_branch, ok_seed);
	}

	free(image); free(initrd); free(disk);
	return rc;
}

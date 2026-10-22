// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_boot_verify - milestone 16 (Phase 7): a rigorous WHOLE-BOOT determinism
 * result for a REAL Linux kernel booted as an L2 guest under hvx, plus
 * branch-from-snapshot applied to that running OS.
 * =============================================================================
 *
 * This is the formal counterpart to src/detlinux.rs (hydo's KVM boot-verify
 * harness) and mirrors src/verify.rs / src/state.rs's model: a CANONICAL
 * fingerprint over meaningful state in a fixed order (never raw struct bytes),
 * then equality checks that define "deterministic".
 *
 * It uses the shared bzImage loader (hvx_boot.h) -- the exact byte-for-byte
 * recipe hvx_linux.c drives -- so both clients boot the identical guest.
 *
 * TWO results:
 *
 *   1. WHOLE-BOOT DETERMINISM. Boot the bzImage+initramfs TWICE from scratch, in
 *      FAST free-run (HVX_RUN, COUNTER-mode virtual TSC), each to a well-defined
 *      stopping point: run/drain HVX_GET_CONSOLE until the console contains a
 *      fixed marker (FINAL_MARKER, printed by the initramfs /init just before it
 *      exec's the shell), then stop. Compute a fingerprint over the FULL end
 *      state -- the reconstructed console bytes + the HVX_GET_SNAPSHOT struct
 *      (fielded) + all 256 MiB of guest RAM (via mmap) -- and assert the two
 *      boots are byte-identical. This proves hvx boots Linux deterministically
 *      all the way THROUGH the RDTSC calibration region (native_calibrate_cpu_early)
 *      that stopped hydo under KVM.
 *
 *   2. BRANCH-FROM-SNAPSHOT on the real boot. Boot to an earlier marker
 *      (MID_MARKER, deep in kernel init with the timer armed and the initramfs
 *      about to unpack), snapshot (HVX_GET_SNAPSHOT + memcpy the mmap'd RAM),
 *      continue to FINAL_MARKER -> fingerprint A. Then RESTORE (HVX_SET_SNAPSHOT +
 *      memcpy RAM back), continue again -> fingerprint B. Assert A == B. If the
 *      snapshot fails to capture some device/CPU state, B diverges; the fix is to
 *      extend struct hvx_snapshot + the GET/SET handlers + this fingerprint.
 *      (Milestone 16 found the 16550 UART register latch was the missing piece.)
 *
 * DETERMINISTIC STOP: HVX_RUN only returns during active boot at a THROTTLE
 * boundary, which M16 made a deterministic function of the guest (retired exits
 * only; host external interrupts no longer jitter it). So "stop at the first
 * boundary where the console contains marker X" resumes at a bit-identical guest
 * state every time -- exactly what a full-state fingerprint needs.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_boot_verify hvx_boot_verify.c
 * Run   (inside L1):  sudo ./hvx_boot_verify /tmp/bzImage /tmp/initramfs.cpio.gz
 */

#include "hvx_boot.h"

/* Marker printed by the initramfs /init (build-linux.sh) just before exec'ing
 * the shell -- the deepest deterministic point of the boot (past the RDTSC
 * calibration region, the timer, initramfs mount, and userspace init). */
#define FINAL_MARKER "hydo-boot-marker: launching shell on ttyS0"

/* An earlier, deep-in-kernel-init marker: the timer is armed (jiffies clocksource
 * active) and the initramfs is about to be unpacked. Branching here forces the
 * snapshot to carry live timer/LAPIC/UART/injection state, then reproduce a big
 * span of execution (initramfs unpack + free memory + run /init + userspace). */
#define MID_MARKER "Trying to unpack rootfs image as initramfs"

/* Free-run COUNTER-mode virtual TSC, identical seed/step to hvx_linux.c. */
#define BOOT_TSC_STEP 0x10000ULL

/* Reconstructed-console buffer. The module caps its own console at HVX_CONSOLE_SZ
 * (64 KiB); a full boot is ~16 KiB and the branch test reconstructs at most one
 * timeline's worth, so 1 MiB is comfortably large. */
#define UBUF_CAP (1UL << 20)

/* Safety bound on HVX_RUN iterations per marker phase (a healthy boot to the
 * shell takes a few thousand THROTTLEs). */
#define MAX_RUN_ITERS 50000000ULL

/* ---- canonical fingerprint (FNV-1a; matches hvx_verify.c's discipline) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) {
		h ^= b[i];
		h *= 0x100000001b3ULL;
	}
	return h;
}

/* Per-boot context: the VM fd, its mmap'd RAM, and a userspace reconstruction of
 * the guest console (so a branch can rewind the console tracking independently of
 * the module's ever-growing internal buffer). */
struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *ubuf;		/* reconstructed console (this timeline) */
	size_t ulen;		/* length of the reconstruction */
	size_t drained;		/* module console_len already consumed */
};

/* Naive substring search over the reconstructed console. */
static long mem_find(const uint8_t *hay, size_t hlen, const char *needle)
{
	size_t nlen = strlen(needle);
	size_t i;

	if (nlen == 0 || hlen < nlen)
		return -1;
	for (i = 0; i + nlen <= hlen; i++)
		if (memcmp(hay + i, needle, nlen) == 0)
			return (long)i;
	return -1;
}

/* Pull any newly-trapped console bytes from the module into b->ubuf. The module
 * accumulates guest UART TX into its own buffer (never reset by restore), so we
 * track how many bytes we have consumed in b->drained and append only new ones. */
static struct hvx_console con_scratch;	/* 64 KiB+, keep off the stack */
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
		add = UBUF_CAP - b->ulen;	/* never overflow (should not happen) */
	memcpy(b->ubuf + b->ulen, con_scratch.buf + b->drained, add);
	b->ulen += add;
	b->drained = modlen;
}

/* Set the drained watermark to the module's current console_len WITHOUT copying
 * anything -- used after a restore so the branched continuation appends only the
 * bytes it newly produces (ignoring the straight run's output still in the module
 * buffer). */
static void console_rebase(struct boot *b)
{
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0) {
		/* never ignore a failed read: it would move the stop point (M58) */
		perror("HVX_GET_CONSOLE");
		exit(1);
	}
	b->drained = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

/* Free-run the guest, draining the console each HVX_RUN, until the reconstructed
 * console contains `marker`. Returns 0 on success; -1 if a terminal exit is hit
 * before the marker (or the iteration cap trips). */
static int run_until_marker(struct boot *b, const char *marker)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;

	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_boot_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;	/* reached the deterministic stop point */

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 512) == 0)
				fprintf(stderr, "[hvx_boot_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		/* A terminal / unhandled exit before the marker: report and fail. */
		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 200 ? b->ulen - 200 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_boot_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_boot_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_boot_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Canonical full-state fingerprint: reconstructed console + snapshot (meaningful
 * fields, fixed order, no raw struct/padding) + all guest RAM. Mirrors
 * hvx_verify.c / src/state.rs. Returns 0 on ioctl failure. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	uint64_t h = 0xcbf29ce484222325ULL;	/* FNV offset basis */
	uint64_t clen = b->ulen, rlen = b->mem_size;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) {
		perror("HVX_GET_SNAPSHOT");
		free(s);
		return 0;
	}

	if (getenv("HVX_FP_DUMP")) {	/* diagnostic: dump console / snapshot / RAM of this boot */
		static int seq; char path[256]; FILE *f;
		snprintf(path, sizeof(path), "%s/boot%d.con", getenv("HVX_FP_DUMP"), seq);
		if ((f = fopen(path, "w"))) { fwrite(b->ubuf, 1, b->ulen, f); fclose(f); }
		snprintf(path, sizeof(path), "%s/boot%d.snap", getenv("HVX_FP_DUMP"), seq);
		if ((f = fopen(path, "w"))) { fwrite(s, 1, sizeof(*s), f); fclose(f); }
		snprintf(path, sizeof(path), "%s/boot%d.ram", getenv("HVX_FP_DUMP"), seq);
		if ((f = fopen(path, "w"))) { fwrite(b->mem, 1, b->mem_size, f); fclose(f); }
		seq++;
	}
	h = fnv(h, "hvx-boot-state-v1", 17);
	/* console (length-prefixed) */
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
	/* curated MSR store + privileged (system) register state */
	h = fnv(h, s->sw_msr, sizeof(s->sw_msr));
	h = fnv(h, &s->sregs, sizeof(s->sregs));	/* padding-free (see hvx_abi.h) */
	/* APIC / IOAPIC / PIC device model */
	h = fnv(h, &s->lapic_apic_base, sizeof(s->lapic_apic_base));
	h = fnv(h, &s->lapic_tsc_deadline, sizeof(s->lapic_tsc_deadline));
	h = fnv(h, s->lapic_reg, sizeof(s->lapic_reg));
	h = fnv(h, &s->lapic_in_service, sizeof(s->lapic_in_service));
	h = fnv(h, &s->ioapic_ioregsel, sizeof(s->ioapic_ioregsel));
	h = fnv(h, &s->ioapic_id, sizeof(s->ioapic_id));
	h = fnv(h, s->ioapic_redir, sizeof(s->ioapic_redir));
	h = fnv(h, s->pic_mask, sizeof(s->pic_mask));
	/* 16550 UART register latch (milestone 16 -- the branch-completeness fix) */
	h = fnv(h, &s->uart_ier, 1);
	h = fnv(h, &s->uart_lcr, 1);
	h = fnv(h, &s->uart_mcr, 1);
	h = fnv(h, &s->uart_fcr, 1);
	h = fnv(h, &s->uart_scr, 1);
	h = fnv(h, &s->uart_dll, 1);
	h = fnv(h, &s->uart_dlm, 1);
	/* all guest RAM (length-prefixed) */
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);

	free(s);
	return h;
}

/* Bring a fresh VM up to the kernel entry point (long mode, boot env built),
 * ready to free-run. Mirrors hvx_linux.c's setup. Returns 0 on success. */
static int boot_setup(struct boot *b, uint64_t mem_size,
		      const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const char *cmdline)
{
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0,
			       .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->mem_size = mem_size;
	b->ubuf = malloc(UBUF_CAP);
	if (!b->ubuf) { perror("malloc ubuf"); return -1; }

	b->fd = open("/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open /dev/hvx"); return -1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	if (getenv("HVX_PMU_CLOCK"))		/* diagnostic: PMU branch clock on (period from HVX_PMU_PERIOD) */
		cfg.flags |= HVX_VM_PMU_CLOCK;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }
	if (getenv("HVX_PMU_CLOCK")) {
		struct hvx_pmu pm; memset(&pm, 0, sizeof(pm));
		pm.period = getenv("HVX_PMU_PERIOD") ? strtoull(getenv("HVX_PMU_PERIOD"), NULL, 0) : 0;
		pm.tick_tsc = getenv("HVX_PMU_TICK") ? strtoull(getenv("HVX_PMU_TICK"), NULL, 0) : 0;
		ioctl(b->fd, HVX_SET_PMU, &pm);
	}

	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap"); return -1; }

	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }

	build_page_tables_and_gdt(b->mem, mem_size);
	if (load_bzimage(b->mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0)
		return -1;

	fill_sregs(&sregs);
	if (ioctl(b->fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return -1; }

	memset(&regs, 0, sizeof(regs));
	regs.rip    = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi    = BOOT_PARAMS_ADDR;
	regs.rsp    = STACK_TOP;
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
	b->ubuf = NULL;
	b->fd = -1;
	b->mem = MAP_FAILED;
}

/* Rotating buffers so several hx() calls in one printf() render distinct values. */
static const char *hx(uint64_t v)
{
	static char bufs[4][17];
	static int i;
	char *b = bufs[i++ & 3];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

/* Component fingerprints so a mismatch can be localized to console / RAM / the
 * snapshot rather than an opaque 64-bit blob. */
static uint64_t fnv_console(struct boot *b)
{
	uint64_t h = 0xcbf29ce484222325ULL, clen = b->ulen;
	h = fnv(h, &clen, sizeof(clen));
	return fnv(h, b->ubuf, b->ulen);
}
static uint64_t fnv_ram(struct boot *b)
{
	uint64_t h = 0xcbf29ce484222325ULL, rlen = b->mem_size;
	h = fnv(h, &rlen, sizeof(rlen));
	return fnv(h, b->mem, b->mem_size);
}

/* A single boot's measured end state, for localized comparison. */
struct result {
	uint64_t fp, h_con, h_ram;
	struct hvx_snapshot snap;
	size_t ulen;
	int ok;
};

static void measure(struct boot *b, struct result *r)
{
	memset(r, 0, sizeof(*r));
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, &r->snap) < 0) { perror("HVX_GET_SNAPSHOT"); return; }
	r->fp = fingerprint(b);
	if (getenv("HVX_PMU_CLOCK")) {
		struct hvx_pmu pm;
		if (ioctl(b->fd, HVX_GET_PMU, &pm) == 0)
			fprintf(stderr, "hvx_boot_verify: PMU branches=%llu ticks=%llu overshoots=%llu steps=%llu land=%016llx\n",
				(unsigned long long)pm.total, (unsigned long long)pm.ticks,
				(unsigned long long)pm.overshoots, (unsigned long long)pm.steps,
				(unsigned long long)pm.land_hash);
	}
	r->h_con = fnv_console(b);
	r->h_ram = fnv_ram(b);
	r->ulen = b->ulen;
	r->ok = 1;
}

/* Field-by-field snapshot diff (fixed order), so a branch-completeness gap names
 * the exact field the snapshot failed to carry. */
static void snap_diff(const struct hvx_snapshot *a, const struct hvx_snapshot *b)
{
#define D64(f) do { if (a->f != b->f) printf("   snap." #f ": %016" PRIx64 " vs %016" PRIx64 "\n", \
	(uint64_t)a->f, (uint64_t)b->f); } while (0)
#define DMEM(f) do { if (memcmp(&a->f, &b->f, sizeof(a->f)) != 0) printf("   snap." #f ": differs\n"); } while (0)
	DMEM(regs);
	D64(tsc); D64(tsc_step); D64(tsc_aux); D64(tsc_mode);
	D64(icount); D64(rdtsc_count);
	D64(inject_at_icount); D64(inject_vector); D64(inject_valid);
	DMEM(sw_msr); DMEM(sregs);
	D64(lapic_apic_base); D64(lapic_tsc_deadline);
	DMEM(lapic_reg); D64(lapic_in_service);
	D64(ioapic_ioregsel); D64(ioapic_id); DMEM(ioapic_redir);
	DMEM(pic_mask);
	D64(uart_ier); D64(uart_lcr); D64(uart_mcr); D64(uart_fcr);
	D64(uart_scr); D64(uart_dll); D64(uart_dlm);
#undef D64
#undef DMEM
}

/* Report which top-level component (console / RAM / snapshot) diverged. */
static void localize(const char *tag, const struct result *a, const struct result *b)
{
	printf("hvx_boot_verify: %s divergence localization:\n", tag);
	printf("   console: %s vs %s -> %s (len %zu vs %zu)\n",
	       hx(a->h_con), hx(b->h_con), a->h_con == b->h_con ? "same" : "DIFFERS",
	       a->ulen, b->ulen);
	printf("   ram    : %s vs %s -> %s\n",
	       hx(a->h_ram), hx(b->h_ram), a->h_ram == b->h_ram ? "same" : "DIFFERS");
	printf("   snapshot fields that differ:\n");
	snap_diff(&a->snap, &b->snap);
}

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *cmdline;
	uint64_t mem_size;
	uint8_t *image = NULL, *initrd = NULL;
	size_t image_len = 0, initrd_len = 0;
	int rc = 1, ok_whole = 0, ok_branch = 0;
	struct result r1, r2, ra, rb;

	memset(&r1, 0, sizeof(r1)); memset(&r2, 0, sizeof(r2));
	memset(&ra, 0, sizeof(ra)); memset(&rb, 0, sizeof(rb));

	if (argc < 3) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs> [\"cmdline\"] [memMiB]\n",
			argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	cmdline     = (argc >= 4 && argv[3][0]) ? argv[3] : DEFAULT_CMDLINE;
	mem_size    = ((argc >= 5) ? (uint64_t)strtoull(argv[4], NULL, 0)
				   : DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	fprintf(stderr, "hvx_boot_verify: bzImage=%zuB initramfs=%zuB mem=%llu MiB\n",
		image_len, initrd_len, (unsigned long long)(mem_size / (1024 * 1024)));

	/* ================= 1. WHOLE-BOOT DETERMINISM (two fresh boots) ========= */
	fprintf(stderr, "\n=== whole-boot determinism: boot #1 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r1);
		fprintf(stderr, "hvx_boot_verify: boot #1 reached shell marker: "
			"%zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	fprintf(stderr, "\n=== whole-boot determinism: boot #2 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r2);
		fprintf(stderr, "hvx_boot_verify: boot #2 reached shell marker: "
			"%zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	ok_whole = (r1.ok && r2.ok && r1.fp == r2.fp);
	printf("hvx_boot_verify: whole-boot  fp1=%s fp2=%s -> %s\n",
	       hx(r1.fp), hx(r2.fp), ok_whole ? "IDENTICAL" : "MISMATCH");
	if (r1.ok && r2.ok && !ok_whole)
		localize("whole-boot", &r1, &r2);

	/* ================= 2. BRANCH-FROM-SNAPSHOT on the real boot =========== */
	fprintf(stderr, "\n=== branch-from-snapshot on the real Linux boot ===\n");
	{
		struct boot b;
		struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
		uint8_t *ram_save = malloc(mem_size);
		size_t branch_ulen = 0;

		if (!mid || !ram_save) {
			perror("alloc");
		} else if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len,
				      cmdline) == 0 &&
			   run_until_marker(&b, MID_MARKER) == 0) {
			fprintf(stderr, "hvx_boot_verify: reached mid marker at %zu console "
				"bytes; snapshotting\n", b.ulen);
			branch_ulen = b.ulen;
			if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) {
				perror("HVX_GET_SNAPSHOT");
			} else {
				memcpy(ram_save, b.mem, mem_size);

				/* straight continuation -> ra */
				if (run_until_marker(&b, FINAL_MARKER) == 0)
					measure(&b, &ra);
				fprintf(stderr, "hvx_boot_verify: straight continuation reached "
					"shell at %zu console bytes\n", b.ulen);

				/* restore the mid snapshot + guest RAM, rewind console tracking */
				if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) {
					perror("HVX_SET_SNAPSHOT");
				} else {
					memcpy(b.mem, ram_save, mem_size);
					b.ulen = branch_ulen;	/* console back to the branch point */
					console_rebase(&b);	/* ignore the straight run's output */

					/* branched continuation -> rb */
					if (run_until_marker(&b, FINAL_MARKER) == 0)
						measure(&b, &rb);
					fprintf(stderr, "hvx_boot_verify: branched continuation "
						"reached shell at %zu console bytes\n", b.ulen);
				}
			}
		}
		boot_teardown(&b);
		free(mid);
		free(ram_save);
	}
	ok_branch = (ra.ok && rb.ok && ra.fp == rb.fp);
	printf("hvx_boot_verify: branch      fp_a=%s fp_b=%s -> %s\n",
	       hx(ra.fp), hx(rb.fp), ok_branch ? "IDENTICAL" : "MISMATCH");
	if (ra.ok && rb.ok && !ok_branch)
		localize("branch", &ra, &rb);

	/* ================= verdict ============================================ */
	if (ok_whole && ok_branch) {
		printf("hvx_boot_verify: MILESTONE 16 PASS: hvx boots real Linux 6.6.35 "
		       "deterministically to the initramfs shell -- two boots are "
		       "bit-identical in FULL state (console + snapshot + 256 MiB RAM), "
		       "PAST the RDTSC calibration region (native_calibrate_cpu_early) "
		       "that stopped hydo under KVM -- and branch-from-snapshot on the "
		       "running OS reproduces the boot bit-for-bit.\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_boot_verify: FAIL: whole-boot=%s branch=%s\n",
			ok_whole ? "ok" : "MISMATCH", ok_branch ? "ok" : "MISMATCH");
	}

	free(image);
	free(initrd);
	return rc;
}

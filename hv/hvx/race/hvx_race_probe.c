// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_race_probe - boot the calibration-race guest (race/build-race.sh) once and
 * report its result. The boot path, console handling and full-state fingerprint
 * are copied from hvx_boot_verify.c.
 *
 * Timing knobs (environment): HVX_PMU_CLOCK (any value: PMU branch clock on),
 * HVX_PMU_PERIOD, HVX_PMU_TICK, HVX_TSC_STEP.
 *
 * Usage: hvx_race_probe <bzImage> <initramfs-race> ["cmdline"] [memMiB]
 * Prints one line: "<HYDO-RACE line> | fp=<full-state fingerprint>".
 */

#include "../hvx_boot.h"

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

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
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
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
	b->drained = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

/* Free-run the guest, draining the console each HVX_RUN, until the reconstructed
 * console contains `marker`. Returns 0 on success; -1 if a terminal exit is hit
 * before the marker (or the iteration cap trips). */
/*
 * Every run call returns after at most g_budget deterministic events (HVX_RUNC max_insns),
 * and a run stops at the first return after its marker appears. The return points are then
 * the same in a full run and in a run restored from a snapshot taken at a return point, so
 * the two end at the same instruction and their fingerprints are comparable. (With plain
 * HVX_RUN the return cadence restarts at every call, and a snapshot shifts every later
 * stop by a few instructions.)
 */
static uint64_t g_budget = 4096;

static int run_until_marker(struct boot *b, const char *marker)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;

	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		struct hvx_run_ctl c;

		memset(&c, 0, sizeof(c));
		c.max_insns = g_budget;
		if (ioctl(b->fd, HVX_RUNC, &c) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_race_probe: HVX_RUNC: %s\n", strerror(errno));
			return -1;
		}
		memset(&ex, 0, sizeof(ex));
		ex.reason = c.reason;
		ex.qualification = c.qualification;
		ex.guest_phys = c.guest_phys;
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;	/* reached the deterministic stop point */

		if (ex.reason == HVX_EXIT_THROTTLE || ex.reason == HVX_EXIT_STEP) {
			if ((++throttles % 512) == 0)
				fprintf(stderr, "[hvx_race_probe: %llu throttles, %zu console "
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
			fprintf(stderr, "hvx_race_probe: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_race_probe: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_race_probe: iteration cap reached before marker \"%s\"\n", marker);
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

	if (getenv("HVX_TSC_STEP"))
		tsc.step = strtoull(getenv("HVX_TSC_STEP"), NULL, 0);

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->mem_size = mem_size;
	b->ubuf = malloc(UBUF_CAP);
	if (!b->ubuf) { perror("malloc ubuf"); return -1; }

	b->fd = open(getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open hvx device"); return -1; }

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
	if (getenv("HVX_TJ_SEED") || getenv("HVX_TJ_EXPLICIT")) {
		/* seeded tick jitter: HVX_TJ_SEED/_MAX/_PERMILLE/_LO/_HI, or an explicit
		 * list HVX_TJ_EXPLICIT="tick:delay,tick:delay" */
		static struct hvx_tjitter tj;
		const char *e;

		memset(&tj, 0, sizeof(tj));
		if ((e = getenv("HVX_TJ_SEED")))     tj.seed = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_TJ_MAX")))      tj.max_delay = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_TJ_PERMILLE"))) tj.permille = (uint32_t)strtoul(e, NULL, 0);
		if ((e = getenv("HVX_TJ_LO")))       tj.tick_lo = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_TJ_HI")))       tj.tick_hi = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_TJ_EXPLICIT"))) {
			char *end;
			while (*e && tj.n_explicit < HVX_TJ_MAX_EXPLICIT) {
				tj.explicit_[tj.n_explicit].tick = strtoull(e, &end, 0);
				if (*end != ':') break;
				tj.explicit_[tj.n_explicit].delay = strtoull(end + 1, &end, 0);
				tj.n_explicit++;
				e = (*end == ',') ? end + 1 : end;
			}
		}
		if (ioctl(b->fd, HVX_SET_TJITTER, &tj) < 0) { perror("HVX_SET_TJITTER"); return -1; }
	}
	if (getenv("HVX_XT_SEED") || getenv("HVX_XT_EXPLICIT")) {
		/* inserted ticks (milestone 49): HVX_XT_SEED/_RATE/_PERMILLE/_LO/_HI/_TSC, or an
		 * explicit list of absolute guest branch counts HVX_XT_EXPLICIT="b,b,..." */
		static struct hvx_xtick xt;
		const char *e;

		memset(&xt, 0, sizeof(xt));
		if ((e = getenv("HVX_XT_SEED")))     xt.seed = strtoull(e, NULL, 0);
		xt.rate = (e = getenv("HVX_XT_RATE")) ? (uint32_t)strtoul(e, NULL, 0) : 4;
		xt.permille = (e = getenv("HVX_XT_PERMILLE")) ? (uint32_t)strtoul(e, NULL, 0) : 500;
		if ((e = getenv("HVX_XT_LO")))       xt.lo = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_XT_HI")))       xt.hi = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_XT_TSC")))      xt.tsc = strtoull(e, NULL, 0);
		if ((e = getenv("HVX_XT_EXPLICIT"))) {
			char *end;
			while (*e && xt.n_explicit < HVX_XT_MAX_EXPLICIT) {
				xt.explicit_[xt.n_explicit++] = strtoull(e, &end, 0);
				if (end == e) break;
				e = (*end == ',') ? end + 1 : end;
			}
		}
		if (ioctl(b->fd, HVX_SET_XTICK, &xt) < 0) { perror("HVX_SET_XTICK"); return -1; }
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

#define RACE_MARKER "hydo-race-done"

/* The HYDO-RACE result line after console offset `from`, and its lost= value. */
static long race_result(const struct boot *b, size_t from, char *line, size_t n)
{
	long at = b->ulen > from ? mem_find(b->ubuf + from, b->ulen - from, "HYDO-RACE:") : -1;
	const char *l;
	size_t i = 0;

	snprintf(line, n, "HYDO-RACE: (none)");
	if (at < 0)
		return -999999;
	at += (long)from;
	while ((size_t)at + i < b->ulen && b->ubuf[at + i] != '\r' && b->ubuf[at + i] != '\n' && i + 1 < n) {
		line[i] = (char)b->ubuf[at + i];
		i++;
	}
	line[i] = '\0';
	l = strstr(line, "lost=");
	return l ? atol(l + 5) : -999999;
}

/*
 * Branch mode (HVX_BRANCH_PLAN set): boot WITHOUT jitter until the console shows a marker
 * (HVX_BRANCH_MARK, default HYDO-RACE-NEAR, printed by hydorace a few seats before its
 * once-per-run window), stopping at a deterministic point (HVX_RUNC in small event budgets),
 * snapshot there, then run timing variants from the snapshot, each in a fresh VM, with
 * jitter only on ticks after the snapshot:
 *   random:N:PM:MAX   seeds 1..N, PM delayed ticks per 1000, delays up to MAX branches
 *   sweep:L:STEP:MAX  one delayed tick at a time: each of the next L ticks, delays STEP,
 *                     2*STEP, ... up to MAX (a systematic scan of where that tick lands)
 *   one:SEED:PM:MAX   one seeded variant (to compare with a straight run that uses
 *                     HVX_TJ_SEED=SEED HVX_TJ_LO=<the printed tick_lo>)
 * Prints "BRANCH tick_lo=.." then one "VARIANT .." line per variant.
 */
static int branch_main(const char *cmdline, uint64_t mem_size, const uint8_t *image, size_t image_len,
		       const uint8_t *initrd, size_t initrd_len)
{
	const char *plan = getenv("HVX_BRANCH_PLAN");
	const char *mark = getenv("HVX_BRANCH_MARK") ? getenv("HVX_BRANCH_MARK") : "HYDO-RACE-NEAR";
	static struct hvx_snapshot sn;
	static struct hvx_tjitter t;
	struct hvx_pmu pm;
	struct boot b;
	uint8_t *ram, *con;
	size_t clen;
	uint64_t lo, a1, a2, a3, i, nvar = 0, hits = 0, first = 0;
	char kind[16] = "", line[256];

	if (sscanf(plan, "%15[a-z]:%" SCNu64 ":%" SCNu64 ":%" SCNu64, kind, &a1, &a2, &a3) != 4) {
		fprintf(stderr, "bad HVX_BRANCH_PLAN\n");
		return 2;
	}
	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline))
		return 1;
	for (;;) {
		struct hvx_run_ctl c;

		memset(&c, 0, sizeof(c));
		c.max_insns = g_budget;
		if (ioctl(b.fd, HVX_RUNC, &c) < 0) {
			if (errno == EINTR) continue;
			perror("HVX_RUNC");
			return 1;
		}
		console_drain(&b);
		if (mem_find(b.ubuf, b.ulen, mark) >= 0)
			break;
		if (c.reason != HVX_EXIT_STEP && c.reason != HVX_EXIT_THROTTLE) {
			fprintf(stderr, "guest stopped before the marker: reason %u\n", c.reason);
			return 1;
		}
	}
	memset(&pm, 0, sizeof(pm));
	if (ioctl(b.fd, HVX_GET_PMU, &pm) < 0 || !pm.period) { perror("HVX_GET_PMU"); return 1; }
	lo = pm.next / pm.period + 1;		/* the first tick whose target is not yet set */
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, &sn) < 0) { perror("HVX_GET_SNAPSHOT"); return 1; }
	ram = malloc(mem_size);
	con = malloc(b.ulen);
	if (!ram || !con) return 1;
	memcpy(ram, b.mem, mem_size);
	memcpy(con, b.ubuf, b.ulen);
	clen = b.ulen;
	if (!strcmp(kind, "cont") || !strcmp(kind, "same")) {
		/* diagnostics: cont = just continue this VM; same = restore into this VM */
		if (!strcmp(kind, "same")) {
			memset(b.mem, 0, mem_size);
			if (ioctl(b.fd, HVX_SET_SNAPSHOT, &sn) < 0) { perror("HVX_SET_SNAPSHOT"); return 1; }
			memcpy(b.mem, ram, mem_size);
		}
		if (run_until_marker(&b, RACE_MARKER)) return 1;
		race_result(&b, clen, line, sizeof(line));
		printf("%s %s | fp=%016" PRIx64 "\n", kind, line, fingerprint(&b));
		return 0;
	}
	boot_teardown(&b);
	printf("BRANCH tick_lo=%" PRIu64 " console=%zu plan=%s\n", lo, clen, plan);

	for (i = 0;; i++) {
		struct boot v;
		long lost;
		uint64_t k = 0, d = 0;

		memset(&t, 0, sizeof(t));
		if (!strcmp(kind, "random")) {
			if (i >= a1) break;
			t.seed = i + 1; t.permille = (uint32_t)a2; t.max_delay = a3; t.tick_lo = lo;
		} else if (!strcmp(kind, "sweep")) {
			uint64_t per = a3 / a2;	/* delays per tick */
			if (i >= a1 * per) break;
			k = lo + i / per; d = (i % per + 1) * a2;
			t.n_explicit = 1; t.explicit_[0].tick = k; t.explicit_[0].delay = d;
		} else if (!strcmp(kind, "one")) {
			if (i >= 1) break;
			t.seed = a1; t.permille = (uint32_t)a2; t.max_delay = a3; t.tick_lo = lo;
		} else {
			fprintf(stderr, "unknown plan kind %s\n", kind);
			return 2;
		}
		if (boot_setup(&v, mem_size, image, image_len, initrd, initrd_len, cmdline)) return 1;
		if (ioctl(v.fd, HVX_SET_SNAPSHOT, &sn) < 0) { perror("HVX_SET_SNAPSHOT"); return 1; }
		memcpy(v.mem, ram, mem_size);
		memcpy(v.ubuf, con, clen);
		v.ulen = clen;
		console_rebase(&v);
		if (ioctl(v.fd, HVX_SET_TJITTER, &t) < 0) { perror("HVX_SET_TJITTER"); return 1; }
		if (run_until_marker(&v, RACE_MARKER) == 0) {
			lost = race_result(&v, clen, line, sizeof(line));
			nvar++;
			if (lost > 0) { hits++; if (!first) first = nvar; }
			if (!strcmp(kind, "sweep"))
				printf("VARIANT %" PRIu64 " tick=%" PRIu64 " delay=%" PRIu64 " lost=%ld fp=%016" PRIx64 "\n",
				       nvar, k, d, lost, fingerprint(&v));
			else
				printf("VARIANT %" PRIu64 " seed=%" PRIu64 " lost=%ld fp=%016" PRIx64 "\n",
				       nvar, (uint64_t)t.seed, lost, fingerprint(&v));
		} else {
			printf("VARIANT %" PRIu64 " ERROR\n", nvar + 1);
		}
		boot_teardown(&v);
	}
	printf("BRANCH-SUMMARY variants=%" PRIu64 " hits=%" PRIu64 " first_hit=%" PRIu64 "\n", nvar, hits, first);
	return 0;
}

int main(int argc, char **argv)
{
	const char *cmdline;
	uint64_t mem_size;
	uint8_t *image, *initrd;
	size_t image_len = 0, initrd_len = 0;
	struct boot b;
	char line[256] = "HYDO-RACE: (none)";
	long at;
	int rc = 1;

	if (argc < 3) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-race> [\"cmdline\"] [memMiB]\n", argv[0]);
		return 2;
	}
	cmdline  = (argc >= 4 && argv[3][0]) ? argv[3] : DEFAULT_CMDLINE;
	mem_size = ((argc >= 5) ? (uint64_t)strtoull(argv[4], NULL, 0) : DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;
	image = read_file(argv[1], &image_len);
	initrd = read_file(argv[2], &initrd_len);
	if (!image || !initrd)
		return 1;
	if (getenv("HVX_RUN_BUDGET"))
		g_budget = strtoull(getenv("HVX_RUN_BUDGET"), NULL, 0);
	if (getenv("HVX_BRANCH_PLAN"))
		return branch_main(cmdline, mem_size, image, image_len, initrd, initrd_len);

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
	    run_until_marker(&b, RACE_MARKER) == 0) {
		at = mem_find(b.ubuf, b.ulen, "HYDO-RACE:");
		if (at >= 0) {
			size_t i = 0;
			while ((size_t)at + i < b.ulen && b.ubuf[at + i] != '\r' &&
			       b.ubuf[at + i] != '\n' && i + 1 < sizeof(line)) {
				line[i] = (char)b.ubuf[at + i];
				i++;
			}
			line[i] = '\0';
		}
		{
			static struct hvx_tjitter tj;
			struct hvx_pmu pm;

			memset(&tj, 0, sizeof(tj));
			memset(&pm, 0, sizeof(pm));
			ioctl(b.fd, HVX_GET_TJITTER, &tj);	/* fails harmlessly on an old module */
			ioctl(b.fd, HVX_GET_PMU, &pm);
			printf("%s | fp=%016" PRIx64 " ticks=%llu delayed=%llu dhash=%016llx", line,
			       fingerprint(&b), (unsigned long long)pm.ticks,
			       (unsigned long long)tj.applied, (unsigned long long)tj.applied_hash);
			if (getenv("HVX_XT_SEED") || getenv("HVX_XT_EXPLICIT")) {
				static struct hvx_xtick xt;
				memset(&xt, 0, sizeof(xt));
				ioctl(b.fd, HVX_GET_XTICK, &xt);
				printf(" xtn=%llu xfirst=%llu xlast=%llu", (unsigned long long)xt.applied,
				       (unsigned long long)xt.first, (unsigned long long)xt.last);
			}
			printf(" steps=%llu\n", (unsigned long long)pm.steps);
		}
		rc = 0;
	}
	if (rc && b.fd >= 0 && getenv("HVX_RACE_DIAG")) {
		struct hvx_snapshot *sn = calloc(1, sizeof(*sn));
		size_t from = b.ulen > 1500 ? b.ulen - 1500 : 0;

		if (sn && ioctl(b.fd, HVX_GET_SNAPSHOT, sn) == 0) {
			printf("DIAG rip=%llx rflags=%llx tsc=%llu icount=%llu deadline=%llu lvt_timer=%x "
			       "tmict=%x tdcr=%x in_service=%x inject_valid=%u vec=%x at=%llu "
			       "irr=%llx:%llx:%llx:%llx pmu_total=%llu pmu_next=%llu\n",
			       (unsigned long long)sn->regs.rip, (unsigned long long)sn->regs.rflags,
			       (unsigned long long)sn->tsc, (unsigned long long)sn->icount,
			       (unsigned long long)sn->lapic_tsc_deadline, sn->lapic_reg[0x32],
			       sn->lapic_reg[0x38], sn->lapic_reg[0x3e], sn->lapic_in_service,
			       sn->inject_valid, sn->inject_vector,
			       (unsigned long long)sn->inject_at_icount,
			       (unsigned long long)sn->irr[0], (unsigned long long)sn->irr[1],
			       (unsigned long long)sn->irr[2], (unsigned long long)sn->irr[3],
			       (unsigned long long)sn->pmu_total, (unsigned long long)sn->pmu_next);
		}
		printf("DIAG console tail:\n%.*s\n", (int)(b.ulen - from), b.ubuf + from);
		free(sn);
	}
	boot_teardown(&b);
	return rc;
}

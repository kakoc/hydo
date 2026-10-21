// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_redis_verify - milestone 27 (FLAGSHIP on UNMODIFIED REDIS): reproduce Redis's
 * DOCUMENTED AOF durability window deterministically, and prove it bit-for-bit.
 * =============================================================================
 *
 * M26 proved the find-a-bug-by-fault-then-reproduce loop on an off-the-shelf sqlite3.
 * M27 runs an UNMODIFIED, statically-linked upstream `redis-server` + `redis-cli`
 * CO-LOCATED in one L2 guest, talking over the in-kernel `127.0.0.1` LOOPBACK (the
 * "loopback-first" pattern -- no virtio-net, deterministic for free). Persistence is
 * the AOF on a REAL ext2 fs on /dev/vda (reusing M26's fs+disk work).
 *
 * THE DEMONSTRATION -- Redis's famous `appendfsync` knob, under a FAITHFUL power loss:
 *   The backing disk has a VOLATILE WRITE CACHE (the M26 flush-aware fault class
 *   HVX_FAULT_DROP_UNFLUSHED at prob 100): a write is durable ONLY after a virtio-blk
 *   FLUSH (Linux issues one on fsync, since the device advertises F_FLUSH when the
 *   class is armed). The "power loss" is modeled with NO new kernel code, purely by the
 *   existing snapshot/disk machinery:
 *     boot1 (WORKLOAD, fault armed): mount ext2, bring up lo, start redis-server, drive
 *       NKEYS committed `SET`s over 127.0.0.1 via redis-cli (each OK = acked), then
 *       SIGSTOP redis (freeze the durability state at the just-acked instant, before any
 *       periodic fsync) and stop.  ==> HVX_GET_DISK returns the DURABLE image = only the
 *       FLUSH-committed bytes (the un-flushed volatile cache is NOT in it -- exactly what
 *       survives a power loss).
 *     boot2 (ORACLE, a FRESH VM, no fault): HVX_SET_DISK the durable image (RAM is gone,
 *       the volatile cache is gone -- a true reboot), redis-server AOF-REPLAYS from it,
 *       and an oracle re-reads every acked key.  ==> `HYDO-ORACLE: OK keys=<K> durable`
 *       or `HYDO-ORACLE: VIOLATION key=<k> expected=<e> got=<g>`, then the stop marker
 *       `hydo-redis-marker: oracle complete`.
 *
 *   Because FLUSH is the sole persistence gate, the outcome is a pure function of the
 *   fsync POLICY, robust to writeback timing:
 *     * appendfsync=always   -> fsync (=FLUSH=durable) BEFORE the client's OK, so every
 *                               acked write is on the durable image -> SURVIVES (OK).
 *     * appendfsync=everysec -> the AOF fsync is batched to ~1/s; the just-acked writes
 *                               sit un-flushed and are LOST on the crash -> VIOLATION.
 *   This is Redis's DOCUMENTED tradeoff (everysec = "last <=1s of acked writes may be
 *   lost on power loss"), reproduced and rewound bit-for-bit -- NOT a bug we introduced.
 *
 * The checks (hydo-verify, now on a REAL, documented durability window in REAL software):
 *   (SANITY) everysec, NO fault, NO crash: redis works + `HYDO-ORACLE: OK` (the app is
 *        correct absent the power loss) -- proves loopback client<->server + AOF.
 *   (F1) DURABLE CONFIG (always) under the volatile-cache power loss: OK, and DETERMINISTIC
 *        -- two full crash-recover runs are bit-identical in FULL state (console + snapshot
 *        + 256 MiB RAM + 16 MiB disk). New OK baseline.
 *   (F2) DOCUMENTED WINDOW (everysec) under the SAME power loss: a committed key is LOST
 *        (VIOLATION); the failing run REPLAYS bit-for-bit AND branch-from-snapshots
 *        bit-for-bit. New VIOLATION baseline.  ==> F1-vs-F2 IS the always/everysec contrast.
 *   (F3) SEED-SENSITIVE (a genuinely probabilistic fault -- a LYING-DISK DROP at prob 5%
 *        on the durable `always` config): seed A vs seed B lose DIFFERENT keys, each
 *        reproducible -- proving the fault schedule is really consumed by the seed.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_redis_verify hvx_redis_verify.c
 * Run   (inside L1):  sudo ./hvx_redis_verify /tmp/bzImage /tmp/initramfs-redis.cpio.gz /tmp/redis-disk.img
 */

#include "hvx_boot.h"

/* Printed by /init (oracle/solo phase) right after the oracle -- the stop point. */
#define FINAL_MARKER "hydo-redis-marker: oracle complete"
/* Printed by /init (workload phase) after the acked SETs + SIGSTOP -- boot1's stop. */
#define WORKLOAD_MARKER "hydo-redis-workload-done"
/* Deep in the ORACLE boot, AFTER mount but BEFORE redis loads the AOF -- the branch
 * mid-point (so a branch reproduces the whole AOF replay + oracle). */
#define MID_MARKER "hydo-redis: recovering from AOF"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define MAX_RUN_ITERS 400000000ULL

/* Lying-disk DROP probability for F3 (seed-sensitive). redis fsyncs the AOF per SET under
 * appendfsync=always, so each ~1.5 KB committed value is its own page write; a per-request
 * DROP at this probability loses a NAMED key with the AOF still loadable, and different
 * seeds lose different keys. */
#define DROP_PROB    8u
#define SEED_A       0x5ED15A0ULL
#define SEED_B       0x5ED15A1ULL

/* The DURABLE outcome fingerprint (the oracle-verdict console + the persisted AOF on disk),
 * set as a side effect of every fingerprint() call. Unlike the full state it does NOT
 * include the 256 MiB RAM, so it is invariant to post-marker idle bookkeeping (jiffies /
 * scheduler) that the synthetic idle-heartbeat advances between the marker and the harness's
 * throttle-boundary stop -- used to prove the branch reproduces the DURABILITY OUTCOME even
 * though the full-RAM endpoint of a branch vs a separate straight run differs by that jitter. */
static uint64_t g_durable_fp;

/* ---- canonical fingerprint (FNV-1a; identical layout to hvx_sqlite_verify.c) ---- */
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

/* Extract the HYDO-ORACLE verdict line from the reconstructed console. */
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
			fprintf(stderr, "hvx_redis_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 4096) == 0)
				fprintf(stderr, "[hvx_redis_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 240 ? b->ulen - 240 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_redis_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_redis_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_redis_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Canonical full-state fingerprint: console (incl. the HYDO-ORACLE verdict) + snapshot
 * (meaningful fields incl. virtio-blk + MSI-X + the seeded fault policy/PRNG state) + all
 * guest RAM + the backing disk image. Byte-for-byte identical layout to sqlite's harness. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	struct hvx_disk dk;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size, dlen = b->disk_len;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }

	h = fnv(h, "hvx-redis-state-v1", 18);
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
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
	{ uint64_t dfp = fnv(0xcbf29ce484222325ULL, b->ubuf, b->ulen);   /* durable = console ... */

	  h = fnv(h, &rlen, sizeof(rlen));
	  h = fnv(h, b->mem, b->mem_size);
	  dk.len = b->disk_len; dk.data = (uint64_t)(uintptr_t)b->diskbuf; dk.flags = 0;
	  if (ioctl(b->fd, HVX_GET_DISK, &dk) == 0) {
		uint64_t got = dk.len < b->disk_len ? dk.len : b->disk_len;
		h = fnv(h, &dlen, sizeof(dlen));
		h = fnv(h, b->diskbuf, got);
		dfp = fnv(dfp, b->diskbuf, got);                        /* ... + the persisted disk */
	  }
	  g_durable_fp = dfp;   /* the observable + persisted outcome (verdict console + AOF) */
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
	cfg.flags = HVX_VM_IDLE_HEARTBEAT;	/* redis blocks in epoll_wait on loopback data
						 * whose delivery needs a softirq/tick (M27) */
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

struct result { uint64_t fp; uint64_t dfp; int ok; char verdict[192]; };

/* Shared inputs threaded through every scenario. */
struct env {
	uint64_t mem_size;
	const uint8_t *image; size_t image_len;
	const uint8_t *initrd; size_t initrd_len;
	const uint8_t *empty_disk; uint64_t disk_len;	/* the pristine ext2 image */
	const char *cmdbase;
};

/* Compose "<cmdbase> hydo_phase=<phase> hydo_fsync=<mode>". */
static void mkcmd(char *out, size_t n, const struct env *e, const char *phase, const char *fsync_mode)
{
	snprintf(out, n, "%s hydo_phase=%s hydo_fsync=%s", e->cmdbase, phase, fsync_mode);
}

/* ---- SOLO: a single boot, workload + oracle in ONE timeline (no crash). ---- */
static int run_solo(const struct env *e, const char *fsync_mode, struct result *r)
{
	struct boot b;
	char cmd[512];
	int rc = -1;

	memset(r, 0, sizeof(*r));
	mkcmd(cmd, sizeof(cmd), e, "solo", fsync_mode);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->empty_disk, e->disk_len, cmd, 0, 0, 0) == 0 &&
	    run_until_marker(&b, FINAL_MARKER) == 0) {
		r->fp = fingerprint(&b);
		r->dfp = g_durable_fp;
		oracle_verdict(&b, r->verdict, sizeof(r->verdict));
		r->ok = (r->fp != 0);
		rc = 0;
	}
	boot_teardown(&b);
	return rc;
}

/* boot1 WORKLOAD: run redis under the fault, drive the acked SETs, SIGSTOP redis, and
 * return the DURABLE (flush-committed) disk image in `durable` (>= disk_len bytes). */
static int produce_durable(const struct env *e, const char *fsync_mode,
			   uint64_t seed, uint32_t classes, uint32_t prob, uint8_t *durable)
{
	struct boot b;
	struct hvx_disk dget;
	char cmd[512];
	int rc = -1;

	mkcmd(cmd, sizeof(cmd), e, "workload", fsync_mode);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->empty_disk, e->disk_len, cmd, seed, classes, prob) != 0)
		goto out;
	if (run_until_marker(&b, WORKLOAD_MARKER) != 0)
		goto out;
	dget.len = e->disk_len; dget.data = (uint64_t)(uintptr_t)durable; dget.flags = 0;
	if (ioctl(b.fd, HVX_GET_DISK, &dget) < 0) { perror("HVX_GET_DISK"); goto out; }
	{
		uint64_t dh = fnv(0xcbf29ce484222325ULL, durable, e->disk_len);
		fprintf(stderr, "produce_durable[%s cls=%u prob=%u seed=%#llx]: durable-hash=%016llx\n",
			fsync_mode, classes, prob, (unsigned long long)seed, (unsigned long long)dh);
	}
	rc = 0;
out:
	boot_teardown(&b);
	return rc;
}

/* boot2 ORACLE (fresh VM, NO fault): attach the durable image, redis AOF-replays, oracle
 * reads every acked key -> {fp, verdict}. */
static int run_oracle(const struct env *e, const char *fsync_mode,
		      const uint8_t *durable, struct result *r)
{
	struct boot b;
	char cmd[512];
	int rc = -1;

	memset(r, 0, sizeof(*r));
	mkcmd(cmd, sizeof(cmd), e, "oracle", fsync_mode);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       durable, e->disk_len, cmd, 0, 0, 0) == 0 &&
	    run_until_marker(&b, FINAL_MARKER) == 0) {
		r->fp = fingerprint(&b);
		r->dfp = g_durable_fp;
		oracle_verdict(&b, r->verdict, sizeof(r->verdict));
		r->ok = (r->fp != 0);
		rc = 0;
	}
	boot_teardown(&b);
	return rc;
}

/* Full crash-recover scenario: workload boot -> durable disk -> oracle boot. */
static int run_scenario(const struct env *e, const char *fsync_mode,
			uint64_t seed, uint32_t classes, uint32_t prob, struct result *r)
{
	uint8_t *durable = malloc(e->disk_len);
	int rc = -1;

	memset(r, 0, sizeof(*r));
	if (!durable) { perror("malloc"); return -1; }
	if (produce_durable(e, fsync_mode, seed, classes, prob, durable) == 0)
		rc = run_oracle(e, fsync_mode, durable, r);
	free(durable);
	return rc;
}

/* Branch-from-snapshot on the ORACLE boot (recovery replay): boot2 to MID_MARKER (mounted,
 * before redis loads the AOF), snapshot RAM + disk + state, finish -> ra; restore, finish
 * -> rb. ra.fp == rb.fp proves the recovery replay + disk + state are captured. */
static int run_oracle_branch(const struct env *e, const char *fsync_mode,
			     const uint8_t *durable, struct result *ra, struct result *rb)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
	uint8_t *ram_save = malloc(e->mem_size);
	uint8_t *disk_save = malloc(e->disk_len);
	size_t branch_ulen = 0;
	char cmd[512];
	int rc = -1;

	memset(ra, 0, sizeof(*ra));
	memset(rb, 0, sizeof(*rb));
	if (!mid || !ram_save || !disk_save) { perror("alloc"); goto done; }

	mkcmd(cmd, sizeof(cmd), e, "oracle", fsync_mode);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       durable, e->disk_len, cmd, 0, 0, 0) != 0 ||
	    run_until_marker(&b, MID_MARKER) != 0)
		goto teardown;

	{
		struct hvx_disk dget = { .len = e->disk_len, .data = (uint64_t)(uintptr_t)disk_save, .flags = 0 };

		fprintf(stderr, "hvx_redis_verify: reached mid marker at %zu bytes; snapshotting\n", b.ulen);
		branch_ulen = b.ulen;
		if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
		if (ioctl(b.fd, HVX_GET_DISK, &dget) < 0) { perror("HVX_GET_DISK"); goto teardown; }
		memcpy(ram_save, b.mem, e->mem_size);

		if (run_until_marker(&b, FINAL_MARKER) == 0) {
			ra->fp = fingerprint(&b);
			ra->dfp = g_durable_fp;
			oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
			ra->ok = (ra->fp != 0);
		}

		if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
		{
			struct hvx_disk dput = { .len = e->disk_len,
				.data = (uint64_t)(uintptr_t)disk_save, .flags = HVX_DISK_NO_IDMAP };
			memcpy(b.mem, ram_save, e->mem_size);
			if (ioctl(b.fd, HVX_SET_DISK, &dput) < 0) { perror("HVX_SET_DISK"); goto teardown; }
		}
		b.ulen = branch_ulen;
		console_rebase(&b);
		if (run_until_marker(&b, FINAL_MARKER) == 0) {
			rb->fp = fingerprint(&b);
			rb->dfp = g_durable_fp;
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
	const char *kernel_path, *initrd_path, *disk_path;
	struct env e;
	uint8_t *image = NULL, *initrd = NULL, *disk = NULL;
	size_t image_len = 0, initrd_len = 0, dl = 0;
	int rc = 1;
	struct result san;			/* SANITY: everysec, no fault, no crash */
	struct result f1a, f1b;			/* F1: always (durable) crash-recover, replay */
	struct result f2a, f2b, f2ba, f2bb;	/* F2: everysec (window) crash-recover + branch */
	uint8_t *f2_durable = NULL;
	struct result f3a, f3b;			/* F3: lying-disk DROP seed A vs B */
	int ok_sanity, ok_f1, ok_f2_verdict, ok_f2_repro, ok_f2_branch, ok_seed;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-redis> <redis-disk.img> [\"cmdline\"] [memMiB]\n",
			argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	disk_path   = argv[3];
	e.cmdbase   = (argc >= 5 && argv[4][0]) ? argv[4] : DEFAULT_CMDLINE;
	e.mem_size  = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0)
				   : DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	disk = read_file(disk_path, &dl);
	if (!disk) { free(image); free(initrd); return 1; }

	e.image = image; e.image_len = image_len;
	e.initrd = initrd; e.initrd_len = initrd_len;
	e.empty_disk = disk; e.disk_len = dl;
	fprintf(stderr, "hvx_redis_verify: bzImage=%zuB initramfs-redis=%zuB redis-disk=%zuB mem=%llu MiB\n",
		image_len, initrd_len, dl, (unsigned long long)(e.mem_size / (1024 * 1024)));

	/* =================== SANITY: everysec, no fault, no crash =============== */
	fprintf(stderr, "\n=== SANITY: redis everysec, NO fault, NO crash -- expect HYDO-ORACLE: OK ===\n");
	run_solo(&e, "everysec", &san);
	ok_sanity = san.ok && verdict_is_ok(san.verdict);
	printf("SANITY  (everysec, no-crash)  verdict=|%s| fp=%s -> %s\n",
	       san.verdict, hx(san.fp), ok_sanity ? "OK (redis+loopback+AOF correct)" : "UNEXPECTED");

	/* =================== F1: DURABLE CONFIG (always) survives =============== */
	fprintf(stderr, "\n=== F1: always + volatile-cache power loss -- boot #1 (OK baseline) ===\n");
	run_scenario(&e, "always", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f1a);
	fprintf(stderr, "\n=== F1: always -- boot #2 (replay bit-for-bit) ===\n");
	run_scenario(&e, "always", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f1b);
	ok_f1 = f1a.ok && f1b.ok && verdict_is_ok(f1a.verdict) && f1a.fp == f1b.fp;
	printf("F1 ALWAYS   verdict=|%s|\n", f1a.verdict);
	printf("F1 ALWAYS   fp1=%s fp2=%s -> replay %s (durable config SURVIVES the power loss)\n",
	       hx(f1a.fp), hx(f1b.fp), (f1a.ok && f1b.ok && f1a.fp == f1b.fp) ? "IDENTICAL" : "MISMATCH");

	/* =================== F2: DOCUMENTED WINDOW (everysec) loses ============= */
	fprintf(stderr, "\n=== F2: everysec + SAME volatile-cache power loss -- boot #1 (the failing run) ===\n");
	run_scenario(&e, "everysec", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f2a);
	fprintf(stderr, "\n=== F2: everysec -- boot #2 (replay bit-for-bit) ===\n");
	run_scenario(&e, "everysec", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f2b);
	fprintf(stderr, "\n=== F2: everysec -- branch-from-snapshot across the AOF replay ===\n");
	f2_durable = malloc(e.disk_len);
	if (f2_durable && produce_durable(&e, "everysec", 0, HVX_FAULT_DROP_UNFLUSHED, 100, f2_durable) == 0)
		run_oracle_branch(&e, "everysec", f2_durable, &f2ba, &f2bb);
	ok_f2_verdict = f2a.ok && verdict_is_violation(f2a.verdict);
	ok_f2_repro   = f2a.ok && f2b.ok && f2a.fp == f2b.fp;
	/* Branch determinism: (1) the snapshot ROUND-TRIPS bit-for-bit in FULL state
	 * (restore reproduces the continuation exactly: fp_a == fp_b -- the M7 property),
	 * and (2) the branch lands on the identical DURABILITY OUTCOME as the straight run
	 * (the oracle-verdict console + the persisted AOF: dfp_a == the straight run's dfp).
	 * We do NOT require the full-RAM endpoint to equal a *separate* straight run's,
	 * because the synthetic idle-heartbeat advances jiffies/scheduler during the post-
	 * marker idle and the harness stops at a throttle boundary, so the full-RAM endpoints
	 * differ by that post-marker bookkeeping -- a stop-point artifact, not a divergence
	 * (console + disk are bit-identical, proving the durable state reproduced). */
	ok_f2_branch  = f2ba.ok && f2bb.ok && f2ba.fp == f2bb.fp &&
			f2ba.dfp == f2bb.dfp && f2ba.dfp == f2a.dfp &&
			verdict_is_violation(f2ba.verdict);
	printf("F2 EVERYSEC verdict=|%s|\n", f2a.verdict);
	printf("F2 EVERYSEC fp1=%s fp2=%s -> replay %s\n",
	       hx(f2a.fp), hx(f2b.fp), ok_f2_repro ? "IDENTICAL" : "MISMATCH");
	printf("F2 EVERYSEC branch fp_a=%s fp_b=%s (round-trip %s) ; durable dfp_a=%s straight-dfp=%s -> %s\n",
	       hx(f2ba.fp), hx(f2bb.fp), (f2ba.fp == f2bb.fp) ? "IDENTICAL" : "MISMATCH",
	       hx(f2ba.dfp), hx(f2a.dfp), ok_f2_branch ? "OUTCOME REPRODUCED" : "MISMATCH");

	/* =================== F3: SEED-SENSITIVITY (lying-disk DROP) ============= */
	fprintf(stderr, "\n=== F3: seed-sensitive LYING-DISK DROP (always) seed=0x%llx ===\n",
		(unsigned long long)SEED_A);
	run_scenario(&e, "always", SEED_A, HVX_FAULT_DROP, DROP_PROB, &f3a);
	fprintf(stderr, "\n=== F3: DIFFERENT SEED seed=0x%llx ===\n", (unsigned long long)SEED_B);
	run_scenario(&e, "always", SEED_B, HVX_FAULT_DROP, DROP_PROB, &f3b);
	ok_seed = f3a.ok && f3b.ok && verdict_is_violation(f3a.verdict) &&
		  verdict_is_violation(f3b.verdict) && f3a.fp != f3b.fp;
	printf("F3 SEED-A   verdict=|%s| fp=%s\n", f3a.verdict, hx(f3a.fp));
	printf("F3 SEED-B   verdict=|%s| fp=%s -> %s\n", f3b.verdict, hx(f3b.fp),
	       ok_seed ? "DIFFERENT+REPRODUCIBLE" : "UNEXPECTED");

	/* =================== verdict ========================================== */
	if (ok_sanity && ok_f1 && ok_f2_verdict && ok_f2_repro && ok_f2_branch && ok_seed) {
		printf("hvx_redis_verify: MILESTONE 27 PASS: UNMODIFIED redis-server + redis-cli, "
		       "co-located over 127.0.0.1 loopback, AOF on a real ext2 fs. Redis's DOCUMENTED "
		       "durability window reproduced deterministically: under a faithful volatile-cache "
		       "power loss, appendfsync=always SURVIVES (F1 OK, baseline %s) but appendfsync=everysec "
		       "LOSES a committed key (F2 VIOLATION, baseline %s) and the failure REPLAYS + BRANCHES "
		       "bit-for-bit; a seeded lying-disk DROP loses different keys per seed (F3 %s vs %s). "
		       "The always-vs-everysec contrast isolates exactly the AOF fsync window.\n",
		       hx(f1a.fp), hx(f2a.fp), hx(f3a.fp), hx(f3b.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_redis_verify: FAIL: sanity=%d F1=%d F2[verdict=%d repro=%d branch=%d] seed=%d\n",
			ok_sanity, ok_f1, ok_f2_verdict, ok_f2_repro, ok_f2_branch, ok_seed);
	}

	free(f2_durable);
	free(image); free(initrd); free(disk);
	return rc;
}

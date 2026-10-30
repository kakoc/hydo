// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_smp_boot_verify - SMP Stage 2: real Linux booted with N vCPUs on the in-kernel
 * scheduler (HVX_VM_SMP_SCHED + HVX_VM_SMP_BOOT), checked for whole-boot determinism and
 * branch-from-snapshot, like hvx_boot_verify does for one vCPU.
 *
 * The boot: vCPU 0 enters the bzImage in long mode (the hvx_boot.h recipe, with one MP
 * table processor entry per vCPU); vCPUs 1..N-1 wait for INIT + STARTUP, which Linux
 * sends through the x2APIC ICR. All vCPUs run in exact branch-counted turns (quantum
 * HVX_QUANTUM, default 2^20) on one VM clock (PMU branch clock). PAUSE exits are on, and
 * HVX_PAUSE_K consecutive PAUSEs end a turn so a vCPU spinning on a lock gives way.
 *
 * Checks:
 *   1. two boots from scratch to FINAL_MARKER are identical in full state: console +
 *      every vCPU's HVX_GET_SNAPSHOT + scheduler state (HVX_GET_SCHED) + all guest RAM;
 *   2. branch, twice: at the first return after MID_MARKER, and at the end of turn T/2
 *      (T = turns of boot #1, HVX_SET_SCHED stop_turn): save everything, continue to
 *      FINAL_MARKER (A); restore, continue (B). A == B == boot #1.
 *   The console must contain SMP_MARKER ("smp: Brought up 1 node, N CPUs").
 *
 * Usage: hvx_smp_boot_verify <bzImage> <initramfs> [ncpus] ["cmdline"] [memMiB]
 * Env:   HVX_DEV (default /dev/hvx), HVX_CPU_V3 (guest AVX), HVX_QUANTUM, HVX_PAUSE_K (default 4), HVX_SEED
 *        (seeded turn lengths), HVX_MODE=all|one (one: a single boot, for bring-up),
 *        HVX_CMDLINE_EXTRA (appended to the command line), HVX_CONSOLE_OUT (file:
 *        console of the last boot), HVX_FINAL / HVX_MID (override the markers),
 *        HVX_BRANCH_TURNS=t1,t2,.. (extra branch points), HVX_MODE=scan with
 *        HVX_SCAN_FROM / HVX_SCAN_TO (print every vCPU's mode at each turn end),
 *        HVX_TSC_STEP (virtual TSC step per RDTSC, default 0x10000), HVX_PROGRESS,
 *        HVX_DISK=<image> (attach a virtio-blk disk; its bytes join the fingerprint).
 */
#include <stddef.h>
#include <time.h>
#include "hvx_boot.h"

#define FINAL_MARKER_DEF "hydo-boot-marker: launching shell on ttyS0"
#define MID_MARKER_DEF   "Trying to unpack rootfs image as initramfs"
#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define MAX_RUN_ITERS 50000000ULL

static const char *g_final = FINAL_MARKER_DEF, *g_mid = MID_MARKER_DEF;
static unsigned int g_ncpus = 2;
static uint64_t g_quantum = 1ULL << 20, g_seed;
static uint32_t g_pause_k = 4;
static uint64_t g_tsc_step = BOOT_TSC_STEP;
static uint8_t *g_disk;		/* HVX_DISK: virtio-blk image attached to every boot (read-only boots) */
static size_t g_disk_len;

static void disk_set(int fd, const uint8_t *img)
{
	struct hvx_disk dk = { .len = g_disk_len, .data = (uint64_t)(uintptr_t)img,
			       .flags = HVX_DISK_NO_IDMAP };

	if (img && ioctl(fd, HVX_SET_DISK, &dk) < 0) {
		perror("HVX_SET_DISK");
		exit(1);
	}
}

static uint8_t *disk_get(int fd)
{
	uint8_t *buf;
	struct hvx_disk dk;

	if (!g_disk)
		return NULL;
	buf = malloc(g_disk_len);
	dk.len = g_disk_len;
	dk.data = (uint64_t)(uintptr_t)buf;
	dk.flags = 0;
	if (!buf || ioctl(fd, HVX_GET_DISK, &dk) < 0) {
		perror("HVX_GET_DISK");
		exit(1);
	}
	return buf;
}

static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;

	while (n--) {
		h ^= *b++;
		h *= 0x100000001b3ULL;
	}
	return h;
}

static double now_s(void)
{
	struct timespec t;

	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec * 1e-9;
}

struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *ubuf;
	size_t ulen, drained;
	unsigned long throttles;
};

static long mem_find(const uint8_t *hay, size_t hlen, const char *needle)
{
	size_t nlen = strlen(needle), i;

	if (!nlen || hlen < nlen)
		return -1;
	for (i = 0; i + nlen <= hlen; i++)
		if (!memcmp(hay + i, needle, nlen))
			return (long)i;
	return -1;
}

static struct hvx_console con_scratch;
static void console_drain(struct boot *b)
{
	size_t modlen, add;

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0) {
		perror("HVX_GET_CONSOLE");	/* never ignore it: it would move the stop point */
		exit(1);
	}
	modlen = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
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
		perror("HVX_GET_CONSOLE");
		exit(1);
	}
	b->drained = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

static void set_vcpu(struct boot *b, uint32_t i)
{
	if (ioctl(b->fd, HVX_SET_VCPU, &i) < 0) {
		perror("HVX_SET_VCPU");
		exit(1);
	}
}

static void get_sched(struct boot *b, struct hvx_sched *s)
{
	if (ioctl(b->fd, HVX_GET_SCHED, s) < 0) {
		perror("HVX_GET_SCHED");
		exit(1);
	}
}

static void dump_vcpus(struct boot *b)
{
	struct hvx_sched s;
	unsigned int i;

	get_sched(b, &s);
	fprintf(stderr, "  sched: cur=%u in_turn=%u turns=%llu branches=%llu tsc=%llu icount=%llu "
		"jumps=%llu wakeups=%llu deferred=%llu overshoots=%llu\n", s.cur, s.in_turn,
		(unsigned long long)s.turns, (unsigned long long)s.pmu_total,
		(unsigned long long)s.tsc, (unsigned long long)s.icount,
		(unsigned long long)s.time_jumps, (unsigned long long)s.wakeups,
		(unsigned long long)s.deferred, (unsigned long long)s.overshoots);
	for (i = 0; i < g_ncpus; i++) {
		struct hvx_regs r;
		struct hvx_sregs sr;

		set_vcpu(b, i);
		ioctl(b->fd, HVX_GET_REGS, &r);
		ioctl(b->fd, HVX_GET_SREGS, &sr);
		fprintf(stderr, "  vcpu%u: halted=%u rip=%#llx rsp=%#llx rflags=%#llx cr0=%#llx "
			"efer=%#llx cs=%#x branches=%llu turns=%llu\n", i, s.v[i].halted,
			(unsigned long long)r.rip, (unsigned long long)r.rsp,
			(unsigned long long)r.rflags, (unsigned long long)sr.cr0,
			(unsigned long long)sr.efer, sr.cs.selector,
			(unsigned long long)s.v[i].branches, (unsigned long long)s.v[i].turns);
		if (r.rip >= 0xffffffff80000000ULL)
			dump_fault_insn(b->mem, b->mem_size, r.rip);
	}
	set_vcpu(b, 0);
}

/*
 * HVX_RUN until a return (THROTTLE) at which the console contains `marker` -- the stop
 * point of a fresh boot. check_first: the current position is such a return (a THROTTLE
 * return that reached an earlier marker), so stop at once if the marker is already there;
 * after a stop that a fresh boot does not make (a turn stop, a max_insns stop) run on to
 * the next return instead.
 */
static int run_until_marker_ex(struct boot *b, const char *marker, int check_first)
{
	struct hvx_exit ex;
	unsigned long long iters;

	if (check_first && mem_find(b->ubuf, b->ulen, marker) >= 0)
		return 0;
	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_smp_boot_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);
		if (getenv("HVX_PROGRESS")) {
			struct hvx_sched s;
			static double t0;
			double t = now_s();

			if (!t0)
				t0 = t;
			get_sched(b, &s);
			fprintf(stderr, "[progress %.3f s: reason=%#x console=%zu turns=%llu branches=%llu "
				"tsc=%.3f s jumps=%llu", t - t0, ex.reason, b->ulen, (unsigned long long)s.turns,
				(unsigned long long)s.pmu_total, s.tsc / 1.6e9,
				(unsigned long long)s.time_jumps);
			for (unsigned int i = 0; i < g_ncpus; i++) {
				struct hvx_regs r;

				set_vcpu(b, i);
				ioctl(b->fd, HVX_GET_REGS, &r);
				fprintf(stderr, " v%u=%llu/%llu@%llx%s", i, (unsigned long long)s.v[i].branches,
					(unsigned long long)s.v[i].turns, (unsigned long long)r.rip,
					s.v[i].halted ? "h" : "");
			}
			set_vcpu(b, 0);
			fprintf(stderr, "]\n");
		}
		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;
		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++b->throttles % 4096) == 0)
				fprintf(stderr, "[%lu throttles, %zu console bytes]\n", b->throttles, b->ulen);
			continue;
		}
		{
			size_t from = b->ulen > 600 ? b->ulen - 600 : 0;

			fprintf(stderr, "hvx_smp_boot_verify: stopped before \"%s\": reason=%#x (%s) "
				"qual=%#llx gpa=%#llx\n", marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification, (unsigned long long)ex.guest_phys);
			dump_vcpus(b);
			fprintf(stderr, "console tail: |%.*s|\n", (int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_smp_boot_verify: iteration cap before \"%s\"\n", marker);
	return -1;
}

static int run_until_marker(struct boot *b, const char *marker)
{
	return run_until_marker_ex(b, marker, 1);
}

/* the scheduler STATE (not statistics, not stop_turn) */
static uint64_t fnv_sched(uint64_t h, const struct hvx_sched *s)
{
	unsigned int i;

	h = fnv(h, &s->quantum, sizeof(s->quantum));
	h = fnv(h, &s->seed, sizeof(s->seed));
	h = fnv(h, &s->pause_k, sizeof(s->pause_k));
	h = fnv(h, &s->cur, sizeof(s->cur));
	h = fnv(h, &s->in_turn, sizeof(s->in_turn));
	h = fnv(h, &s->turn_over, sizeof(s->turn_over));
	h = fnv(h, &s->pause_run, sizeof(s->pause_run));
	h = fnv(h, &s->turn_end, sizeof(s->turn_end));
	h = fnv(h, &s->turns, sizeof(s->turns));
	h = fnv(h, &s->guard, sizeof(s->guard));
	h = fnv(h, &s->turn_hash, sizeof(s->turn_hash));
	h = fnv(h, &s->tsc, 6 * sizeof(uint64_t));		/* tsc .. pmu_next */
	for (i = 0; i < g_ncpus; i++)
		h = fnv(h, &s->v[i], sizeof(s->v[i]));
	return h;
}

struct result {
	uint64_t fp, h_con, h_ram, h_vcpu[HVX_SCHED_MAX_VCPUS], h_sched;
	size_t ulen;
	int ok;
	struct hvx_sched sched;
};

static void measure(struct boot *b, struct result *r)
{
	uint64_t h = 0xcbf29ce484222325ULL, clen = b->ulen, rlen = b->mem_size;
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	unsigned int i;

	memset(r, 0, sizeof(*r));
	if (!s) {
		perror("calloc");
		exit(1);
	}
	h = fnv(h, "hvx-smp-boot-v1", 15);
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
	r->h_con = fnv(0xcbf29ce484222325ULL, b->ubuf, b->ulen);
	for (i = 0; i < g_ncpus; i++) {
		set_vcpu(b, i);
		memset(s, 0, sizeof(*s));
		if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) {
			perror("HVX_GET_SNAPSHOT");
			exit(1);
		}
		/* zero-filled by the kernel: padding is stable. The guest-AVX fields at the end
		 * count only for an AVX VM (xcr0 != 0), so other VMs hash the bytes they did before. */
		size_t sl = s->xcr0 ? sizeof(*s) : offsetof(struct hvx_snapshot, xcr0);
		h = fnv(h, s, sl);
		r->h_vcpu[i] = fnv(0xcbf29ce484222325ULL, s, sl);
	}
	set_vcpu(b, 0);
	get_sched(b, &r->sched);
	h = fnv_sched(h, &r->sched);
	r->h_sched = fnv_sched(0xcbf29ce484222325ULL, &r->sched);
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);
	if (g_disk) {
		uint8_t *d = disk_get(b->fd);

		h = fnv(h, d, g_disk_len);
		free(d);
	}
	r->h_ram = fnv(0xcbf29ce484222325ULL, b->mem, b->mem_size);
	r->fp = h;
	r->ulen = b->ulen;
	r->ok = 1;
	free(s);
}

static void report_stats(const char *tag, struct boot *b, const struct result *r, double secs)
{
	const struct hvx_sched *s = &r->sched;
	uint64_t steps = 0, ticks = 0, over = 0;
	unsigned int i;

	for (i = 0; i < g_ncpus; i++) {
		struct hvx_pmu pm;

		set_vcpu(b, i);
		if (ioctl(b->fd, HVX_GET_PMU, &pm) == 0) {
			steps += pm.steps;
			ticks += pm.ticks;
			over += pm.overshoots;
		}
	}
	set_vcpu(b, 0);
	printf("hvx_smp_boot_verify: %s: %.2f s wall, %lu throttles, turns=%llu branches=%llu "
	       "ticks=%llu landing-steps=%llu pmu-overshoots=%llu turn-overshoots=%llu deferred=%llu "
	       "time-jumps=%llu wakeups=%llu\n", tag, secs, b->throttles,
	       (unsigned long long)s->turns, (unsigned long long)s->pmu_total,
	       (unsigned long long)ticks, (unsigned long long)steps, (unsigned long long)over,
	       (unsigned long long)s->overshoots, (unsigned long long)s->deferred,
	       (unsigned long long)s->time_jumps, (unsigned long long)s->wakeups);
	{
		/* where the guest routed its device interrupts (IOAPIC entry 4 = COM1, the
		 * virtio-blk MSI-X table): destination APIC ID and vector */
		struct hvx_snapshot *sn = calloc(1, sizeof(*sn));

		if (sn && ioctl(b->fd, HVX_GET_SNAPSHOT, sn) == 0) {
			printf("hvx_smp_boot_verify: %s: COM1 IOAPIC entry: dest %u vector %#x%s", tag,
			       sn->ioapic_redir[9] >> 24, sn->ioapic_redir[8] & 0xff,
			       (sn->ioapic_redir[8] & (1U << 16)) ? " (masked)" : "");
			if (g_disk)
				for (i = 0; i < HVX_MSIX_ENTRIES; i++)
					if (sn->vblk_msix_tbl[i * 4])
						printf("; virtio-blk MSI-X %u: dest %u vector %#x", i,
						       (sn->vblk_msix_tbl[i * 4] >> 12) & 0xff,
						       sn->vblk_msix_tbl[i * 4 + 2] & 0xff);
			printf("\n");
		}
		free(sn);
	}
	for (i = 0; i < g_ncpus; i++)
		printf("hvx_smp_boot_verify: %s: vcpu%u branches=%llu turns=%llu halted=%u\n", tag, i,
		       (unsigned long long)s->v[i].branches, (unsigned long long)s->v[i].turns,
		       s->v[i].halted);
}

static int boot_setup(struct boot *b, uint64_t mem_size, const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len, const char *cmdline)
{
	const char *dev = getenv("HVX_DEV");
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_sched s;
	struct hvx_tsc tsc = { .tsc = 0, .step = g_tsc_step, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->mem_size = mem_size;
	b->ubuf = malloc(UBUF_CAP);
	if (!b->ubuf)
		return -1;
	b->fd = open(dev ? dev : "/dev/hvx", O_RDWR);
	if (b->fd < 0) {
		perror("open hvx device");
		return -1;
	}
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	cfg.n_vcpus = g_ncpus;
	cfg.flags = HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED | HVX_VM_SMP_BOOT | HVX_VM_PAUSE_EXIT;
	if (getenv("HVX_CPU_V3") && strcmp(getenv("HVX_CPU_V3"), "0"))
		cfg.flags |= HVX_VM_X86_64_V3;	/* guest AVX/AVX2 on every vCPU */
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) {
		perror("HVX_CREATE_VM");
		return -1;
	}
	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) {
		perror("mmap");
		return -1;
	}
	set_vcpu(b, 0);
	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) {
		perror("HVX_SET_TSC");
		return -1;
	}
	build_page_tables_and_gdt(b->mem, mem_size);
	if (load_bzimage_n(b->mem, mem_size, image, image_len, initrd, initrd_len, cmdline,
			   g_ncpus) < 0)
		return -1;
	fill_sregs(&sregs);
	if (ioctl(b->fd, HVX_SET_SREGS, &sregs) < 0) {
		perror("HVX_SET_SREGS");
		return -1;
	}
	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(b->fd, HVX_SET_REGS, &regs) < 0) {
		perror("HVX_SET_REGS");
		return -1;
	}
	get_sched(b, &s);
	s.quantum = g_quantum;
	s.seed = g_seed;
	s.pause_k = g_pause_k;
	disk_set(b->fd, g_disk);
	if (ioctl(b->fd, HVX_SET_SCHED, &s) < 0) {
		perror("HVX_SET_SCHED");
		return -1;
	}
	return 0;
}

static void boot_teardown(struct boot *b)
{
	if (b->mem && b->mem != MAP_FAILED)
		munmap(b->mem, b->mem_size);
	if (b->fd >= 0)
		close(b->fd);
	free(b->ubuf);
	memset(b, 0, sizeof(*b));
	b->fd = -1;
}

static void save_console(struct boot *b)
{
	const char *p = getenv("HVX_CONSOLE_OUT");
	FILE *f;

	if (p && (f = fopen(p, "w"))) {
		fwrite(b->ubuf, 1, b->ulen, f);
		fclose(f);
	}
}

static void localize(const char *tag, const struct result *a, const struct result *b)
{
	unsigned int i;

	printf("hvx_smp_boot_verify: %s: console %s, ram %s, sched %s", tag,
	       a->h_con == b->h_con ? "same" : "DIFFERS", a->h_ram == b->h_ram ? "same" : "DIFFERS",
	       a->h_sched == b->h_sched ? "same" : "DIFFERS");
	for (i = 0; i < g_ncpus; i++)
		printf(", vcpu%u %s", i, a->h_vcpu[i] == b->h_vcpu[i] ? "same" : "DIFFERS");
	printf("\n");
}

/* one fresh boot to FINAL; fills r */
static int fresh_boot(const char *tag, uint64_t mem_size, const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len, const char *cmdline,
		      struct result *r, char *smp_line, size_t smp_cap)
{
	struct boot b;
	double t0 = now_s();
	int rc = -1;

	fprintf(stderr, "\n=== %s ===\n", tag);
	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
	    run_until_marker(&b, g_final) == 0) {
		double t = now_s() - t0;
		long at = mem_find(b.ubuf, b.ulen, "smp: Brought up");

		measure(&b, r);
		report_stats(tag, &b, r, t);
		if (at >= 0 && smp_line) {
			size_t n = 0;

			while (at + n < b.ulen && b.ubuf[at + n] != '\n' && b.ubuf[at + n] != '\r' &&
			       n + 1 < smp_cap)
				n++;
			memcpy(smp_line, b.ubuf + at, n);
			smp_line[n] = 0;
		}
		rc = 0;
	}
	save_console(&b);
	boot_teardown(&b);
	return rc;
}

/* HVX_RUN until `turn` turns are done (stop_turn); 0 on HVX_EXIT_TURN */
static int run_until_turn(struct boot *b, uint64_t turn)
{
	struct hvx_sched s;
	struct hvx_exit ex;

	get_sched(b, &s);
	s.stop_turn = turn;
	if (ioctl(b->fd, HVX_SET_SCHED, &s) < 0) {
		perror("HVX_SET_SCHED");
		return -1;
	}
	for (;;) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			perror("HVX_RUN");
			return -1;
		}
		console_drain(b);
		if (ex.reason == HVX_EXIT_TURN)
			break;
		if (ex.reason != HVX_EXIT_THROTTLE) {
			fprintf(stderr, "run_until_turn: reason %#x\n", ex.reason);
			dump_vcpus(b);
			return -1;
		}
		b->throttles++;
	}
	get_sched(b, &s);
	s.stop_turn = 0;
	return ioctl(b->fd, HVX_SET_SCHED, &s) < 0 ? -1 : 0;
}

/*
 * Branch-from-snapshot: run to the branch point (the mid marker if turn == 0, else the end
 * of turn `turn`), save every vCPU + the scheduler + RAM, continue to the final marker (A);
 * restore, continue again (B). A must equal B and the fresh boot `fresh`.
 */
static int branch_check(const char *tag, uint64_t turn, uint64_t mem_size, const uint8_t *image,
			size_t image_len, const uint8_t *initrd, size_t initrd_len, const char *cmdline,
			const struct result *fresh)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(g_ncpus, sizeof(*mid));
	struct hvx_sched msch;
	uint8_t *ram_save = malloc(mem_size);
	struct result ra, rb;
	size_t branch_ulen = 0;
	unsigned int i;
	int ok;
	uint8_t *disk_save = NULL;

	memset(&ra, 0, sizeof(ra));
	memset(&rb, 0, sizeof(rb));
	fprintf(stderr, "\n=== %s ===\n", tag);
	if (!mid || !ram_save) {
		perror("alloc");
		exit(1);
	}
	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
	    (turn ? run_until_turn(&b, turn) : run_until_marker(&b, g_mid)) == 0) {
		branch_ulen = b.ulen;
		for (i = 0; i < g_ncpus; i++) {
			set_vcpu(&b, i);
			if (ioctl(b.fd, HVX_GET_SNAPSHOT, &mid[i]) < 0) {
				perror("HVX_GET_SNAPSHOT");
				exit(1);
			}
		}
		set_vcpu(&b, 0);
		get_sched(&b, &msch);
		memcpy(ram_save, b.mem, mem_size);
		disk_save = disk_get(b.fd);
		printf("hvx_smp_boot_verify: %s: branch point at turn %llu, %zu console bytes%s\n", tag,
		       (unsigned long long)msch.turns, branch_ulen,
		       mem_find(b.ubuf, b.ulen, g_final) >= 0 ? " (already past the final marker)" : "");
		if (run_until_marker_ex(&b, g_final, turn == 0) == 0)
			measure(&b, &ra);
		memcpy(b.mem, ram_save, mem_size);
		for (i = 0; i < g_ncpus; i++) {
			set_vcpu(&b, i);
			if (ioctl(b.fd, HVX_SET_SNAPSHOT, &mid[i]) < 0) {
				perror("HVX_SET_SNAPSHOT");
				exit(1);
			}
		}
		set_vcpu(&b, 0);
		if (ioctl(b.fd, HVX_SET_SCHED, &msch) < 0) {
			perror("HVX_SET_SCHED");
			exit(1);
		}
		disk_set(b.fd, disk_save);
		b.ulen = branch_ulen;
		console_rebase(&b);
		if (run_until_marker_ex(&b, g_final, turn == 0) == 0)
			measure(&b, &rb);
	}
	boot_teardown(&b);
	free(mid);
	free(ram_save);
	free(disk_save);
	ok = ra.ok && rb.ok && ra.fp == rb.fp && ra.fp == fresh->fp;
	printf("hvx_smp_boot_verify: %s fp_a=%016llx fp_b=%016llx fresh=%016llx -> %s\n", tag,
	       (unsigned long long)ra.fp, (unsigned long long)rb.fp, (unsigned long long)fresh->fp,
	       ok ? "IDENTICAL" : "MISMATCH");
	if (ra.ok && rb.ok && ra.fp != rb.fp)
		localize("continued-vs-restored", &ra, &rb);
	if (ra.ok && fresh->ok && ra.fp != fresh->fp)
		localize("continued-vs-fresh", &ra, fresh);
	return ok;
}

/* one HVX_RUNC with max_insns = 1: stop at the next clean boundary after one retired
 * exit or tick of whichever vCPU runs */
static int runc1(struct boot *b)
{
	struct hvx_run_ctl c;

	memset(&c, 0, sizeof(c));
	c.max_insns = 1;
	if (ioctl(b->fd, HVX_RUNC, &c) < 0) {
		perror("HVX_RUNC");
		return -1;
	}
	console_drain(b);
	if (c.reason != HVX_EXIT_STEP && c.reason != HVX_EXIT_THROTTLE) {
		fprintf(stderr, "runc1: reason %#x\n", c.reason);
		dump_vcpus(b);
		return -1;
	}
	return 0;
}

/* the AP start-up point: end of turn `t`, then k single-event stops */
static int to_ap_point(struct boot *b, uint64_t t, unsigned int k)
{
	unsigned int i;

	if (run_until_turn(b, t))
		return -1;
	for (i = 0; i < k; i++)
		if (runc1(b))
			return -1;
	return 0;
}

/*
 * Branch inside vCPU 1's start-up (HVX_MODE=apbranch): find the turn after which the
 * STARTUP IPI made vCPU 1 runnable, then step it one exit at a time (CPUID / RDMSR /
 * WRMSR / port I/O of Linux's real-mode trampoline and verify_cpu) until it reaches long
 * mode, and branch at each of those points: continue (A), restore + continue (B); both
 * must equal the fresh boot. Covers snapshots taken while an AP is in real mode and in
 * protected mode without paging.
 */
static int apbranch(uint64_t mem_size, const uint8_t *image, size_t image_len,
		    const uint8_t *initrd, size_t initrd_len, const char *cmdline,
		    const struct result *fresh)
{
	struct boot b;
	struct hvx_sched s;
	uint64_t t, T = 0;
	unsigned int k, kmax = 0, bad = 0, n = 0;
	char modes[512][48];

	if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline))
		return 1;
	for (t = 1; t < 100000 && !T; t++) {
		if (run_until_turn(&b, t))
			return 1;
		get_sched(&b, &s);
		if (s.v[1].halted == 0)
			T = t;
	}
	if (!T) {
		fprintf(stderr, "apbranch: vCPU 1 never started\n");
		return 1;
	}
	for (k = 0; k < 512; k++) {
		struct hvx_sregs sr;
		struct hvx_regs r;

		set_vcpu(&b, 1);
		ioctl(b.fd, HVX_GET_SREGS, &sr);
		ioctl(b.fd, HVX_GET_REGS, &r);
		set_vcpu(&b, 0);
		snprintf(modes[k], sizeof(modes[k]), "cr0=%llx efer=%llx cs=%x rip=%llx",
			 (unsigned long long)sr.cr0, (unsigned long long)sr.efer, sr.cs.selector,
			 (unsigned long long)r.rip);
		if ((sr.efer & EFER_LMA) && sr.cs.selector == 0x10)	/* kernel long-mode CS */
			break;
		kmax = k;
		if (runc1(&b))
			return 1;
	}
	boot_teardown(&b);
	printf("hvx_smp_boot_verify: apbranch: vCPU 1 runnable after turn %llu; %u stop points "
	       "before it reaches the kernel's long-mode CS\n", (unsigned long long)T, kmax + 1);
	for (k = 0; k <= kmax; k++) {
		struct hvx_snapshot *mid = calloc(g_ncpus, sizeof(*mid));
		uint8_t *ram_save = malloc(mem_size);
		struct hvx_sched msch;
		struct result ra, rb;
		size_t ulen;
		unsigned int i;
		uint8_t *dsave;

		memset(&ra, 0, sizeof(ra));
		memset(&rb, 0, sizeof(rb));
		if (!mid || !ram_save || boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) ||
		    to_ap_point(&b, T, k))
			return 1;
		for (i = 0; i < g_ncpus; i++) {
			set_vcpu(&b, i);
			ioctl(b.fd, HVX_GET_SNAPSHOT, &mid[i]);
		}
		set_vcpu(&b, 0);
		get_sched(&b, &msch);
		memcpy(ram_save, b.mem, mem_size);
		dsave = disk_get(b.fd);
		ulen = b.ulen;
		if (run_until_marker_ex(&b, g_final, 0) == 0)
			measure(&b, &ra);
		memcpy(b.mem, ram_save, mem_size);
		for (i = 0; i < g_ncpus; i++) {
			set_vcpu(&b, i);
			if (ioctl(b.fd, HVX_SET_SNAPSHOT, &mid[i]) < 0)
				return 1;
		}
		set_vcpu(&b, 0);
		if (ioctl(b.fd, HVX_SET_SCHED, &msch) < 0)
			return 1;
		disk_set(b.fd, dsave);
		free(dsave);
		b.ulen = ulen;
		console_rebase(&b);
		if (run_until_marker_ex(&b, g_final, 0) == 0)
			measure(&b, &rb);
		boot_teardown(&b);
		free(mid);
		free(ram_save);
		n++;
		if (!(ra.ok && rb.ok && ra.fp == rb.fp && ra.fp == fresh->fp))
			bad++;
		printf("hvx_smp_boot_verify: apbranch point %u (vCPU 1 %s): A=%016llx B=%016llx -> %s\n", k,
		       modes[k], (unsigned long long)ra.fp, (unsigned long long)rb.fp,
		       ra.ok && rb.ok && ra.fp == rb.fp && ra.fp == fresh->fp ? "IDENTICAL" : "MISMATCH");
	}
	printf("hvx_smp_boot_verify: apbranch: %u points, %u mismatches -> %s\n", n, bad, bad ? "FAIL" : "PASS");
	return bad ? 1 : 0;
}

int main(int argc, char **argv)
{
	const char *mode = getenv("HVX_MODE") ? getenv("HVX_MODE") : "all";
	char cmdline[1024], smp_line[160] = "", smp_line2[160] = "", want[80];
	uint64_t mem_size;
	uint8_t *image, *initrd = NULL;
	size_t image_len = 0, initrd_len = 0;
	struct result r1, r2;
	int ok_whole, ok_branch, ok_smp;

	if (argc < 3) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs> [ncpus] [\"cmdline\"] [memMiB]\n", argv[0]);
		return 2;
	}
	if (argc >= 4)
		g_ncpus = (unsigned int)strtoul(argv[3], NULL, 0);
	/* ncpus 1: the same boot without the scheduler (the module ignores SMP_SCHED and
	 * SMP_BOOT with one vCPU), for a time comparison; its "smp:" line says "1 CPU" */
	if (g_ncpus < 1 || g_ncpus > HVX_SCHED_MAX_VCPUS) {
		fprintf(stderr, "ncpus must be 1..%d\n", HVX_SCHED_MAX_VCPUS);
		return 2;
	}
	snprintf(cmdline, sizeof(cmdline), "%s%s%s", (argc >= 5 && argv[4][0]) ? argv[4] : DEFAULT_CMDLINE,
		 getenv("HVX_CMDLINE_EXTRA") ? " " : "",
		 getenv("HVX_CMDLINE_EXTRA") ? getenv("HVX_CMDLINE_EXTRA") : "");
	mem_size = (argc >= 6 ? strtoull(argv[5], NULL, 0) : DEFAULT_MEM_MIB) << 20;
	if (getenv("HVX_QUANTUM"))
		g_quantum = strtoull(getenv("HVX_QUANTUM"), NULL, 0);
	if (getenv("HVX_SEED"))
		g_seed = strtoull(getenv("HVX_SEED"), NULL, 0);
	if (getenv("HVX_TSC_STEP"))
		g_tsc_step = strtoull(getenv("HVX_TSC_STEP"), NULL, 0);
	if (getenv("HVX_PAUSE_K"))
		g_pause_k = (uint32_t)strtoul(getenv("HVX_PAUSE_K"), NULL, 0);
	if (getenv("HVX_FINAL"))
		g_final = getenv("HVX_FINAL");
	if (getenv("HVX_MID"))
		g_mid = getenv("HVX_MID");
	if (getenv("HVX_DISK")) {
		g_disk = read_file(getenv("HVX_DISK"), &g_disk_len);
		if (!g_disk)
			return 1;
	}
	image = read_file(argv[1], &image_len);
	if (!image)
		return 1;
	if (argv[2][0] && strcmp(argv[2], "-")) {
		initrd = read_file(argv[2], &initrd_len);
		if (!initrd)
			return 1;
	}
	snprintf(want, sizeof(want), "smp: Brought up 1 node, %u CPUs", g_ncpus);
	fprintf(stderr, "hvx_smp_boot_verify: %u vCPUs, quantum %llu, seed %llu, pause_k %u, mem %llu MiB, "
		"cmdline \"%s\"\n", g_ncpus, (unsigned long long)g_quantum, (unsigned long long)g_seed,
		g_pause_k, (unsigned long long)(mem_size >> 20), cmdline);

	memset(&r1, 0, sizeof(r1));
	memset(&r2, 0, sizeof(r2));
	fresh_boot("boot #1", mem_size, image, image_len, initrd, initrd_len, cmdline, &r1,
		   smp_line, sizeof(smp_line));
	printf("hvx_smp_boot_verify: boot #1 %s, fp=%016llx, \"%s\"\n", r1.ok ? "reached the shell" : "FAILED",
	       (unsigned long long)r1.fp, smp_line);
	if (!strcmp(mode, "one"))
		return r1.ok && strstr(smp_line, want) ? 0 : 1;
	if (!strcmp(mode, "scan")) {
		/* print each vCPU's mode at every turn in [HVX_SCAN_FROM, HVX_SCAN_TO] */
		uint64_t t, from = strtoull(getenv("HVX_SCAN_FROM") ? getenv("HVX_SCAN_FROM") : "1", NULL, 0);
		uint64_t to = strtoull(getenv("HVX_SCAN_TO") ? getenv("HVX_SCAN_TO") : "200", NULL, 0);
		struct boot b;

		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline))
			return 1;
		for (t = from; t <= to; t++) {
			struct hvx_sched sc;
			unsigned int i;

			if (run_until_turn(&b, t))
				return 1;
			get_sched(&b, &sc);
			printf("turn %llu cur=%u:", (unsigned long long)t, sc.cur);
			for (i = 0; i < g_ncpus; i++) {
				struct hvx_regs r;
				struct hvx_sregs sr;

				set_vcpu(&b, i);
				ioctl(b.fd, HVX_GET_REGS, &r);
				ioctl(b.fd, HVX_GET_SREGS, &sr);
				printf(" v%u[h%u cr0=%llx efer=%llx cs=%x rip=%llx]", i, sc.v[i].halted,
				       (unsigned long long)sr.cr0, (unsigned long long)sr.efer, sr.cs.selector,
				       (unsigned long long)r.rip);
			}
			set_vcpu(&b, 0);
			printf("\n");
		}
		return 0;
	}
	if (!strcmp(mode, "apbranch"))
		return apbranch(mem_size, image, image_len, initrd, initrd_len, cmdline, &r1);
	if (getenv("HVX_BRANCH_TURNS")) {
		/* extra branch points (comma-separated turn numbers) */
		char *list = strdup(getenv("HVX_BRANCH_TURNS")), *tok, *save = NULL;
		int okx = 1;

		for (tok = strtok_r(list, ",", &save); tok; tok = strtok_r(NULL, ",", &save)) {
			char tag[64];

			snprintf(tag, sizeof(tag), "branch@turn%s", tok);
			okx &= branch_check(tag, strtoull(tok, NULL, 0), mem_size, image, image_len,
					    initrd, initrd_len, cmdline, &r1);
		}
		free(list);
		printf("hvx_smp_boot_verify: extra branches %s\n", okx ? "IDENTICAL" : "MISMATCH");
		if (!okx)
			return 1;
	}

	fresh_boot("boot #2", mem_size, image, image_len, initrd, initrd_len, cmdline, &r2,
		   smp_line2, sizeof(smp_line2));
	ok_whole = r1.ok && r2.ok && r1.fp == r2.fp;
	printf("hvx_smp_boot_verify: whole-boot fp1=%016llx fp2=%016llx -> %s\n",
	       (unsigned long long)r1.fp, (unsigned long long)r2.fp, ok_whole ? "IDENTICAL" : "MISMATCH");
	if (r1.ok && r2.ok && !ok_whole)
		localize("whole-boot", &r1, &r2);

	/* branch at the mid marker, and at the middle turn of boot #1 */
	ok_branch = branch_check("branch@marker", 0, mem_size, image, image_len, initrd, initrd_len,
				 cmdline, &r1);
	if (r1.ok && r1.sched.turns >= 2)
		ok_branch &= branch_check("branch@turn", r1.sched.turns / 2, mem_size, image, image_len,
					  initrd, initrd_len, cmdline, &r1);
	ok_smp = strstr(smp_line, want) != NULL;
	printf("hvx_smp_boot_verify: console \"%s\" -> %s\n", smp_line, ok_smp ? "ok" : "MISSING");
	if (ok_whole && ok_branch && ok_smp) {
		printf("hvx_smp_boot_verify: PASS (%u vCPUs)\n", g_ncpus);
		return 0;
	}
	printf("hvx_smp_boot_verify: FAIL\n");
	return 1;
}

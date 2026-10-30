// SPDX-License-Identifier: GPL-2.0
/*
 * turns_selftest - checks the several-vCPU snapshot / restore / fingerprint helpers that
 * hvx_explore and hvx_stack_verify use (smp_get, smp_set, fnv_smp, park_aps and
 * fingerprint() in hvx_stack_verify.c; hvx/explore/TURNS.md) on a hand-written two-vCPU
 * guest, since real Linux cannot start a second vCPU yet (SMP Stage 2).
 *
 * The guest is the seat-selling race of hvx_sched_race_test: both vCPUs sell seats from one
 * counter with a check-then-act window, so the result depends on where turns end. Checks:
 *   A  stop at a turn boundary (stop_turn), snapshot with the helpers, finish; restore the
 *      snapshot into a FRESH VM, finish: both equal the straight run (fingerprint over both
 *      vCPUs, the scheduler state and RAM), and vCPU 1 really ran.
 *   B  the same with a stop in the middle of a turn (PMU ticks every 997 branches and an
 *      HVX_RUNC budget), so the restored scheduler state has a turn in progress.
 *   C  the fingerprint sees vCPU 1: changing only vCPU 1's rax changes it.
 *   D  park_aps: a parked vCPU 1 (IF=0, halted, RIP on garbage) never runs while vCPU 0
 *      works and halts.
 *   E  seeded turn lengths (the qs= token's policy) give a different, reproducible result.
 *
 * Build on the box: cc -O2 -o turns_selftest turns_selftest.c   (in hvx/explore)
 * Run:  ~/bin/hvx-run sudo ./turns_selftest
 */
#define HVX_STACK_NO_MAIN
#include "../hvx_stack_verify.c"
#undef main

#define MEM	(256UL * 1024)
#define CODE	HVX_GUEST_CODE_BASE
#define SEATS_GPA 0x10000UL
#define SOLD_GPA  0x10100UL
#define SEATS	400

extern const uint8_t g_ts_start[], g_ts_end[];
__asm__(
	".pushsection .rodata\n"
	".globl g_ts_start\n"
	"g_ts_start:\n"
	"1:	mov %r8, %rcx\n"
	"2:	dec %rcx\n"
	"	jnz 2b\n"
	"	mov (%rbx), %rax\n"
	"	test %rax, %rax\n"
	"	jle 4f\n"
	"	mov %r9, %rcx\n"
	"3:	dec %rcx\n"
	"	jnz 3b\n"
	"	dec %rax\n"
	"	mov %rax, (%rbx)\n"
	"	incq (%r12)\n"
	"	jmp 1b\n"
	"4:	hlt\n"
	".globl g_ts_end\n"
	"g_ts_end:\n"
	".popsection\n");

static int g_fail;
#define CHECK(c, ...) do { if (!(c)) { g_fail = 1; printf("  FAIL: " __VA_ARGS__); printf("\n"); } } while (0)

/* A fresh 2-vCPU scheduler VM as a struct boot (no disk: p2_len 0, so fingerprint() hashes
 * console + every vCPU + scheduler + RAM). */
static void vm_new(struct boot *b)
{
	struct hvx_config cfg;
	memset(b, 0, sizeof(*b));
	b->fd = open(getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open"); exit(2); }
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = MEM;
	cfg.n_vcpus = g_vcpus;
	cfg.flags = HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) {
		if (errno == ENODEV) { printf("turns_selftest: SKIP (no usable PMU)\n"); exit(0); }
		perror("HVX_CREATE_VM"); exit(2);
	}
	b->mem_size = MEM;
	b->mem = mmap(NULL, MEM, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap"); exit(2); }
	b->ubuf = calloc(1, 16);
	b->disk_mm = b->mem;	/* p2_len = 0: no disk bytes are hashed */
}

static void vm_free(struct boot *b)
{
	munmap(b->mem, b->mem_size);
	close(b->fd);
	free(b->ubuf);
}

static void pmu_period(struct boot *b, uint64_t period)
{
	struct hvx_pmu pm;
	memset(&pm, 0, sizeof(pm));
	pm.period = period;
	if (ioctl(b->fd, HVX_SET_PMU, &pm) < 0) { perror("HVX_SET_PMU"); exit(2); }
}

static void policy(struct boot *b, uint64_t q, uint64_t seed, uint64_t stop_turn)
{
	struct hvx_sched c;
	if (ioctl(b->fd, HVX_GET_SCHED, &c) < 0) { perror("HVX_GET_SCHED"); exit(2); }
	c.quantum = q; c.seed = seed; c.stop_turn = stop_turn;
	if (ioctl(b->fd, HVX_SET_SCHED, &c) < 0) { perror("HVX_SET_SCHED"); exit(2); }
}

/* the race guest on both vCPUs (or, park=1, only on vCPU 0 with vCPU 1 parked) */
static void load_race(struct boot *b, int park)
{
	struct hvx_regs r;
	unsigned int i;
	memcpy(b->mem + CODE, g_ts_start, g_ts_end - g_ts_start);
	*(uint64_t *)(b->mem + SEATS_GPA) = SEATS;
	for (i = 0; i < 2; i++) {
		memset(&r, 0, sizeof(r));
		r.rip = CODE; r.rbx = SEATS_GPA; r.r12 = SOLD_GPA + 8 * i;
		r.r8 = 200; r.r9 = 1; r.rsp = 0x30000 + 0x1000 * i; r.rflags = 0x2;
		if (set_vcpu(b->fd, i) || ioctl(b->fd, HVX_SET_REGS, &r) < 0) { perror("SET_REGS"); exit(2); }
	}
	set_vcpu(b->fd, 0);
	if (park) {
		/* vCPU 1 parked at RIP 0 (page-table bytes: garbage if it ever ran) */
		if (park_aps(b->fd)) exit(2);
	}
}

/* run to the final HLT (THROTTLE transparent); returns the reason of the last exit */
static uint32_t run_end(struct boot *b, uint64_t budget, int once)
{
	for (;;) {
		uint32_t reason;
		int rc;
		if (budget) {
			struct hvx_run_ctl c;
			memset(&c, 0, sizeof(c));
			c.max_insns = budget;
			rc = ioctl(b->fd, HVX_RUNC, &c);
			reason = c.reason;
		} else {
			struct hvx_exit ex;
			memset(&ex, 0, sizeof(ex));
			rc = ioctl(b->fd, HVX_RUN, &ex);
			reason = ex.reason;
		}
		if (rc < 0) {
			if (errno == EINTR) continue;
			perror("run"); exit(2);
		}
		if (once) return reason;
		if (reason == HVX_EXIT_THROTTLE || reason == HVX_EXIT_STEP) continue;
		return reason;
	}
}

struct full { struct hvx_snapshot s0; struct smpstate smp; uint8_t *ram; };

static void take(struct boot *b, struct full *f)
{
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, &f->s0) < 0) { perror("GET_SNAPSHOT"); exit(2); }
	if (smp_get(b->fd, &f->smp)) exit(2);
	f->ram = malloc(MEM);
	memcpy(f->ram, b->mem, MEM);
}

static void put(struct boot *b, struct full *f)
{
	if (ioctl(b->fd, HVX_SET_SNAPSHOT, &f->s0) < 0) { perror("SET_SNAPSHOT"); exit(2); }
	if (smp_set(b->fd, &f->smp)) exit(2);
	memcpy(b->mem, f->ram, MEM);
}

static uint64_t sold(struct boot *b, int i) { return *(uint64_t *)(b->mem + SOLD_GPA + 8 * i); }

static void sched_of(struct boot *b, struct hvx_sched *c)
{
	if (ioctl(b->fd, HVX_GET_SCHED, c) < 0) { perror("HVX_GET_SCHED"); exit(2); }
}

/* one round of check A or B. midturn: stop with an HVX_RUNC budget (PMU ticks every 997
 * branches) instead of at a turn boundary. */
static void check_branch(const char *name, uint64_t q, uint64_t seed, int midturn)
{
	struct boot b;
	struct full f;
	struct hvx_sched c;
	uint64_t fp_straight, fp_cont, fp_rest, turns_all;
	int64_t lost;

	memset(&f, 0, sizeof(f));
	vm_new(&b); load_race(&b, 0);
	pmu_period(&b, midturn ? 997 : 1ULL << 40);
	policy(&b, q, seed, 0);
	CHECK(run_end(&b, 0, 0) == 12, "%s straight run did not end in HLT", name);
	fp_straight = fingerprint(&b);
	sched_of(&b, &c);
	turns_all = c.turns;
	lost = (int64_t)(sold(&b, 0) + sold(&b, 1)) - SEATS;
	CHECK(sold(&b, 1) > 0 && sold(&b, 0) > 0, "%s: both vCPUs must sell", name);
	vm_free(&b);

	vm_new(&b); load_race(&b, 0);
	pmu_period(&b, midturn ? 997 : 1ULL << 40);
	if (midturn) {
		uint32_t r;
		policy(&b, q, seed, 0);
		r = run_end(&b, 30, 1);	/* 30 events = 30 PMU ticks here */
		CHECK(r == HVX_EXIT_STEP || r == HVX_EXIT_THROTTLE, "%s: budget stop gave reason %#x", name, r);
	} else {
		policy(&b, q, seed, turns_all / 2);
		CHECK(run_end(&b, 0, 0) == HVX_EXIT_TURN, "%s: no stop at turn %llu", name, (unsigned long long)(turns_all / 2));
		policy(&b, q, seed, 0);
	}
	sched_of(&b, &c);
	printf("  %s: stopped at turn %llu of %llu, in_turn=%u cur=%u\n", name, (unsigned long long)c.turns,
	       (unsigned long long)turns_all, c.in_turn, c.cur);
	if (midturn) CHECK(c.in_turn == 1, "%s: the stop is not in the middle of a turn", name);
	take(&b, &f);
	CHECK(run_end(&b, 0, 0) == 12, "%s continued run did not end in HLT", name);
	fp_cont = fingerprint(&b);
	vm_free(&b);

	vm_new(&b);
	/* the tick period is policy, not state (not in any snapshot): a restoring harness sets
	 * it the same way the original VM got it, as hvx_explore's vm_fresh does */
	pmu_period(&b, midturn ? 997 : 1ULL << 40);
	put(&b, &f);
	CHECK(run_end(&b, 0, 0) == 12, "%s restored run did not end in HLT", name);
	fp_rest = fingerprint(&b);
	vm_free(&b);
	printf("  %s: Q=%llu seed=%llu lost=%lld turns=%llu fp straight=%016llx continued=%016llx restored=%016llx\n",
	       name, (unsigned long long)q, (unsigned long long)seed, (long long)lost, (unsigned long long)turns_all,
	       (unsigned long long)fp_straight, (unsigned long long)fp_cont, (unsigned long long)fp_rest);
	CHECK(fp_straight == fp_cont && fp_cont == fp_rest, "%s: fingerprints differ", name);
	smp_free(&f.smp);
	free(f.ram);
}

int main(void)
{
	struct boot b;
	struct hvx_sched c;
	uint64_t h1, h2, fa, fb;

	g_vcpus = 2;
	printf("A: stop at a turn boundary, snapshot both vCPUs + scheduler, restore in a fresh VM\n");
	check_branch("A", 1000, 1, 0);
	printf("B: stop in the middle of a turn (ticks every 997 branches, HVX_RUNC budget)\n");
	check_branch("B", 5000, 0, 1);

	printf("C: the fingerprint covers vCPU 1\n");
	vm_new(&b); load_race(&b, 0);
	h1 = fingerprint(&b);
	{
		struct hvx_regs r;
		set_vcpu(b.fd, 1);
		ioctl(b.fd, HVX_GET_REGS, &r);
		r.rax ^= 1;
		ioctl(b.fd, HVX_SET_REGS, &r);
		set_vcpu(b.fd, 0);
	}
	h2 = fingerprint(&b);
	printf("  C: before %016llx, after changing vCPU 1's rax %016llx\n", (unsigned long long)h1, (unsigned long long)h2);
	CHECK(h1 != h2 && h1 && h2, "C: vCPU 1 is not in the fingerprint");
	vm_free(&b);

	printf("D: a parked vCPU never runs\n");
	vm_new(&b); load_race(&b, 1);
	pmu_period(&b, 1ULL << 40);
	policy(&b, 1000, 0, 0);
	{
		uint32_t r = run_end(&b, 0, 0);
		CHECK(r == 12, "D: did not end in HLT (reason %#x)", r);
	}
	sched_of(&b, &c);
	printf("  D: sold %llu+%llu, vCPU 1 turns=%llu branches=%llu halted=%u, vCPU 0 turns=%llu\n",
	       (unsigned long long)sold(&b, 0), (unsigned long long)sold(&b, 1),
	       (unsigned long long)c.v[1].turns, (unsigned long long)c.v[1].branches, c.v[1].halted,
	       (unsigned long long)c.v[0].turns);
	CHECK(sold(&b, 0) == SEATS && sold(&b, 1) == 0 && c.v[1].turns == 0 && c.v[1].branches == 0 && c.v[1].halted,
	      "D: vCPU 1 ran");
	vm_free(&b);

	printf("E: seeded turn lengths are reproducible and differ from fixed ones\n");
	vm_new(&b); load_race(&b, 0); pmu_period(&b, 1ULL << 40); policy(&b, 1000, 0x5eed, 0);
	run_end(&b, 0, 0); fa = fingerprint(&b); vm_free(&b);
	vm_new(&b); load_race(&b, 0); pmu_period(&b, 1ULL << 40); policy(&b, 1000, 0x5eed, 0);
	run_end(&b, 0, 0); fb = fingerprint(&b); vm_free(&b);
	vm_new(&b); load_race(&b, 0); pmu_period(&b, 1ULL << 40); policy(&b, 1000, 0, 0);
	run_end(&b, 0, 0); h1 = fingerprint(&b); vm_free(&b);
	printf("  E: seeded %016llx / %016llx, fixed %016llx\n", (unsigned long long)fa, (unsigned long long)fb,
	       (unsigned long long)h1);
	CHECK(fa == fb && fa != h1, "E: seeded turns not reproducible or not different");

	printf(g_fail ? "turns_selftest: FAIL\n" : "turns_selftest: PASS\n");
	return g_fail;
}

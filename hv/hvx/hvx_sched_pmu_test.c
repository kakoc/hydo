// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sched_pmu_test - PMU accounting under the in-kernel vCPU scheduler (HVX_VM_SMP_SCHED).
 * Needs a bare-metal Intel PMU (SKIPs otherwise).
 *
 * Two vCPUs run   1: dec %rcx ; jnz 1b ; hlt   with rcx = N0 and N1: exactly N0 and N1
 * guest conditional branches. One HVX_RUN drives both, in turns of Q branches.
 *
 *   A. EXACT SPLIT  -- no ticks: each vCPU's own count is exactly N_i, the VM-wide count is
 *                      N0+N1, no turn end overshoots, and the turn hash (vCPU, rip, rcx,
 *                      rflags, count at every switch) is the same in 3 runs.
 *   B. TICKS        -- with a tick period P the VM takes floor((N0+N1)/P) ticks on ONE grid
 *                      (summed over both vCPUs), zero overshoots, identical landing hashes,
 *                      icount, TSC and turn hash in 3 runs.
 *   C. SHORT TURNS  -- the same with a short fixed turn, and with seeded turn lengths.
 *   D. SNAPSHOT     -- stop at a turn boundary, save, finish; restore, finish: both equal
 *                      the uninterrupted run's fingerprint.
 */
#include "hvx_sched_util.h"

#define MEM	(2UL * 1024 * 1024)
#define CODE	HVX_GUEST_CODE_BASE

struct res {
	uint64_t own[2], total, ticks, over, land[2], icount, tsc, turns, thash, sover, fp, rcx[2];
};

static int open_vm(struct su_vm *vm)
{
	static const uint8_t code[] = {
		0x48, 0xff, 0xc9,	/* 1: dec %rcx */
		0x75, 0xfb,		/* jnz 1b */
		0xf4,			/* hlt */
	};

	if (su_create(vm, MEM, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED) < 0) {
		if (errno == ENODEV) {
			printf("hvx_sched_pmu_test: SKIP (no usable PMU on this host)\n");
			exit(0);
		}
		su_die("HVX_CREATE_VM");
	}
	memcpy(vm->mem + CODE, code, sizeof(code));
	return 0;
}

static void start(struct su_vm *vm, uint64_t n0, uint64_t n1, uint64_t period,
		  uint64_t q, uint64_t seed)
{
	struct hvx_regs r;
	struct hvx_tsc t;
	uint64_t n[2] = { n0, n1 };
	unsigned int i;

	for (i = 0; i < 2; i++) {
		memset(&r, 0, sizeof(r));
		r.rip = CODE;
		r.rcx = n[i];
		r.rsp = MEM - 16 - 0x1000 * i;
		r.rflags = 0x2;
		su_set_regs(vm, i, &r);
	}
	memset(&t, 0, sizeof(t));
	t.tsc = 1000;
	t.step = 1;
	t.mode = HVX_TSC_COUNTER;
	if (ioctl(vm->fd, HVX_SET_TSC, &t) < 0)
		su_die("SET_TSC");
	su_pmu(vm, period, 1000000);
	su_policy(vm, q, seed, 0);
}

static void collect(struct su_vm *vm, struct res *o)
{
	struct hvx_sched s;
	struct hvx_pmu pm;
	struct hvx_regs r;
	unsigned int i;

	memset(o, 0, sizeof(*o));
	su_get_sched(vm, &s);
	for (i = 0; i < 2; i++) {
		su_set_vcpu(vm, i);
		if (ioctl(vm->fd, HVX_GET_PMU, &pm) < 0)
			su_die("GET_PMU");
		if (ioctl(vm->fd, HVX_GET_REGS, &r) < 0)
			su_die("GET_REGS");
		o->own[i] = s.v[i].branches;
		o->ticks += pm.ticks;
		o->over += pm.overshoots;
		o->land[i] = pm.land_hash;
		o->rcx[i] = r.rcx;
	}
	o->total = s.pmu_total;
	o->icount = s.icount;
	o->tsc = s.tsc;
	o->turns = s.turns;
	o->thash = s.turn_hash;
	o->sover = s.overshoots;
	o->fp = su_fingerprint(vm);
}

static void one(uint64_t n0, uint64_t n1, uint64_t period, uint64_t q, uint64_t seed, struct res *o)
{
	struct su_vm vm;

	open_vm(&vm);
	start(&vm, n0, n1, period, q, seed);
	if (su_run(&vm, NULL) != SU_EXIT_HLT) {
		fprintf(stderr, "run did not end in HLT\n");
		exit(1);
	}
	collect(&vm, o);
	su_destroy(&vm);
}

static void show(const char *tag, int i, const struct res *r)
{
	printf("%s run %d: own=%" PRIu64 "+%" PRIu64 " total=%" PRIu64 " ticks=%" PRIu64
	       " over=%" PRIu64 "/%" PRIu64 " turns=%" PRIu64 " turn_hash=%016" PRIx64
	       " land=%016" PRIx64 "/%016" PRIx64 " icount=%" PRIu64 " tsc=%" PRIu64 " fp=%016" PRIx64 "\n",
	       tag, i, r->own[0], r->own[1], r->total, r->ticks, r->over, r->sover, r->turns, r->thash,
	       r->land[0], r->land[1], r->icount, r->tsc, r->fp);
}

/* 3 runs of one configuration: exact split, expected ticks, identical across runs */
static int check3(const char *tag, uint64_t n0, uint64_t n1, uint64_t period, uint64_t q, uint64_t seed)
{
	struct res r[3];
	int i, ok = 1;
	uint64_t want_ticks = period >= (1ULL << 40) ? 0 : (n0 + n1) / period;

	for (i = 0; i < 3; i++) {
		one(n0, n1, period, q, seed, &r[i]);
		show(tag, i, &r[i]);
		if (r[i].own[0] != n0 || r[i].own[1] != n1 || r[i].total != n0 + n1 ||
		    r[i].rcx[0] || r[i].rcx[1] || r[i].ticks != want_ticks || r[i].over || r[i].sover)
			ok = 0;
		if (memcmp(&r[i], &r[0], sizeof(r[0])))
			ok = 0;
	}
	printf("%s: %s (want own=%" PRIu64 "+%" PRIu64 " ticks=%" PRIu64 ", identical x3)\n",
	       tag, ok ? "OK" : "FAIL", n0, n1, want_ticks);
	return ok;
}

int main(void)
{
	const uint64_t N0 = 150000000ULL, N1 = 50000017ULL, P = 999983ULL;
	int ok = 1;

	ok &= check3("A no-ticks Q=1000003", N0, N1, 1ULL << 40, 1000003, 0);
	ok &= check3("B ticks  Q=1000003", N0, N1, P, 1000003, 0);
	ok &= check3("C1 ticks Q=10007", 20000003ULL, 7000001ULL, 99991, 10007, 0);
	ok &= check3("C2 ticks seeded Q<=50000", 20000003ULL, 7000001ULL, 99991, 50000, 0x5eed);

	{	/* D: stop at turn T/2, snapshot, finish; restore, finish */
		struct su_vm vm;
		struct su_snap sn;
		struct res full, a, b;
		uint64_t half;

		memset(&sn, 0, sizeof(sn));
		one(20000003ULL, 7000001ULL, 99991, 10007, 0x77, &full);
		half = full.turns / 2;
		open_vm(&vm);
		start(&vm, 20000003ULL, 7000001ULL, 99991, 10007, 0x77);
		su_stop_turn(&vm, half);
		if (su_run(&vm, NULL) != HVX_EXIT_TURN) {
			fprintf(stderr, "D: no stop at turn %" PRIu64 "\n", half);
			return 1;
		}
		su_save(&vm, &sn);
		su_stop_turn(&vm, 0);
		su_run(&vm, NULL);
		collect(&vm, &a);
		su_restore(&vm, &sn, 0);
		su_run(&vm, NULL);
		collect(&vm, &b);
		printf("D snapshot at turn %" PRIu64 "/%" PRIu64 ": full fp=%016" PRIx64 " straight=%016" PRIx64
		       " restored=%016" PRIx64 "\n", half, full.turns, full.fp, a.fp, b.fp);
		if (a.fp != full.fp || b.fp != full.fp || a.own[0] != full.own[0] || b.own[1] != full.own[1])
			ok = 0;
		su_destroy(&vm);
		free(sn.ram);
	}
	printf(ok ? "hvx_sched_pmu_test: PASS (per-vCPU branch counts add up exactly, one tick grid, "
		    "identical landings and turns, snapshot-safe)\n"
		  : "hvx_sched_pmu_test: FAIL\n");
	return ok ? 0 : 1;
}

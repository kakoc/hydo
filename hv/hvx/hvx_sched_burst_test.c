// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sched_burst_test - seeded burst turns of the in-kernel vCPU scheduler
 * (HVX_SET_TURNCTL, hv/hvx/FASTTURN.md). Needs a bare-metal Intel PMU (SKIPs otherwise).
 *
 * Two vCPUs run   1: dec %rcx ; jnz 1b ; hlt   (N0 and N1 guest conditional branches),
 * long turns of Q branches with seeded bursts of short turns (B branches per burst, turns
 * of s branches, a burst in `permille` of the windows), PMU ticks on.
 *
 *   A. MODEL    -- the turn count, the number of turns that started in a burst, and the
 *                  number of long turns cut at a burst's start equal a userspace model of
 *                  the length rule (struct hvx_turnctl in hvx_abi.h); 3 runs identical.
 *   B. BRANCH   -- stop at a turn that starts INSIDE a burst and at one OUTSIDE any burst:
 *                  save, finish; restore (same VM, and a fresh VM with the policy set
 *                  again), finish: all equal the straight run's fingerprint.
 *   C. MID-TURN -- stop after k ticks (HVX_RUNC max_insns) inside a burst; same check.
 *   D. CONTROL  -- without bursts the run differs (fewer turns, another fingerprint); a
 *                  1-vCPU VM refuses HVX_SET_TURNCTL; the burst policy is not part of the
 *                  scheduler state (GET_SCHED is unchanged by SET_TURNCTL).
 *   E. MARGIN   -- the same burst run with turn-end margins 256, 160, 128: same turn hash
 *                  and fingerprint (turn ends are exact whatever the margin).
 */
#include "hvx_sched_util.h"

#define MEM	(2UL * 1024 * 1024)
#define CODE	HVX_GUEST_CODE_BASE

static const uint64_t N0 = 3000000ULL, N1 = 2000017ULL, P = 99991ULL;
static const uint64_t Q = 100000ULL;
static struct hvx_turnctl g_tc = {
	.burst_seed = 0xb0b5eedULL, .burst_len = 20000, .burst_q = 97, .burst_permille = 300,
};

struct res {
	uint64_t turns, thash, total, fp, bturns, clipped, over, sover;
};

static uint64_t mix(uint64_t x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}

/* userspace copy of the module's burst rule */
static int zone(const struct hvx_turnctl *t, uint64_t w, uint64_t W, uint64_t *zs)
{
	uint64_t h = mix(t->burst_seed + (w + 1) * 0x9e3779b97f4a7c15ULL);

	if (h % 1000 >= t->burst_permille)
		return 0;
	*zs = w * W + (h >> 16) % (W - t->burst_len + 1);
	return 1;
}

static uint64_t len_at(const struct hvx_turnctl *t, uint64_t q, uint64_t x, uint64_t *bt, uint64_t *cl)
{
	uint64_t W = t->burst_window, len = q, w, k, zs;

	if (q > W) W = q;
	if (t->burst_len > W) W = t->burst_len;
	if (!t->burst_seed || !t->burst_permille)
		return len;
	w = x / W;
	if (zone(t, w, W, &zs) && x >= zs && x < zs + t->burst_len) {
		(*bt)++;
		return t->burst_q < zs + t->burst_len - x ? t->burst_q : zs + t->burst_len - x;
	}
	for (k = w; k <= w + 1; k++)
		if (zone(t, k, W, &zs) && zs > x) {
			if (zs - x < len) {
				len = zs - x;
				(*cl)++;
			}
			break;
		}
	return len;
}

/*
 * The schedule of this guest under the rule: round-robin from vCPU 0, a vCPU whose
 * remaining count is < the turn length (or == 0) runs into its HLT, which ends the turn
 * and halts it for good (IF=0). in_burst[t] records whether turn t started in a burst.
 */
static void model(const struct hvx_turnctl *t, struct res *o, uint8_t *in_burst, uint64_t cap)
{
	uint64_t rem[2] = { N0, N1 }, x = 0, turns = 0, bt = 0, cl = 0;
	int halted[2] = { 0, 0 }, cur = 1;

	for (;;) {
		int v = -1, k;
		uint64_t len, b0 = bt;

		for (k = 1; k <= 2; k++)
			if (!halted[(cur + k) % 2]) {
				v = (cur + k) % 2;
				break;
			}
		if (v < 0)
			break;
		cur = v;
		len = len_at(t, Q, x, &bt, &cl);
		if (turns < cap)
			in_burst[turns] = bt != b0;
		if (rem[v] >= len && rem[v]) {
			rem[v] -= len;		/* ends right after its len-th branch */
			x += len;
		} else {
			x += rem[v];		/* runs into the HLT */
			rem[v] = 0;
			halted[v] = 1;
		}
		turns++;
	}
	memset(o, 0, sizeof(*o));
	o->turns = turns;
	o->bturns = bt;
	o->clipped = cl;
	o->total = x;
}

static void open_vm(struct su_vm *vm)
{
	static const uint8_t code[] = { 0x48, 0xff, 0xc9, 0x75, 0xfb, 0xf4 };

	if (su_create(vm, MEM, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED) < 0) {
		if (errno == ENODEV) {
			printf("hvx_sched_burst_test: SKIP (no usable PMU on this host)\n");
			exit(0);
		}
		su_die("HVX_CREATE_VM");
	}
	memcpy(vm->mem + CODE, code, sizeof(code));
}

static void set_tc(struct su_vm *vm, const struct hvx_turnctl *t)
{
	if (ioctl(vm->fd, HVX_SET_TURNCTL, t) < 0)
		su_die("HVX_SET_TURNCTL");
}

static void start(struct su_vm *vm, const struct hvx_turnctl *t)
{
	struct hvx_regs r;
	struct hvx_tsc ts;
	uint64_t n[2] = { N0, N1 };
	unsigned int i;

	for (i = 0; i < 2; i++) {
		memset(&r, 0, sizeof(r));
		r.rip = CODE;
		r.rcx = n[i];
		r.rsp = MEM - 16 - 0x1000 * i;
		r.rflags = 0x2;
		su_set_regs(vm, i, &r);
	}
	memset(&ts, 0, sizeof(ts));
	ts.tsc = 1000;
	ts.step = 1;
	ts.mode = HVX_TSC_COUNTER;
	if (ioctl(vm->fd, HVX_SET_TSC, &ts) < 0)
		su_die("SET_TSC");
	su_pmu(vm, P, 1000000);
	su_policy(vm, Q, 0, 0);
	if (t)
		set_tc(vm, t);
}

static void collect(struct su_vm *vm, struct res *o)
{
	struct hvx_sched s;
	struct hvx_turnctl t;
	struct hvx_pmu pm;
	unsigned int i;

	memset(o, 0, sizeof(*o));
	su_get_sched(vm, &s);
	for (i = 0; i < 2; i++) {
		su_set_vcpu(vm, i);
		if (ioctl(vm->fd, HVX_GET_PMU, &pm) < 0)
			su_die("GET_PMU");
		o->over += pm.overshoots;
	}
	if (ioctl(vm->fd, HVX_GET_TURNCTL, &t) < 0)
		su_die("HVX_GET_TURNCTL");
	o->turns = s.turns;
	o->thash = s.turn_hash;
	o->total = s.pmu_total;
	o->sover = s.overshoots;
	o->bturns = t.burst_turns;
	o->clipped = t.clipped;
	o->fp = su_fingerprint(vm);
}

static void finish(struct su_vm *vm)
{
	if (su_run(vm, NULL) != SU_EXIT_HLT) {
		fprintf(stderr, "run did not end in HLT\n");
		exit(1);
	}
}

static void one(const struct hvx_turnctl *t, struct res *o)
{
	struct su_vm vm;

	open_vm(&vm);
	start(&vm, t);
	finish(&vm);
	collect(&vm, o);
	su_destroy(&vm);
}

static void show(const char *tag, const struct res *r)
{
	printf("%s: turns=%" PRIu64 " burst_turns=%" PRIu64 " clipped=%" PRIu64 " total=%" PRIu64
	       " overshoots=%" PRIu64 "/%" PRIu64 " turn_hash=%016" PRIx64 " fp=%016" PRIx64 "\n",
	       tag, r->turns, r->bturns, r->clipped, r->total, r->over, r->sover, r->thash, r->fp);
}

/* stop at turn T (or after k ticks), save; finish (A); restore into the same VM and a
 * fresh VM with the policy set again, finish (B, C); all must equal `full` */
static int branch_check(const char *tag, uint64_t T, uint64_t ticks, const struct res *full)
{
	struct su_vm vm, vm2;
	struct su_snap sn;
	struct res a, b, c;
	uint32_t why;
	int ok;

	memset(&sn, 0, sizeof(sn));
	open_vm(&vm);
	start(&vm, &g_tc);
	if (T) {
		su_stop_turn(&vm, T);
		why = su_run(&vm, NULL);
		if (why != HVX_EXIT_TURN) {
			fprintf(stderr, "%s: no stop at turn %" PRIu64 "\n", tag, T);
			exit(1);
		}
	} else {
		why = su_runc(&vm, ticks);
		if (why != HVX_EXIT_STEP) {
			fprintf(stderr, "%s: no max_insns stop (reason 0x%x)\n", tag, why);
			exit(1);
		}
	}
	su_save(&vm, &sn);
	su_stop_turn(&vm, 0);
	finish(&vm);
	collect(&vm, &a);
	su_restore(&vm, &sn, 0);
	finish(&vm);
	collect(&vm, &b);
	open_vm(&vm2);
	start(&vm2, &g_tc);	/* registers etc. are overwritten by the restore */
	su_restore(&vm2, &sn, 0);
	finish(&vm2);
	collect(&vm2, &c);
	ok = a.fp == full->fp && b.fp == full->fp && c.fp == full->fp &&
	     a.thash == full->thash && b.thash == full->thash && c.thash == full->thash;
	printf("%s (in_turn=%u turn=%" PRIu64 "): straight=%016" PRIx64 " restored=%016" PRIx64
	       " fresh-VM=%016" PRIx64 " full=%016" PRIx64 " -> %s\n", tag, sn.sch.in_turn,
	       (uint64_t)sn.sch.turns, a.fp, b.fp, c.fp, full->fp, ok ? "IDENTICAL" : "MISMATCH");
	su_destroy(&vm2);
	su_destroy(&vm);
	free(sn.ram);
	return ok;
}

int main(void)
{
	struct res m, r[3], off;
	uint8_t *inb = calloc(1, 1 << 20);
	uint64_t t, tin = 0, tout = 0;
	int ok = 1, i;

	if (!inb)
		su_die("calloc");
	/* A: model + 3 identical runs */
	model(&g_tc, &m, inb, 1 << 20);
	show("A model", &m);
	for (i = 0; i < 3; i++) {
		char tag[32];

		one(&g_tc, &r[i]);
		snprintf(tag, sizeof(tag), "A run %d", i);
		show(tag, &r[i]);
		if (r[i].turns != m.turns || r[i].bturns != m.bturns || r[i].clipped != m.clipped ||
		    r[i].total != N0 + N1 || r[i].over || r[i].sover || memcmp(&r[i], &r[0], sizeof(r[0])))
			ok = 0;
	}
	printf("A: %s (model = module, 3 runs identical, no overshoots)\n", ok ? "OK" : "FAIL");

	/* B: a turn inside a burst and one outside, from the model, in the middle of the run */
	for (t = m.turns / 3; t < m.turns && t < (1 << 20) && !(tin && tout); t++) {
		if (inb[t] && inb[t - 1] && !tin)
			tin = t;	/* turn t (0-based) starts in a burst: stop after t turns */
		if (!inb[t] && !inb[t - 1] && !tout)
			tout = t;
	}
	if (!tin || !tout) {
		printf("B: FAIL (no burst / no long turn found in the model)\n");
		ok = 0;
	} else {
		ok &= branch_check("B branch inside a burst", tin, 0, &r[0]);
		ok &= branch_check("B branch outside bursts", tout, 0, &r[0]);
	}
	/* C: mid-turn stops after k ticks */
	ok &= branch_check("C mid-turn after 7 ticks", 0, 7, &r[0]);
	ok &= branch_check("C mid-turn after 23 ticks", 0, 23, &r[0]);

	/* D: control */
	{
		struct hvx_turnctl none;
		struct su_vm vm;
		struct hvx_config cfg;
		struct hvx_sched s1, s2;
		int fd, rc;

		memset(&none, 0, sizeof(none));
		one(&none, &off);
		show("D bursts off", &off);
		if (off.turns >= r[0].turns || off.fp == r[0].fp || off.bturns)
			ok = 0;
		open_vm(&vm);
		start(&vm, NULL);
		su_get_sched(&vm, &s1);
		set_tc(&vm, &g_tc);
		su_get_sched(&vm, &s2);
		if (memcmp(&s1, &s2, sizeof(s1))) {
			printf("D: FAIL (SET_TURNCTL changed the scheduler state)\n");
			ok = 0;
		}
		su_destroy(&vm);
		fd = open(getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx", O_RDWR);
		memset(&cfg, 0, sizeof(cfg));
		cfg.mem_size = MEM;
		cfg.n_vcpus = 1;
		cfg.flags = HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED;
		if (fd < 0 || ioctl(fd, HVX_CREATE_VM, &cfg) < 0)
			su_die("1-vCPU CREATE_VM");
		rc = ioctl(fd, HVX_SET_TURNCTL, &g_tc);
		printf("D: 1-vCPU VM SET_TURNCTL -> %s\n", rc < 0 && errno == EINVAL ? "EINVAL (refused)" : "ACCEPTED");
		if (!(rc < 0 && errno == EINVAL))
			ok = 0;
		close(fd);
	}

	/* E: margins */
	{
		static const uint64_t mg[] = { 256, 160, 128 };
		struct res e;

		for (i = 0; i < 3; i++) {
			struct hvx_turnctl t2 = g_tc;
			char tag[32];

			t2.margin = mg[i];
			one(&t2, &e);
			snprintf(tag, sizeof(tag), "E margin %" PRIu64, mg[i]);
			show(tag, &e);
			if (e.fp != r[0].fp || e.thash != r[0].thash || e.over || e.sover)
				ok = 0;
		}
	}
	printf(ok ? "hvx_sched_burst_test: PASS (burst turns follow the rule, replay and branch "
		    "inside and outside bursts identical, fp=%016" PRIx64 ")\n"
		  : "hvx_sched_burst_test: FAIL (fp=%016" PRIx64 ")\n", r[0].fp);
	free(inb);
	return ok ? 0 : 1;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * sweep_diag - M156: hvx_sched_sweep_test's main sweep (replay + branch at turn T/2 for every
 * turn length Q) with diagnostics, to locate a rare branch-from-snapshot mismatch. The VMs it
 * creates and the ioctls it makes are the test's own, in the test's order (it includes the test
 * with SWEEP_NO_MAIN); it only reads more state (HVX_GET_SCHED, HVX_GET_PMU, HVX_GET_FTSTAT,
 * HVX_GET_SNAPSHOT) after each run, never between a save and its runs.
 *
 * Per run it records the scheduler's overshoots/deferred counters and, per vCPU, the PMU's tick
 * overshoots, landing steps and hops (deltas over the run) and the overflow skid maximum. On a
 * mismatch it prints them for all four runs of that Q (a, b, the continuation after the save
 * "fa", the run after the restore "fb") and the fields of the final states that differ.
 *
 * Usage: sweep_diag [QMIN [QMAX [REPS]]]   (default 1 2000 1: the test's sweep; REPS > 1 repeats
 * the range, to measure the rate at a few Q; $HVX_DEV as the test)
 */
#define SWEEP_NO_MAIN
#include "../hvx_sched_sweep_test.c"

struct dstat {
	uint64_t fp, turns, turn_hash, sover, deferred, wakeups, jumps, guard, pmu_total;
	uint64_t over[2], steps[2], hops[2], ticks[2], skid_max[2], rearms[2], ovf[2];
};

struct full {
	struct su_snap s;
	struct hvx_sched sch;
};

static void pmu_of(struct su_vm *vm, unsigned int i, struct hvx_pmu *p)
{
	su_set_vcpu(vm, i);
	memset(p, 0, sizeof(*p));
	if (ioctl(vm->fd, HVX_GET_PMU, p) < 0)
		su_die("HVX_GET_PMU");
}

/* the cumulative per-vCPU counters now (pmu stats are not in a snapshot) */
static void cum(struct su_vm *vm, struct dstat *d)
{
	struct hvx_ftstat ft;
	struct hvx_pmu p;
	unsigned int i;

	memset(&ft, 0, sizeof(ft));
	if (ioctl(vm->fd, HVX_GET_FTSTAT, &ft) < 0)
		memset(&ft, 0, sizeof(ft));
	for (i = 0; i < 2; i++) {
		pmu_of(vm, i, &p);
		d->over[i] = p.overshoots;
		d->steps[i] = p.steps;
		d->hops[i] = p.hops;
		d->ticks[i] = p.ticks;
		d->skid_max[i] = ft.v[i].skid_max;
		d->rearms[i] = ft.v[i].rearms;
		d->ovf[i] = ft.v[i].ovf;
	}
}

/* the run's statistics: base = the counters before the run */
static void stat_of(struct su_vm *vm, const struct dstat *base, struct dstat *d)
{
	struct hvx_sched s;
	unsigned int i;

	cum(vm, d);
	for (i = 0; i < 2; i++) {
		d->over[i] -= base->over[i];
		d->steps[i] -= base->steps[i];
		d->hops[i] -= base->hops[i];
		d->ticks[i] -= base->ticks[i];
		d->rearms[i] -= base->rearms[i];
		d->ovf[i] -= base->ovf[i];
	}
	su_get_sched(vm, &s);
	d->fp = su_fingerprint(vm);
	d->turns = s.turns;
	d->turn_hash = s.turn_hash;
	d->sover = s.overshoots;
	d->deferred = s.deferred;
	d->wakeups = s.wakeups;
	d->jumps = s.time_jumps;
	d->guard = s.guard;
	d->pmu_total = s.pmu_total;
}

static void pr_stat(const char *tag, const struct dstat *d)
{
	printf("  %-3s fp=%016" PRIx64 " turns=%" PRIu64 " turn_hash=%016" PRIx64 " pmu_total=%" PRIu64
	       " sched_over=%" PRIu64 " deferred=%" PRIu64 " wakeups=%" PRIu64 " jumps=%" PRIu64
	       " | v0 over=%" PRIu64 " steps=%" PRIu64 " hops=%" PRIu64 " ticks=%" PRIu64 " skidmax=%" PRIu64
	       " ovf=%" PRIu64 " | v1 over=%" PRIu64 " steps=%" PRIu64 " hops=%" PRIu64 " ticks=%" PRIu64
	       " skidmax=%" PRIu64 " ovf=%" PRIu64 "\n",
	       tag, d->fp, d->turns, d->turn_hash, d->pmu_total, d->sover, d->deferred, d->wakeups, d->jumps,
	       d->over[0], d->steps[0], d->hops[0], d->ticks[0], d->skid_max[0], d->ovf[0],
	       d->over[1], d->steps[1], d->hops[1], d->ticks[1], d->skid_max[1], d->ovf[1]);
}

static void save_full(struct su_vm *vm, struct full *f)
{
	su_save(vm, &f->s);
	su_get_sched(vm, &f->sch);
}

#define U(x) ((uint64_t)(x))
#define DF(name, field) do { if (memcmp(&x->field, &y->field, sizeof(x->field))) \
	printf("    v%u %s differs\n", i, name); } while (0)

/* which parts of two final states differ */
static void diff_full(const char *what, const struct full *a, const struct full *b, uint64_t mem)
{
	unsigned int i, shown = 0;
	uint64_t k, nram = 0;

	printf("  diff %s:\n", what);
	for (i = 0; i < 2; i++) {
		const struct hvx_snapshot *x = &a->s.v[i], *y = &b->s.v[i];
		const uint8_t *p = (const void *)x, *q = (const void *)y;
		size_t o, n = offsetof(struct hvx_snapshot, xcr0);

		if (!memcmp(x, y, n))
			continue;
		printf("    v%u: rip %" PRIx64 "/%" PRIx64 " rcx %" PRIx64 "/%" PRIx64 " rflags %" PRIx64 "/%" PRIx64
		       " r13 %" PRIx64 "/%" PRIx64 " tsc %" PRIu64 "/%" PRIu64 " icount %" PRIu64 "/%" PRIu64
		       " pmu_total %" PRIu64 "/%" PRIu64 " pmu_next %" PRIu64 "/%" PRIu64 " deadline %" PRIu64
		       "/%" PRIu64 " in_service %u/%u inject %u:%u@%" PRIu64 "/%u:%u@%" PRIu64 "\n", i,
		       (uint64_t)x->regs.rip, (uint64_t)y->regs.rip, (uint64_t)x->regs.rcx, (uint64_t)y->regs.rcx,
		       (uint64_t)x->regs.rflags, (uint64_t)y->regs.rflags, (uint64_t)x->regs.r13,
		       (uint64_t)y->regs.r13, (uint64_t)x->tsc, (uint64_t)y->tsc, (uint64_t)x->icount,
		       (uint64_t)y->icount, (uint64_t)x->pmu_total, (uint64_t)y->pmu_total,
		       (uint64_t)x->pmu_next, (uint64_t)y->pmu_next, (uint64_t)x->lapic_tsc_deadline,
		       (uint64_t)y->lapic_tsc_deadline, x->lapic_in_service, y->lapic_in_service,
		       x->inject_valid, x->inject_vector, (uint64_t)x->inject_at_icount, y->inject_valid,
		       y->inject_vector, (uint64_t)y->inject_at_icount);
		DF("regs", regs);
		DF("sregs", sregs);
		DF("lapic_reg", lapic_reg);
		DF("irr", irr);
		DF("fx", fx);
		DF("sw_msr", sw_msr);
		for (o = 0, shown = 0; o < n && shown < 12; o++)
			if (p[o] != q[o]) {
				printf("    v%u byte +%zu: %02x/%02x\n", i, o, p[o], q[o]);
				shown++;
			}
	}
	{
		const struct hvx_sched *x = &a->sch, *y = &b->sch;

		printf("    sched: cur %u/%u in_turn %u/%u turn_over %u/%u pause_run %u/%u turn_end %" PRIu64 "/%" PRIu64
		       " turns %" PRIu64 "/%" PRIu64 " guard %" PRIu64 "/%" PRIu64 " turn_hash %016" PRIx64 "/%016" PRIx64
		       " tsc %" PRIu64 "/%" PRIu64 " icount %" PRIu64 "/%" PRIu64 " pmu_total %" PRIu64 "/%" PRIu64
		       " pmu_next %" PRIu64 "/%" PRIu64 " wakeups %" PRIu64 "/%" PRIu64 " jumps %" PRIu64 "/%" PRIu64 "\n",
		       x->cur, y->cur, x->in_turn, y->in_turn, x->turn_over, y->turn_over, x->pause_run, y->pause_run,
		       U(x->turn_end), U(y->turn_end), U(x->turns), U(y->turns), U(x->guard), U(y->guard),
		       U(x->turn_hash), U(y->turn_hash), U(x->tsc), U(y->tsc), U(x->icount), U(y->icount),
		       U(x->pmu_total), U(y->pmu_total), U(x->pmu_next), U(y->pmu_next), U(x->wakeups),
		       U(y->wakeups), U(x->time_jumps), U(y->time_jumps));
		for (i = 0; i < 2; i++)
			printf("    sched v%u: branches %" PRIu64 "/%" PRIu64 " turns %" PRIu64 "/%" PRIu64
			       " halted %u/%u reinj %u:%x/%u:%x\n", i, (uint64_t)x->v[i].branches,
			       (uint64_t)y->v[i].branches, (uint64_t)x->v[i].turns, (uint64_t)y->v[i].turns,
			       x->v[i].halted, y->v[i].halted, x->v[i].reinj_valid, x->v[i].reinj_info,
			       y->v[i].reinj_valid, y->v[i].reinj_info);
	}
	for (k = 0, shown = 0; k < mem; k++)
		if (a->s.ram[k] != b->s.ram[k]) {
			nram++;
			if (shown++ < 16)
				printf("    ram +%#" PRIx64 ": %02x/%02x\n", k, a->s.ram[k], b->s.ram[k]);
		}
	printf("    ram bytes differing: %" PRIu64 "\n", nram);
}

static void run_full(uint64_t q, struct dstat *d, struct full *f)
{
	struct su_vm vm;
	struct dstat base;

	setup(&vm, q);
	cum(&vm, &base);
	su_run(&vm, NULL);
	stat_of(&vm, &base, d);
	save_full(&vm, f);
	su_destroy(&vm);
}

/* branch_at(q, k_turn, 0, ...) with the statistics of the three parts */
static int branch_diag(uint64_t q, uint64_t k_turn, struct dstat *ds, struct dstat *da, struct dstat *db,
		       struct full *fa, struct full *fb)
{
	struct su_vm vm;
	struct su_snap sn;
	struct dstat base;
	uint32_t why;

	memset(&sn, 0, sizeof(sn));
	setup(&vm, q);
	cum(&vm, &base);
	su_stop_turn(&vm, k_turn);
	why = su_run(&vm, NULL);
	su_stop_turn(&vm, 0);
	if (why != HVX_EXIT_TURN) {
		su_destroy(&vm);
		return -1;
	}
	stat_of(&vm, &base, ds);
	su_save(&vm, &sn);
	cum(&vm, &base);
	su_run(&vm, NULL);
	stat_of(&vm, &base, da);
	save_full(&vm, fa);
	su_restore(&vm, &sn, 0);
	cum(&vm, &base);
	su_run(&vm, NULL);
	stat_of(&vm, &base, db);
	save_full(&vm, fb);
	su_destroy(&vm);
	free(sn.ram);
	return 1;
}

int main(int argc, char **argv)
{
	uint64_t qmin = argc > 1 ? strtoull(argv[1], NULL, 0) : 1;
	uint64_t qmax = argc > 2 ? strtoull(argv[2], NULL, 0) : 2000, q;
	uint64_t reps = argc > 3 ? strtoull(argv[3], NULL, 0) : 1, r;
	uint64_t bad = 0, runs = 0, over_runs = 0, sover_runs = 0, h = 0xcbf29ce484222325ULL;
	uint64_t smax = 0;
	static struct full fA, fB, fa, fb;

	setvbuf(stdout, NULL, _IOLBF, 0);
	for (r = 0; r < reps; r++)
	for (q = qmin; q <= qmax; q++) {
		struct dstat a, b, s, da, db;
		const struct dstat *all[4] = { &a, &b, &da, &db };
		int br, j, any_over = 0;

		run_full(q, &a, &fA);
		run_full(q, &b, &fB);
		br = branch_diag(q, a.turns / 2 ? a.turns / 2 : 1, &s, &da, &db, &fa, &fb);
		h = su_fnv(h, &a.fp, sizeof(a.fp));
		for (j = 0; j < 4; j++) {
			runs++;
			if (all[j]->over[0] || all[j]->over[1]) {
				over_runs++;
				any_over = 1;
			}
			if (all[j]->sover) {
				sover_runs++;
				any_over = 1;
			}
			if (all[j]->skid_max[0] > smax)
				smax = all[j]->skid_max[0];
			if (all[j]->skid_max[1] > smax)
				smax = all[j]->skid_max[1];
		}
		if (a.fp != b.fp || br != 1 || da.fp != a.fp || db.fp != a.fp || any_over) {
			if (a.fp != b.fp || br != 1 || da.fp != a.fp || db.fp != a.fp)
				bad++;
			printf("rep %" PRIu64 " Q=%" PRIu64 " %s: a=%016" PRIx64 " b=%016" PRIx64 " fa=%016" PRIx64 " fb=%016" PRIx64 "\n",
			       r, q, (a.fp != b.fp || br != 1 || da.fp != a.fp || db.fp != a.fp) ? "MISMATCH" : "overshoot",
			       a.fp, b.fp, da.fp, db.fp);
			pr_stat("a", &a);
			pr_stat("b", &b);
			pr_stat("s", &s);
			pr_stat("fa", &da);
			pr_stat("fb", &db);
			if (a.fp != b.fp)
				diff_full("a vs b", &fA, &fB, MEM);
			if (da.fp != a.fp)
				diff_full("a vs fa", &fA, &fa, MEM);
			if (db.fp != a.fp)
				diff_full("a vs fb", &fA, &fb, MEM);
		}
		if (reps > 1 && q == qmax && (r + 1) % 10 == 0)	/* progress of a repeated range */
			printf("progress: %" PRIu64 " repetitions, %" PRIu64 " runs, %" PRIu64 " mismatches\n",
			       r + 1, runs, bad);
	}
	printf("sweep_diag Q=%" PRIu64 "..%" PRIu64 ": %" PRIu64 " mismatches, %" PRIu64 " runs, %" PRIu64
	       " with tick overshoots, %" PRIu64 " with turn overshoots, max skid %" PRIu64 ", hash %016" PRIx64 "\n",
	       qmin, qmax, bad, runs, over_runs, sover_runs, smax, h);
	printf("sweep_diag: %s\n", bad ? "FAIL" : "PASS");
	return bad ? 1 : 0;
}

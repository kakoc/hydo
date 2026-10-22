// SPDX-License-Identifier: GPL-2.0
/*
 * sched_diag - debugging aid for hvx_sched_sweep_test: run the sweep guest straight to
 * the end and again with one mid-turn stop (HVX_RUNC max_insns K) or turn stop, and
 * print where the two final states differ (snapshot byte offsets, scheduler fields, RAM).
 * Build from hvx/:  cc -O2 -o sched_diag -DSWEEP_NO_MAIN -include hvx_sched_sweep_test.c smp/sched_diag.c
 * Usage: sched_diag Q K [turn]
 */
#include <stddef.h>

static void final_state(uint64_t q, uint64_t k, int by_turn, struct su_snap *out)
{
	struct su_vm vm;

	setup(&vm, q);
	if (k) {
		if (by_turn) {
			su_stop_turn(&vm, k);
			printf("stop -> 0x%x\n", su_run(&vm, NULL));
			su_stop_turn(&vm, 0);
		} else {
			printf("stop -> 0x%x\n", su_runc(&vm, k));
		}
	}
	su_run(&vm, NULL);
	su_save(&vm, out);
	su_destroy(&vm);
}

int main(int argc, char **argv)
{
	uint64_t q = strtoull(argv[1], NULL, 0), k = strtoull(argv[2], NULL, 0);
	struct su_snap *a = calloc(1, sizeof(*a)), *b = calloc(1, sizeof(*b));
	unsigned int i;
	size_t o, n = 0;

	final_state(q, 0, 0, a);
	final_state(q, k, argc > 3, b);
	for (i = 0; i < 2; i++)
		for (o = 0; o < sizeof(a->v[i]); o++)
			if (((uint8_t *)&a->v[i])[o] != ((uint8_t *)&b->v[i])[o] && n++ < 40)
				printf("vcpu%u snapshot byte +0x%zx: %02x vs %02x\n", i, o,
				       ((uint8_t *)&a->v[i])[o], ((uint8_t *)&b->v[i])[o]);
	printf("regs offsets: rip=0x%zx rsp=0x%zx rflags=0x%zx icount=0x%zx tsc=0x%zx lapic_reg=0x%zx inject=0x%zx fx=0x%zx pmu_total=0x%zx irr=0x%zx deadline=0x%zx\n",
	       offsetof(struct hvx_snapshot, regs.rip), offsetof(struct hvx_snapshot, regs.rsp),
	       offsetof(struct hvx_snapshot, regs.rflags), offsetof(struct hvx_snapshot, icount),
	       offsetof(struct hvx_snapshot, tsc), offsetof(struct hvx_snapshot, lapic_reg),
	       offsetof(struct hvx_snapshot, inject_at_icount), offsetof(struct hvx_snapshot, fx),
	       offsetof(struct hvx_snapshot, pmu_total), offsetof(struct hvx_snapshot, irr),
	       offsetof(struct hvx_snapshot, lapic_tsc_deadline));
	printf("sched: turns %llu/%llu thash %llx/%llx guard %llu/%llu tsc %llu/%llu icount %llu/%llu total %llu/%llu cur %u/%u\n",
	       (unsigned long long)a->sch.turns, (unsigned long long)b->sch.turns,
	       (unsigned long long)a->sch.turn_hash, (unsigned long long)b->sch.turn_hash,
	       (unsigned long long)a->sch.guard, (unsigned long long)b->sch.guard,
	       (unsigned long long)a->sch.tsc, (unsigned long long)b->sch.tsc,
	       (unsigned long long)a->sch.icount, (unsigned long long)b->sch.icount,
	       (unsigned long long)a->sch.pmu_total, (unsigned long long)b->sch.pmu_total,
	       a->sch.cur, b->sch.cur);
	for (i = 0; i < 2; i++)
		printf("v%u: br %llu/%llu turns %llu/%llu halted %u/%u cr2 %llx/%llx\n", i,
		       (unsigned long long)a->sch.v[i].branches, (unsigned long long)b->sch.v[i].branches,
		       (unsigned long long)a->sch.v[i].turns, (unsigned long long)b->sch.v[i].turns,
		       a->sch.v[i].halted, b->sch.v[i].halted,
		       (unsigned long long)a->sch.v[i].cr2, (unsigned long long)b->sch.v[i].cr2);
	n = 0;
	for (o = 0; o < MEM; o++)
		if (a->ram[o] != b->ram[o] && n++ < 20)
			printf("ram 0x%zx: %02x vs %02x\n", o, a->ram[o], b->ram[o]);
	return 0;
}

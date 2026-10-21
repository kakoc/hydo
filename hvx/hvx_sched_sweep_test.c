// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sched_sweep_test - turn boundaries under the in-kernel vCPU scheduler
 * (HVX_VM_SMP_SCHED) are a function of guest state only. Needs a bare-metal Intel PMU.
 *
 * Two vCPUs run the same loop (ITER times), built from the instructions that make a
 * switch point delicate:
 *   - mov %eax,%ss ; pause      a PAUSE exit inside a MOV-SS interrupt shadow; with
 *                               pause_k = 1 the turn must end there, which is NOT a
 *                               clean boundary, so the switch is deferred one instruction
 *   - mov %eax,%ss ; incq ...   an ordinary MOV-SS shadow
 *   - rep movsb (700 bytes)     a long branch-free stretch (landing steps and hops)
 *   - a 37-branch loop
 *   - wrmsr ICR                 an IPI to the other vCPU (vector 0x20)
 *   - wrmsr TSC_DEADLINE        the LAPIC timer, deadline = rdtsc + 3000
 *   - sti ; hlt ; cli           halt until the IPI or the timer wakes it
 * The ISR (vector 0x20) EOIs and counts. PMU ticks every 1031 branches move the clock.
 *
 * For every turn length Q in 1..2000:
 *   replay   two fresh runs give the same fingerprint (all vCPU state + scheduler + RAM)
 *   branch   stop at turn T/2 (HVX_EXIT_TURN), save, finish; restore, finish: both equal
 * and for a few Q, a branch at EVERY turn boundary and at many mid-turn stops (HVX_RUNC
 * max_insns), each equal to the uninterrupted run.
 *
 * Usage: hvx_sched_sweep_test [QMAX]   (default 2000)
 */
#include "hvx_sched_util.h"

#define MEM	(256UL * 1024)
#define CODE	HVX_GUEST_CODE_BASE
#define DATA0	0x10000UL
#define ITER	12

extern const uint8_t g_sweep_start[], g_sweep_end[], g_isr_start[], g_isr_end[];
__asm__(
	".pushsection .rodata\n"
	".globl g_sweep_start\n"
	"g_sweep_start:\n"
	"	mov $0x832, %ecx\n"		/* LVT timer: vector 0x20, TSC-deadline mode */
	"	mov $0x40020, %eax\n"
	"	xor %edx, %edx\n"
	"	wrmsr\n"
	"	mov $" "12" ", %r13d\n"		/* ITER */
	"1:\n"
	"	mov %ss, %eax\n"
	"	mov %eax, %ss\n"
	"	pause\n"			/* PAUSE exit in the MOV-SS shadow */
	"	incq (%r15)\n"
	"	mov %ss, %eax\n"
	"	mov %eax, %ss\n"
	"	incq 8(%r15)\n"			/* in the MOV-SS shadow */
	"	lea 0x100(%r15), %rsi\n"
	"	lea 0x400(%r15), %rdi\n"
	"	mov $700, %ecx\n"
	"	rep movsb\n"
	"	add %r13b, 0x100(%r15)\n"
	"	mov $37, %ecx\n"
	"2:	dec %ecx\n"
	"	jnz 2b\n"
	"	mov $0x830, %ecx\n"		/* IPI vector 0x20 to the other vCPU (r14) */
	"	mov $0x20, %eax\n"
	"	mov %r14d, %edx\n"
	"	wrmsr\n"
	"	rdtsc\n"			/* deadline = TSC + 3000 */
	"	shl $32, %rdx\n"
	"	or %rax, %rdx\n"
	"	add $3000, %rdx\n"
	"	mov %edx, %eax\n"
	"	shr $32, %rdx\n"
	"	mov $0x6e0, %ecx\n"
	"	wrmsr\n"
	"	sti\n"
	"	hlt\n"
	"	cli\n"
	"	incq 0x10(%r15)\n"
	"	dec %r13d\n"
	"	jnz 1b\n"
	"	mov $0x830, %ecx\n"		/* a last IPI so the other is not left waiting */
	"	mov $0x20, %eax\n"
	"	mov %r14d, %edx\n"
	"	wrmsr\n"
	"	movq $1, 0x18(%r15)\n"		/* done */
	"	hlt\n"				/* IF=0: final */
	".globl g_sweep_end\n"
	"g_sweep_end:\n"
	".globl g_isr_start\n"
	"g_isr_start:\n"
	"	push %rax\n"
	"	push %rcx\n"
	"	push %rdx\n"
	"	mov $0x80b, %ecx\n"		/* x2APIC EOI */
	"	xor %eax, %eax\n"
	"	xor %edx, %edx\n"
	"	wrmsr\n"
	"	incq 0x2f00\n"
	"	pop %rdx\n"
	"	pop %rcx\n"
	"	pop %rax\n"
	"	iretq\n"
	".globl g_isr_end\n"
	"g_isr_end:\n"
	".popsection\n");

static void setup(struct su_vm *vm, uint64_t q)
{
	struct hvx_regs r;
	struct hvx_tsc t;
	unsigned int i;

	if (su_create(vm, MEM, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED | HVX_VM_PAUSE_EXIT) < 0) {
		if (errno == ENODEV) {
			printf("hvx_sched_sweep_test: SKIP (no usable PMU on this host)\n");
			exit(0);
		}
		su_die("HVX_CREATE_VM");
	}
	memcpy(vm->mem + CODE, g_sweep_start, g_sweep_end - g_sweep_start);
	memcpy(vm->mem + SU_GPA_ISR, g_isr_start, g_isr_end - g_isr_start);
	for (i = 0; i < 2; i++) {
		memset(&r, 0, sizeof(r));
		r.rip = CODE;
		r.r15 = DATA0 + 0x1000 * i;
		r.r14 = 1 - i;			/* the other vCPU's x2APIC ID */
		r.rsp = 0x30000 + 0x1000 * i;
		r.rflags = 0x2;
		su_set_regs(vm, i, &r);
		memset(&t, 0, sizeof(t));
		t.tsc = 5000;
		t.step = 1;
		t.mode = HVX_TSC_COUNTER;
		if (ioctl(vm->fd, HVX_SET_TSC, &t) < 0)
			su_die("SET_TSC");
	}
	su_pmu(vm, 1031, 700);
	su_policy(vm, q, 0, 1);
}

struct out {
	uint64_t fp, turns, deferred, jumps, wakeups, isr, iters[2], done[2];
};

static void collect(struct su_vm *vm, struct out *o)
{
	struct hvx_sched s;
	unsigned int i;

	su_get_sched(vm, &s);
	o->fp = su_fingerprint(vm);
	o->turns = s.turns;
	o->deferred = s.deferred;
	o->jumps = s.time_jumps;
	o->wakeups = s.wakeups;
	o->isr = *(uint64_t *)(vm->mem + SU_GPA_INTR);
	for (i = 0; i < 2; i++) {
		o->iters[i] = *(uint64_t *)(vm->mem + DATA0 + 0x1000 * i + 0x10);
		o->done[i] = *(uint64_t *)(vm->mem + DATA0 + 0x1000 * i + 0x18);
	}
}

/* equal in everything that is guest state ("deferred" counts run-loop passes, which
 * host interrupts add to, so it is a statistic only) */
static int same(const struct out *a, const struct out *b)
{
	return a->fp == b->fp && a->turns == b->turns && a->jumps == b->jumps &&
	       a->wakeups == b->wakeups && a->isr == b->isr &&
	       !memcmp(a->iters, b->iters, sizeof(a->iters)) && !memcmp(a->done, b->done, sizeof(a->done));
}

static void full_run(uint64_t q, struct out *o)
{
	struct su_vm vm;

	setup(&vm, q);
	su_run(&vm, NULL);
	collect(&vm, o);
	su_destroy(&vm);
}

/* stop at turn k (turn_stop) or after max_insns events (k_insns), save, finish, restore,
 * finish: returns 1 if both finishes equal fp */
static int branch_at(uint64_t q, uint64_t k_turn, uint64_t k_insns, uint64_t fp,
		     uint64_t *fa, uint64_t *fb)
{
	struct su_vm vm;
	struct su_snap sn;
	uint32_t why;

	memset(&sn, 0, sizeof(sn));
	setup(&vm, q);
	if (k_turn) {
		su_stop_turn(&vm, k_turn);
		why = su_run(&vm, NULL);
		su_stop_turn(&vm, 0);
		if (why != HVX_EXIT_TURN) {
			su_destroy(&vm);
			return -1;		/* the run ended first */
		}
	} else {
		why = su_runc(&vm, k_insns);
		if (why != HVX_EXIT_STEP) {
			su_destroy(&vm);
			return -1;
		}
	}
	su_save(&vm, &sn);
	su_run(&vm, NULL);
	*fa = su_fingerprint(&vm);
	su_restore(&vm, &sn, 0);
	su_run(&vm, NULL);
	*fb = su_fingerprint(&vm);
	su_destroy(&vm);
	free(sn.ram);
	return *fa == fp && *fb == fp;
}

int main(int argc, char **argv)
{
	uint64_t qmax = argc > 1 ? strtoull(argv[1], NULL, 0) : 2000, q;
	uint64_t nq = 0, bad = 0, defer_q = 0, every_bad = 0, every_n = 0, mid_bad = 0, mid_n = 0;
	uint64_t h = 0xcbf29ce484222325ULL, minturns = ~0ULL, maxturns = 0;
	static const uint64_t every_q[] = { 13, 300, 1999 };
	unsigned int e;

	for (q = 1; q <= qmax; q++) {
		struct out a, b;
		uint64_t fa = 0, fb = 0;
		int br;

		full_run(q, &a);
		full_run(q, &b);
		br = branch_at(q, a.turns / 2 ? a.turns / 2 : 1, 0, a.fp, &fa, &fb);
		nq++;
		if (a.deferred)
			defer_q++;
		if (a.turns < minturns)
			minturns = a.turns;
		if (a.turns > maxturns)
			maxturns = a.turns;
		h = su_fnv(h, &a.fp, sizeof(a.fp));
		if (!same(&a, &b) || br != 1 || a.done[0] != 1 || a.done[1] != 1 ||
		    a.iters[0] != ITER || a.iters[1] != ITER) {
			bad++;
			printf("Q=%" PRIu64 " MISMATCH: fp %016" PRIx64 "/%016" PRIx64 " branch %d (%016" PRIx64
			       "/%016" PRIx64 ") turns %" PRIu64 " iters %" PRIu64 "/%" PRIu64 " done %" PRIu64
			       "/%" PRIu64 "\n", q, a.fp, b.fp, br, fa, fb, a.turns, a.iters[0], a.iters[1],
			       a.done[0], a.done[1]);
		}
		if (q <= 3 || q % 250 == 0 || q == qmax)
			printf("Q=%" PRIu64 ": fp=%016" PRIx64 " turns=%" PRIu64 " deferred=%" PRIu64
			       " time_jumps=%" PRIu64 " wakeups=%" PRIu64 " isr=%" PRIu64 " replay+branch %s\n",
			       q, a.fp, a.turns, a.deferred, a.jumps, a.wakeups, a.isr,
			       (!same(&a, &b) || br != 1) ? "DIFFER" : "identical");
	}
	printf("sweep Q=1..%" PRIu64 ": %" PRIu64 " turn lengths, %" PRIu64 " mismatches, turns per run %"
	       PRIu64 "..%" PRIu64 ", deferred switches seen at %" PRIu64 " lengths, sweep hash %016" PRIx64 "\n",
	       qmax, nq, bad, minturns, maxturns, defer_q, h);

	/* a branch at EVERY turn boundary, and at mid-turn max_insns stops, for a few Q */
	for (e = 0; e < sizeof(every_q) / sizeof(every_q[0]); e++) {
		struct out a;
		uint64_t k, fa, fb;

		q = every_q[e];
		if (q > qmax)
			continue;
		full_run(q, &a);
		for (k = 1; k < a.turns; k++) {
			every_n++;
			if (branch_at(q, k, 0, a.fp, &fa, &fb) != 1) {
				every_bad++;
				if (every_bad < 10)
					printf("Q=%" PRIu64 " branch at turn %" PRIu64 ": %016" PRIx64 "/%016" PRIx64
					       " want %016" PRIx64 "\n", q, k, fa, fb, a.fp);
			}
		}
		for (k = 1; k < 400; k += (k < 40 ? 1 : 7)) {
			int r = branch_at(q, 0, k, a.fp, &fa, &fb);

			if (r < 0)
				break;		/* the run ended before k events */
			mid_n++;
			if (r != 1) {
				mid_bad++;
				if (mid_bad < 10)
					printf("Q=%" PRIu64 " branch at max_insns %" PRIu64 ": %016" PRIx64 "/%016"
					       PRIx64 " want %016" PRIx64 "\n", q, k, fa, fb, a.fp);
			}
		}
		printf("Q=%" PRIu64 ": branch at every turn (%" PRIu64 " turns) and at mid-turn stops: done\n",
		       q, a.turns);
	}
	printf("every-turn branches: %" PRIu64 " checked, %" PRIu64 " mismatches; mid-turn (max_insns) branches: %"
	       PRIu64 " checked, %" PRIu64 " mismatches\n", every_n, every_bad, mid_n, mid_bad);

	if (!bad && !every_bad && !mid_bad && defer_q) {
		printf("hvx_sched_sweep_test: PASS (replay and branch-from-snapshot identical at every turn length "
		       "1..%" PRIu64 ", at every turn boundary and at mid-turn stops)\n", qmax);
		return 0;
	}
	printf("hvx_sched_sweep_test: FAIL\n");
	return 1;
}

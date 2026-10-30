// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sched_race_test - turns alone hit a plain-computation race window
 * (HVX_VM_SMP_SCHED). Needs a bare-metal Intel PMU.
 *
 * The shared-counter race of hvx_smp_test, shaped like hv/hvx/race/hydorace.c: two vCPUs
 * sell seats from one counter with a check-then-act race:
 *     loop: OUT iterations of unrelated work (dec/jnz)
 *           s = seats ; if (s <= 0) stop          -- check
 *           W iterations of work (dec/jnz)        -- the race window: plain computation
 *           seats = s - 1 ; sold++                -- act
 * A sale is lost only if a turn ends inside the window and the other vCPU sells
 * meanwhile. There is NO interrupt and NO tick (the PMU tick period is 2^40), so the
 * guest's own scheduling plays no part: only turn boundaries interleave the vCPUs.
 *
 * Asserted:
 *   - lost sales (sold0 + sold1 - SEATS) and the full fingerprint are a pure function of
 *     (turn length Q, seed): every (Q, seed) of the grid run twice gives the same result;
 *   - a serialized schedule (Q = 2^30) loses nothing;
 *   - turns alone hit the window: some (Q, seed) lose sales.
 */
#include "hvx_sched_util.h"

#define MEM	(256UL * 1024)
#define CODE	HVX_GUEST_CODE_BASE
#define SEATS_GPA 0x10000UL
#define SOLD_GPA  0x10100UL	/* + 8 * vcpu */
#define SEATS	400
#define OUT	200
#define W	1

extern const uint8_t g_race_start[], g_race_end[];
__asm__(
	".pushsection .rodata\n"
	".globl g_race_start\n"
	"g_race_start:\n"
	"1:	mov %r8, %rcx\n"		/* work(OUT) */
	"2:	dec %rcx\n"
	"	jnz 2b\n"
	"	mov (%rbx), %rax\n"		/* s = seats      (check) */
	"	test %rax, %rax\n"
	"	jle 4f\n"
	"	mov %r9, %rcx\n"		/* work(W): the window */
	"3:	dec %rcx\n"
	"	jnz 3b\n"
	"	dec %rax\n"
	"	mov %rax, (%rbx)\n"		/* seats = s - 1  (act) */
	"	incq (%r12)\n"			/* sold++ */
	"	jmp 1b\n"
	"4:	hlt\n"
	".globl g_race_end\n"
	"g_race_end:\n"
	".popsection\n");

struct res { int64_t lost; uint64_t sold[2], turns, fp; };

static void run_one(uint64_t q, uint64_t seed, struct res *o)
{
	struct su_vm vm;
	struct hvx_regs r;
	struct hvx_sched s;
	unsigned int i;

	if (su_create(&vm, MEM, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED) < 0) {
		if (errno == ENODEV) {
			printf("hvx_sched_race_test: SKIP (no usable PMU on this host)\n");
			exit(0);
		}
		su_die("HVX_CREATE_VM");
	}
	memcpy(vm.mem + CODE, g_race_start, g_race_end - g_race_start);
	*(uint64_t *)(vm.mem + SEATS_GPA) = SEATS;
	for (i = 0; i < 2; i++) {
		memset(&r, 0, sizeof(r));
		r.rip = CODE;
		r.rbx = SEATS_GPA;
		r.r12 = SOLD_GPA + 8 * i;
		r.r8 = OUT;
		r.r9 = W;
		r.rsp = 0x30000 + 0x1000 * i;
		r.rflags = 0x2;
		su_set_regs(&vm, i, &r);
	}
	su_pmu(&vm, 1ULL << 40, 0);		/* no tick ever: turns are the only switches */
	su_policy(&vm, q, seed, 0);
	if (su_run(&vm, NULL) != SU_EXIT_HLT) {
		fprintf(stderr, "run did not end in HLT\n");
		exit(1);
	}
	su_get_sched(&vm, &s);
	o->sold[0] = *(uint64_t *)(vm.mem + SOLD_GPA);
	o->sold[1] = *(uint64_t *)(vm.mem + SOLD_GPA + 8);
	o->lost = (int64_t)(o->sold[0] + o->sold[1]) - SEATS;
	o->turns = s.turns;
	o->fp = su_fingerprint(&vm);
	su_destroy(&vm);
}

int main(void)
{
	static const uint64_t qs[] = { 50, 203, 1000, 3000, 20000 };
	static const uint64_t seeds[] = { 0, 1, 2, 3, 4, 5, 6, 7 };
	unsigned int qi, si;
	int ok = 1, hits = 0, cells = 0;
	struct res ser;
	uint64_t h = 0xcbf29ce484222325ULL;

	run_one(1ULL << 30, 0, &ser);
	printf("serialized (Q=2^30): sold=%" PRIu64 "+%" PRIu64 " lost=%" PRId64 " turns=%" PRIu64 "\n",
	       ser.sold[0], ser.sold[1], ser.lost, ser.turns);
	if (ser.lost != 0)
		ok = 0;

	printf("lost sales by (Q, seed), window = %d of %d branches per sale:\n", W + 1, OUT + W + 1);
	for (qi = 0; qi < sizeof(qs) / sizeof(qs[0]); qi++) {
		printf("  Q=%-6" PRIu64 ":", qs[qi]);
		for (si = 0; si < sizeof(seeds) / sizeof(seeds[0]); si++) {
			struct res a, b;

			run_one(qs[qi], seeds[si], &a);
			run_one(qs[qi], seeds[si], &b);
			cells++;
			if (a.lost != b.lost || a.fp != b.fp || a.turns != b.turns) {
				ok = 0;
				printf(" [seed %" PRIu64 " NOT REPRODUCIBLE: %" PRId64 "/%" PRId64 " %016" PRIx64 "/%016" PRIx64 "]",
				       seeds[si], a.lost, b.lost, a.fp, b.fp);
			} else {
				printf(" %3" PRId64, a.lost);
			}
			if (a.lost > 0)
				hits++;
			if (a.lost < 0)
				ok = 0;		/* more seats than exist cannot be lost */
			h = su_fnv(h, &a.fp, sizeof(a.fp));
		}
		printf("   (seed 0 = fixed turns, others seeded)\n");
	}
	printf("%d of %d (Q, seed) cells lost sales; grid hash %016" PRIx64 "\n", hits, cells, h);
	if (!hits)
		ok = 0;
	printf(ok ? "hvx_sched_race_test: PASS (lost updates are a pure function of (Q, seed); turns alone hit "
		    "a plain-computation window)\n"
		  : "hvx_sched_race_test: FAIL\n");
	return ok ? 0 : 1;
}

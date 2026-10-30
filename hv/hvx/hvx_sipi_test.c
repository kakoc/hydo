// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_sipi_test - AP start-up by INIT + STARTUP IPI (HVX_VM_SMP_BOOT, SMP Stage 2) on a
 * hand-written two-vCPU guest. Needs a bare-metal Intel PMU and the "unrestricted guest"
 * VMX control.
 *
 * vCPU 0 starts in long mode (the module's default tables) and sends, through its x2APIC
 * ICR, INIT assert + INIT de-assert + STARTUP(vector 8) + a second STARTUP to APIC ID 1,
 * then spins (PAUSE) until vCPU 1 reports it is done. vCPU 1 starts in wait-for-SIPI; the
 * STARTUP puts it in real mode at 0800:0000 (GPA 0x8000), where a stub
 *   - writes 0xbeef through ES = 0x900 (real-mode segment arithmetic),
 *   - runs a branch loop in real mode, loads a GDT, enters 32-bit protected mode,
 *   - runs a branch loop there, writes 0xcafe, enables PAE + EFER.LME + paging,
 *   - far-jumps to 64-bit code, runs a branch loop, writes 0x64 and a done flag, HLTs.
 * A second STARTUP to a running vCPU must be ignored (otherwise vCPU 1 restarts).
 *
 * Asserted, for turn lengths Q = 1..QMAX (so turn boundaries fall in real mode, in
 * protected mode without paging and in long mode) and the default:
 *   - every marker is written, vCPU 1 ends with EFER.LMA and CS = 0x18;
 *   - replay: the same Q twice gives the same fingerprint (every vCPU's snapshot +
 *     scheduler state + RAM);
 *   - branch: stop at turn T/2, save, finish; restore, finish: both equal the straight run.
 */
#include "hvx_sched_util.h"

#define MEM	(4UL << 20)
#define CODE	HVX_GUEST_CODE_BASE	/* 0x3000: vCPU 0 */
#define AP_GPA	0x8000UL		/* STARTUP vector 8 */
#define M_RM	0x9000UL
#define M_PM	0x9004UL
#define M_LM	0x9008UL
#define M_DONE	0x9010UL
#define M_RUNS	0x9018UL		/* how many times the AP stub started */

extern const uint8_t g_bsp_start[], g_bsp_end[], g_ap_start[], g_ap_end[];
__asm__(
	".pushsection .rodata\n"
	".code64\n"
	".globl g_bsp_start\n"
	"g_bsp_start:\n"
	"	mov $0x830, %ecx\n"
	"	mov $1, %edx\n"
	"	mov $0xc500, %eax\n"		/* INIT, level assert, to APIC ID 1 */
	"	wrmsr\n"
	"	mov $0x8500, %eax\n"		/* INIT level de-assert (no effect) */
	"	wrmsr\n"
	"	mov $0x0608, %eax\n"		/* STARTUP, vector 8 -> 0x8000 */
	"	wrmsr\n"
	"	mov $0x0608, %eax\n"		/* second STARTUP: ignored (vCPU 1 runs) */
	"	wrmsr\n"
	"1:	pause\n"
	"	cmpq $0, 0x9010\n"
	"	je 1b\n"
	"	hlt\n"
	".globl g_bsp_end\n"
	"g_bsp_end:\n"

	".code16\n"
	".globl g_ap_start\n"
	"g_ap_start:\n"
	"	cli\n"
	"	incw 0x9018\n"			/* DS = 0 after STARTUP */
	"	mov $0x900, %ax\n"
	"	mov %ax, %es\n"
	"	movw $0xbeef, %es:0\n"		/* 0x900:0 = GPA 0x9000 */
	"	mov $40, %cx\n"
	"2:	dec %cx\n"
	"	jnz 2b\n"
	"	lgdtl (ap_gdtr - g_ap_start + 0x8000)\n"
	"	mov %cr0, %eax\n"
	"	or $1, %eax\n"
	"	mov %eax, %cr0\n"
	"	ljmpl $0x08, $(ap_pm - g_ap_start + 0x8000)\n"
	".code32\n"
	"ap_pm:\n"
	"	mov $0x10, %ax\n"
	"	mov %ax, %ds\n"
	"	mov %ax, %es\n"
	"	mov %ax, %ss\n"
	"	mov $40, %ecx\n"
	"3:	dec %ecx\n"
	"	jnz 3b\n"
	"	movl $0xcafe, 0x9004\n"
	"	mov %cr4, %eax\n"
	"	or $0x20, %eax\n"		/* PAE */
	"	mov %eax, %cr4\n"
	"	xor %eax, %eax\n"
	"	mov %eax, %cr3\n"		/* the module's identity tables at GPA 0 */
	"	mov $0xc0000080, %ecx\n"
	"	rdmsr\n"
	"	or $0x100, %eax\n"		/* EFER.LME */
	"	wrmsr\n"
	"	mov %cr0, %eax\n"
	"	or $0x80000000, %eax\n"		/* paging: long mode becomes active */
	"	mov %eax, %cr0\n"
	"	ljmpl $0x18, $(ap_lm - g_ap_start + 0x8000)\n"
	".code64\n"
	"ap_lm:\n"
	"	mov $0x20, %ax\n"
	"	mov %ax, %ds\n"
	"	mov %ax, %ss\n"
	"	mov $3000, %ecx\n"
	"4:	dec %ecx\n"
	"	jnz 4b\n"
	"	movq $0x64, 0x9008\n"
	"	movq $1, 0x9010\n"
	"	hlt\n"
	".p2align 3\n"
	"ap_gdt:\n"
	"	.quad 0\n"
	"	.quad 0x00cf9b000000ffff\n"	/* 0x08 code32 */
	"	.quad 0x00cf93000000ffff\n"	/* 0x10 data32 */
	"	.quad 0x00af9b000000ffff\n"	/* 0x18 code64 */
	"	.quad 0x00cf93000000ffff\n"	/* 0x20 data */
	"ap_gdtr:\n"
	"	.word 39\n"
	"	.long ap_gdt - g_ap_start + 0x8000\n"
	".globl g_ap_end\n"
	"g_ap_end:\n"
	".code64\n"
	".popsection\n");

struct res { uint64_t fp, turns, rm, pm, lm, done, runs, efer; uint32_t cs; };

static void setup(struct su_vm *vm, uint64_t q)
{
	struct hvx_regs r;

	if (su_create(vm, MEM, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED | HVX_VM_SMP_BOOT |
		      HVX_VM_PAUSE_EXIT) < 0) {
		if (errno == ENODEV) {
			printf("hvx_sipi_test: SKIP (no usable PMU / unrestricted guest)\n");
			exit(0);
		}
		su_die("HVX_CREATE_VM");
	}
	memcpy(vm->mem + CODE, g_bsp_start, g_bsp_end - g_bsp_start);
	memcpy(vm->mem + AP_GPA, g_ap_start, g_ap_end - g_ap_start);
	memset(&r, 0, sizeof(r));
	r.rip = CODE;
	r.rsp = 0x30000;
	r.rflags = 0x2;
	su_set_regs(vm, 0, &r);
	su_pmu(vm, 1ULL << 40, 0);	/* no ticks */
	su_policy(vm, q, 0, 1);
}

static void finish(struct su_vm *vm, struct res *o)
{
	struct hvx_sched s;
	struct hvx_sregs sr;

	o->fp = su_fingerprint(vm);
	su_get_sched(vm, &s);
	o->turns = s.turns;
	o->rm = *(uint16_t *)(vm->mem + M_RM);
	o->pm = *(uint32_t *)(vm->mem + M_PM);
	o->lm = *(uint64_t *)(vm->mem + M_LM);
	o->done = *(uint64_t *)(vm->mem + M_DONE);
	o->runs = *(uint16_t *)(vm->mem + M_RUNS);
	su_set_vcpu(vm, 1);
	if (ioctl(vm->fd, HVX_GET_SREGS, &sr) < 0)
		su_die("HVX_GET_SREGS");
	o->efer = sr.efer;
	o->cs = sr.cs.selector;
}

static void run_one(uint64_t q, struct res *o)
{
	struct su_vm vm;
	uint32_t why;

	setup(&vm, q);
	why = su_run(&vm, NULL);
	if (why != SU_EXIT_HLT) {
		fprintf(stderr, "Q=%" PRIu64 ": run ended with 0x%x\n", q, why);
		exit(1);
	}
	finish(&vm, o);
	su_destroy(&vm);
}

static int check(uint64_t q, const struct res *a)
{
	if (a->rm != 0xbeef || a->pm != 0xcafe || a->lm != 0x64 || a->done != 1 || a->runs != 1 ||
	    !(a->efer & (1ULL << 10)) || a->cs != 0x18) {
		fprintf(stderr, "Q=%" PRIu64 ": BAD rm=%" PRIx64 " pm=%" PRIx64 " lm=%" PRIx64
			" done=%" PRIu64 " runs=%" PRIu64 " efer=%" PRIx64 " cs=%x\n", q, a->rm, a->pm,
			a->lm, a->done, a->runs, a->efer, a->cs);
		return 1;
	}
	return 0;
}

/* branch at turn t: stop, save, finish (A); restore, finish (B) */
static int branch(uint64_t q, uint64_t t, uint64_t want)
{
	struct su_vm vm;
	struct su_snap sn;
	struct res a, b;
	uint32_t why;

	memset(&sn, 0, sizeof(sn));
	setup(&vm, q);
	su_stop_turn(&vm, t);
	why = su_run(&vm, NULL);
	if (why != HVX_EXIT_TURN) {
		fprintf(stderr, "Q=%" PRIu64 " stop at %" PRIu64 ": got 0x%x\n", q, t, why);
		return 1;
	}
	su_save(&vm, &sn);
	su_stop_turn(&vm, 0);
	if (su_run(&vm, NULL) != SU_EXIT_HLT)
		return 1;
	finish(&vm, &a);
	su_restore(&vm, &sn, 0);
	if (su_run(&vm, NULL) != SU_EXIT_HLT)
		return 1;
	finish(&vm, &b);
	su_destroy(&vm);
	free(sn.ram);
	if (a.fp != want || b.fp != want) {
		fprintf(stderr, "Q=%" PRIu64 " branch at %" PRIu64 ": straight %016" PRIx64
			" continued %016" PRIx64 " restored %016" PRIx64 "\n", q, t, want, a.fp, b.fp);
		return 1;
	}
	return 0;
}

int main(int argc, char **argv)
{
	uint64_t qmax = argc > 1 ? strtoull(argv[1], NULL, 0) : 300, q, h = 0xcbf29ce484222325ULL;
	struct res a, b;
	int bad = 0, nb = 0;

	/* the default turn length first, with a readable report */
	run_one(1ULL << 20, &a);
	run_one(1ULL << 20, &b);
	bad |= check(1ULL << 20, &a);
	printf("hvx_sipi_test: Q=2^20 turns=%" PRIu64 " rm=%" PRIx64 " pm=%" PRIx64 " lm=%" PRIx64
	       " runs=%" PRIu64 " efer=%" PRIx64 " cs=%x fp=%016" PRIx64 " / %016" PRIx64 "\n",
	       a.turns, a.rm, a.pm, a.lm, a.runs, a.efer, a.cs, a.fp, b.fp);
	if (a.fp != b.fp)
		bad = 1;
	for (q = 1; q <= qmax; q++) {
		run_one(q, &a);
		run_one(q, &b);
		if (check(q, &a) || a.fp != b.fp) {
			fprintf(stderr, "Q=%" PRIu64 ": replay %016" PRIx64 " vs %016" PRIx64 "\n",
				q, a.fp, b.fp);
			bad = 1;
			break;
		}
		h = su_fnv(h, &a.fp, 8);
		if (a.turns >= 2 && (q % 7 == 1 || q == qmax)) {
			bad |= branch(q, a.turns / 2, a.fp);
			nb++;
		}
	}
	printf("hvx_sipi_test: Q=1..%" PRIu64 " replay identical, %d branches checked, sweep hash %016"
	       PRIx64 "\n", qmax, nb, h);
	printf("hvx_sipi_test: %s\n", bad ? "FAIL" : "PASS");
	return bad;
}

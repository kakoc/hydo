/*
 * probe.c -- capability probes for a deterministic hydo backend on Apple Silicon
 * (Hypervisor.framework). Each mode runs in its own process: ./probe <mode>.
 * Guests are the tiny EL1 programs in guest.s (MMU off, identity physical).
 */
#include <Hypervisor/Hypervisor.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>

extern char guest_blob[], guest_blob_end[];
extern char g_hvc[], g_mmio[], g_time[], g_pmu[], g_step[], g_irq[], g_wfi[], g_ids[], g_compute[], g_cntfrq[], g_cntp[], g_vectors[], g_spin[], g_wfe[];

#define BASE   0x10000000ULL          /* IPA of guest RAM */
#define MEMSZ  (1u << 20)
#define DATA   (BASE + 0x80000)
#define MMIO   0x20000000ULL          /* unmapped */
#define CHK(x) do { hv_return_t _r = (x); if (_r) { printf("FAIL %s = 0x%x\n", #x, _r); exit(1); } } while (0)

static uint8_t *mem;
static hv_vcpu_t vcpu;
static hv_vcpu_exit_t *ex;

static uint64_t entry(char *sym) { return BASE + (uint64_t)(sym - guest_blob); }
static uint64_t ec(void) { return (ex->exception.syndrome >> 26) & 0x3f; }
static uint64_t iss(void) { return ex->exception.syndrome & 0x1ffffff; }
static uint64_t rd(hv_reg_t r) { uint64_t v; CHK(hv_vcpu_get_reg(vcpu, r, &v)); return v; }
static void wr(hv_reg_t r, uint64_t v) { CHK(hv_vcpu_set_reg(vcpu, r, v)); }
static uint64_t srd(hv_sys_reg_t r) { uint64_t v = 0; hv_vcpu_get_sys_reg(vcpu, r, &v); return v; }

static void vm_setup(int with_gic)
{
	CHK(hv_vm_create(NULL));
	mem = mmap(NULL, MEMSZ, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
	memcpy(mem, guest_blob, guest_blob_end - guest_blob);
	CHK(hv_vm_map(mem, BASE, MEMSZ, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	if (with_gic) {
		hv_gic_config_t gc = hv_gic_config_create();
		size_t ds, rs; hv_gic_get_distributor_size(&ds); hv_gic_get_redistributor_region_size(&rs);
		hv_gic_config_set_distributor_base(gc, 0x08000000);
		hv_gic_config_set_redistributor_base(gc, 0x080a0000);
		hv_return_t r = hv_gic_create(gc);
		printf("gic: hv_gic_create = 0x%x (dist size %zu, redist region %zu)\n", r, ds, rs);
	}
}

static void vcpu_new(char *sym)
{
	CHK(hv_vcpu_create(&vcpu, &ex, NULL));
	wr(HV_REG_PC, entry(sym));
	wr(HV_REG_CPSR, 0x3c5);                 /* EL1h, DAIF masked */
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_SP_EL1, BASE + MEMSZ - 16));
	wr(HV_REG_X2, DATA);
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_CPACR_EL1, 3 << 20));      /* FP/SIMD enabled at EL1/EL0 */
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_VBAR_EL1, entry(g_vectors))); /* guest faults -> hvc #8 */
}

/* run; advance PC past trapping instructions that do not do it themselves */
static int run(void)
{
	CHK(hv_vcpu_run(vcpu));
	if (ex->reason != HV_EXIT_REASON_EXCEPTION) return -(int)ex->reason;
	uint64_t e = ec();
	if (e == 0x16) return (int)(iss() & 0xffff);          /* HVC: PC already past it */
	return 1000 + (int)e;
}
static void skip(void) { wr(HV_REG_PC, rd(HV_REG_PC) + 4); }

static double now_s(void) { static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb);
	return (double)mach_absolute_time() * tb.numer / tb.denom / 1e9; }

/* ---------- 1. lifecycle + exit latency ---------- */
static void p_lifecycle(void)
{
	vm_setup(0);
	vcpu_new(g_hvc);
	int n = 200000; double t0 = now_s();
	for (int i = 0; i < n; i++) if (run() != 0) { printf("unexpected exit\n"); exit(1); }
	double dt = now_s() - t0;
	printf("lifecycle: %d HVC exits in %.3f s = %.0f exits/s (%.2f us/exit)\n", n, dt, n / dt, dt / n * 1e6);
	hv_vcpu_destroy(vcpu);
	vcpu_new(g_mmio); wr(HV_REG_X2, MMIO);
	int r = run();
	printf("mmio store: exit=%d EC=0x%llx ISS=0x%llx FAR(ex)=0x%llx IPA=0x%llx pc=0x%llx\n", r, ec(), iss(),
	       ex->exception.virtual_address, ex->exception.physical_address, rd(HV_REG_PC));
	skip(); r = run();
	printf("mmio load : exit=%d EC=0x%llx ISS=0x%llx IPA=0x%llx (ISS.SRT=x%llu)\n", r, ec(), iss(),
	       ex->exception.physical_address, (iss() >> 16) & 31);
	skip(); r = run(); printf("after mmio: exit=%d (9 = done)\n", r);
}

/* ---------- 2. time ---------- */
static void time_run(int drive, uint64_t *out, int n)
{
	vcpu_new(g_time); wr(HV_REG_X3, n);
	uint64_t *buf = (uint64_t *)(mem + (DATA - BASE));
	int traps = 0; uint64_t k = 0, trap_iss = 0;
	for (;;) {
		if (drive) {   /* make CNTVCT = 1000*k + 5000 at entry: offset = mach_absolute_time() - target */
			uint64_t target = 5000 + 1000ULL * k;
			hv_vcpu_set_vtimer_offset(vcpu, mach_absolute_time() - target);
		}
		int r = run();
		if (r >= 1000) {        /* trapped counter read: emulate it as the deterministic value 7777+k */
			traps++; trap_iss = iss(); wr(HV_REG_X0 + ((trap_iss >> 5) & 31), 7777 + k); skip(); continue;
		}
		if (r == 9) break;
		k++;
	}
	if (traps) printf("time: trapped read ISS=0x%llx (op0=%llu op1=%llu CRn=%llu CRm=%llu op2=%llu) -> CNTPCT_EL0 is op2=1, CNTVCT_EL0 op2=2\n",
	       trap_iss, (trap_iss >> 20) & 3, (trap_iss >> 14) & 7, (trap_iss >> 10) & 15, (trap_iss >> 1) & 15, (trap_iss >> 17) & 7);
	memcpy(out, buf, 2 * n * sizeof(uint64_t));
	if (traps) printf("time: %d counter reads trapped\n", traps);
	hv_vcpu_destroy(vcpu);
}
static void p_time(void)
{
	vm_setup(0);
	vcpu_new(g_cntfrq); run(); printf("time: guest CNTFRQ_EL0 = %llu Hz\n", rd(HV_REG_X0)); hv_vcpu_destroy(vcpu);
	enum { N = 2000 };
	static uint64_t a[2 * N], b[2 * N];
	time_run(0, a, N);
	printf("time: default offset: cntvct[0..3] = %llu %llu %llu %llu; cntpct[0] = %llu\n", a[0], a[2], a[4], a[6], a[1]);
	printf("time: no exits were taken by MRS CNTVCT/CNTPCT unless reported above\n");
	time_run(1, a, N); time_run(1, b, N);
	long dmin = 1L << 40, dmax = -(1L << 40), same = 0; double sum = 0;
	for (int k = 0; k < N; k++) {
		long d = (long)a[2 * k] - (long)(5000 + 1000L * k);
		if (d < dmin) dmin = d; if (d > dmax) dmax = d; sum += d;
		same += a[2 * k] == b[2 * k];
	}
	printf("time: driven offset (target 5000+1000k at each entry), %d reads: guest-seen minus target min %ld max %ld mean %.1f ticks\n",
	       N, dmin, dmax, sum / N);
	printf("time: run A vs run B identical counter values: %ld / %d\n", same, N);
	/* masking the vtimer offset idea: does the counter advance while the vcpu is NOT running? */
	printf("time: cntpct vs cntvct (driven) first pair: %llu vs %llu\n", a[1], a[0]);
}

/* ---------- 3. PMU ---------- */
static void p_pmu(void)
{
	bool el2 = false; hv_vm_config_get_el2_supported(&el2);
	printf("pmu: EL2 (nested) supported on this machine: %d\n", el2);
	vm_setup(0);
	vcpu_new(g_pmu);
	for (;;) {
		int r = run();
		if (r == 9) break;
		if (r == 1000 + 0x18) {
			uint64_t s = iss();
			printf("pmu: MRS/MSR trap ISS=0x%llx (op0=%llu op1=%llu CRn=%llu CRm=%llu op2=%llu dir=%s Rt=x%llu)\n", s,
			       (s >> 20) & 3, (s >> 14) & 7, (s >> 10) & 15, (s >> 1) & 15, (s >> 17) & 7,
			       (s & 1) ? "read" : "write", (s >> 5) & 31);
			skip(); continue;
		}
		printf("pmu: other exit %d EC=0x%llx\n", r, ec()); skip();
	}
	uint64_t dfr = rd(HV_REG_X0);
	printf("pmu: guest ID_AA64DFR0_EL1 = 0x%llx (PMUVer = %llu)\n", dfr, (dfr >> 8) & 15);
}

/* ---------- 4. single step ---------- */
static uint64_t step_count(char *sym, int maxsteps, double *rate)
{
	vcpu_new(sym);
	CHK(hv_vcpu_set_trap_debug_exceptions(vcpu, true));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, 1 | (1 << 13)));  /* SS | KDE */
	uint64_t steps = 0; double t0 = now_s();
	for (;;) {
		wr(HV_REG_CPSR, (rd(HV_REG_CPSR) & ~0x200ULL) | (1ULL << 21));      /* D clear, PSTATE.SS = 1 */
		int r = run();
		if (r == 1000 + 0x32 || r == 1000 + 0x33) { steps++; if (steps >= (uint64_t)maxsteps) break; continue; }
		if (r == 9) break;
		if (r == 0) continue;                                        /* g_hvc loop used for the rate */
		printf("step: unexpected exit %d EC=0x%llx at pc 0x%llx\n", r, ec(), rd(HV_REG_PC)); break;
	}
	if (rate) *rate = steps / (now_s() - t0);
	return steps;
}
static void p_step(void)
{
	vm_setup(0);
	double rate;
	uint64_t s1 = step_count(g_step, 1 << 30, &rate); hv_vcpu_destroy(vcpu);
	uint64_t s2 = step_count(g_step, 1 << 30, NULL); hv_vcpu_destroy(vcpu);
	printf("step: loop of 1+100*2 instructions counted %llu and %llu steps (expected 201)\n", s1, s2);
	step_count(g_hvc, 200000, &rate);
	printf("step: %.0f single steps/s\n", rate);
}

/* ---------- 5. interrupt injection at an exact instruction ---------- */
static uint64_t irq_after(int k, uint64_t *elr)
{
	vcpu_new(g_irq);
	/* step k instructions, then stop stepping and make an IRQ pending */
	CHK(hv_vcpu_set_trap_debug_exceptions(vcpu, true));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, 1 | (1 << 13)));
	for (int i = 0; i < k; i++) {
		wr(HV_REG_CPSR, (rd(HV_REG_CPSR) & ~0x200ULL) | (1ULL << 21));
		int r = run(); if (r != 1000 + 0x32 && r != 1000 + 0x33) { printf("irq: step exit %d\n", r); exit(1); }
	}
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, 0));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
	CHK(hv_vcpu_set_pending_interrupt(vcpu, HV_INTERRUPT_TYPE_IRQ, true));
	int r = run();
	*elr = srd(HV_SYS_REG_ELR_EL1);
	uint64_t x5 = rd(HV_REG_X5);
	if (r != 7) printf("irq: expected handler hvc #7, got %d EC=0x%llx\n", r, ec());
	hv_vcpu_destroy(vcpu);
	return x5;
}
static void p_irq(void)
{
	vm_setup(0);
	for (int k = 20; k <= 23; k++) {
		uint64_t e1, e2, a = irq_after(k, &e1), b = irq_after(k, &e2);
		printf("irq: after %d stepped insns, IRQ taken with x5=%llu ELR=0x%llx | repeat x5=%llu ELR=0x%llx %s\n",
		       k, a, e1 - BASE, b, e2 - BASE, (a == b && e1 == e2) ? "SAME" : "DIFF");
	}
	/* vtimer: arm CNTV with a deadline and see the exit */
	vcpu_new(g_hvc);
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_CNTV_CVAL_EL0, 0));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_CNTV_CTL_EL0, 1));     /* enable, unmasked, already expired */
	hv_vcpu_set_vtimer_mask(vcpu, false);
	int r = 0, n = 0; do { r = run(); n++; } while (r == 0 && n < 100000);
	printf("vtimer: expired CNTV -> exit reason %d after %d runs (-%d = VTIMER_ACTIVATED)\n", r, n, HV_EXIT_REASON_VTIMER_ACTIVATED);
	hv_vcpu_destroy(vcpu);
}

/* ---------- 6. snapshot / restore ---------- */
static const struct { hv_sys_reg_t r; const char *n; } SR[] = {
#define S(x) { HV_SYS_REG_##x, #x }
	S(SCTLR_EL1), S(CPACR_EL1), S(TTBR0_EL1), S(TTBR1_EL1), S(TCR_EL1), S(SPSR_EL1), S(ELR_EL1), S(SP_EL0),
	S(SP_EL1), S(AFSR0_EL1), S(AFSR1_EL1), S(ESR_EL1), S(FAR_EL1), S(PAR_EL1), S(MAIR_EL1), S(AMAIR_EL1),
	S(VBAR_EL1), S(CONTEXTIDR_EL1), S(TPIDR_EL1), S(CNTKCTL_EL1), S(CSSELR_EL1), S(TPIDR_EL0), S(TPIDRRO_EL0),
	S(CNTV_CTL_EL0), S(CNTV_CVAL_EL0), S(MDSCR_EL1), S(MDCCINT_EL1), S(ACTLR_EL1), S(SCXTNUM_EL0), S(SCXTNUM_EL1),
	S(TPIDR2_EL0), S(SMCR_EL1), S(SMPRI_EL1), S(APIAKEYLO_EL1), S(APIAKEYHI_EL1), S(APIBKEYLO_EL1),
	S(APIBKEYHI_EL1), S(APDAKEYLO_EL1), S(APDAKEYHI_EL1), S(APDBKEYLO_EL1), S(APDBKEYHI_EL1), S(APGAKEYLO_EL1),
	S(APGAKEYHI_EL1), S(MIDR_EL1), S(MPIDR_EL1),
#undef S
};
#define NSR (sizeof SR / sizeof SR[0])
struct snap { uint64_t x[31], pc, cpsr, fpcr, fpsr, sr[NSR]; hv_simd_fp_uchar16_t q[32]; uint8_t ok[NSR]; uint64_t vtoff; };
static void take(struct snap *s, int verbose)
{
	for (int i = 0; i < 31; i++) s->x[i] = rd(HV_REG_X0 + i);
	s->pc = rd(HV_REG_PC); s->cpsr = rd(HV_REG_CPSR); s->fpcr = rd(HV_REG_FPCR); s->fpsr = rd(HV_REG_FPSR);
	for (int i = 0; i < 32; i++) CHK(hv_vcpu_get_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, &s->q[i]));
	for (size_t i = 0; i < NSR; i++) {
		hv_return_t r = hv_vcpu_get_sys_reg(vcpu, SR[i].r, &s->sr[i]);
		s->ok[i] = r == 0;
		if (verbose && r) printf("snapshot: get %s -> 0x%x (unavailable)\n", SR[i].n, r);
	}
	hv_vcpu_get_vtimer_offset(vcpu, &s->vtoff);
}
static void put(const struct snap *s, int verbose)
{
	for (int i = 0; i < 31; i++) wr(HV_REG_X0 + i, s->x[i]);
	wr(HV_REG_PC, s->pc); wr(HV_REG_CPSR, s->cpsr); wr(HV_REG_FPCR, s->fpcr); wr(HV_REG_FPSR, s->fpsr);
	for (int i = 0; i < 32; i++) CHK(hv_vcpu_set_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, s->q[i]));
	for (size_t i = 0; i < NSR; i++) if (s->ok[i]) {
		hv_return_t r = hv_vcpu_set_sys_reg(vcpu, SR[i].r, s->sr[i]);
		if (verbose && r) printf("snapshot: set %s -> 0x%x (not writable)\n", SR[i].n, r);
	}
	hv_vcpu_set_vtimer_offset(vcpu, s->vtoff);
}
static uint64_t fnv(const void *p, size_t n, uint64_t h)
{ const uint8_t *b = p; for (size_t i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; } return h; }
static uint64_t fp_state(void)
{
	struct snap s; memset(&s, 0, sizeof s); take(&s, 0);
	uint64_t h = 0xcbf29ce484222325ULL;
	h = fnv(s.x, sizeof s.x, h); h = fnv(&s.pc, 8, h); h = fnv(&s.cpsr, 8, h); h = fnv(&s.fpcr, 16, h);
	h = fnv(s.q, sizeof s.q, h);
	for (size_t i = 0; i < NSR; i++) if (SR[i].r != HV_SYS_REG_CNTV_CVAL_EL0) h = fnv(&s.sr[i], 8, h);
	return fnv(mem, MEMSZ, h);
}
static void p_snapshot(void)
{
	vm_setup(0);
	static uint8_t mcopy[MEMSZ];
	vcpu_new(g_compute);
	while (run() != 9) ;
	uint64_t full = fp_state(); hv_vcpu_destroy(vcpu);
	memcpy(mem, guest_blob, guest_blob_end - guest_blob); memset(mem + (DATA - BASE), 0, 0x10000);

	vcpu_new(g_compute);
	int r; while ((r = run()) != 2) ;
	struct snap s; memset(&s, 0, sizeof s); take(&s, 1); memcpy(mcopy, mem, MEMSZ);
	while (run() != 9) ;
	uint64_t a = fp_state(); hv_vcpu_destroy(vcpu);

	memset(mem, 0xee, MEMSZ);                     /* trash, then restore into a FRESH vcpu */
	CHK(hv_vcpu_create(&vcpu, &ex, NULL));
	memcpy(mem, mcopy, MEMSZ); put(&s, 1);
	while (run() != 9) ;
	uint64_t b = fp_state();
	printf("snapshot: full run %016llx | snapshot at midpoint -> finish %016llx | restore into new vcpu -> finish %016llx => %s\n",
	       full, a, b, (full == a && a == b) ? "IDENTICAL" : "DIFFERENT");
	int n_ok = 0; for (size_t i = 0; i < NSR; i++) n_ok += s.ok[i];
	printf("snapshot: %d of %zu probed system registers readable\n", n_ok, NSR);
}

/* ---------- 7. ID registers, RNDR, WFI ---------- */
static void p_ids(void)
{
	vm_setup(0);
	hv_vcpu_config_t cfg = hv_vcpu_config_create();
	uint64_t v;
	hv_vcpu_config_get_feature_reg(cfg, HV_FEATURE_REG_ID_AA64ISAR0_EL1, &v); printf("ids: config ISAR0 = 0x%016llx RNDR field = %llu\n", v, v >> 60);
	hv_vcpu_config_get_feature_reg(cfg, HV_FEATURE_REG_ID_AA64MMFR0_EL1, &v); printf("ids: config MMFR0 = 0x%016llx ECV field = %llu\n", v, v >> 60);
	hv_vcpu_config_get_feature_reg(cfg, HV_FEATURE_REG_ID_AA64DFR0_EL1, &v); printf("ids: config DFR0  = 0x%016llx PMUVer = %llu\n", v, (v >> 8) & 15);
	for (int pass = 0; pass < 2; pass++) {
		vcpu_new(g_ids);
		if (pass == 1) { /* try to hide RNDR from the guest */
			uint64_t isar = srd(HV_SYS_REG_ID_AA64ISAR0_EL1);
			hv_return_t r = hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_ID_AA64ISAR0_EL1, isar & ~(0xfULL << 60));
			printf("ids: set ISAR0 with RNDR cleared -> 0x%x\n", r);
		}
		int r, guard = 0;
		while ((r = run()) != 9 && guard++ < 20) {
			if (r == 8) {   /* guest took a synchronous exception: report it, return past the faulting insn */
				uint64_t esr = srd(HV_SYS_REG_ESR_EL1), elr = srd(HV_SYS_REG_ELR_EL1);
				printf("ids: guest exception ESR_EL1=0x%llx (EC 0x%llx) at +0x%llx -> treated as UNDEFINED\n", esr, esr >> 26, elr - BASE);
				wr(HV_REG_PC, elr + 4); wr(HV_REG_CPSR, 0x3c5); continue;
			}
			printf("ids: exit %d EC=0x%llx ISS=0x%llx pc+%llx\n", r, ec(), iss(), rd(HV_REG_PC) - BASE); skip();
		}
		printf("ids: pass %d guest sees ISAR0=0x%016llx MMFR0=0x%016llx DFR0=0x%016llx MIDR=0x%llx RNDR=%016llx %016llx\n",
		       pass, rd(HV_REG_X0), rd(HV_REG_X1), rd(HV_REG_X4), rd(HV_REG_X5), rd(HV_REG_X6), rd(HV_REG_X7));
		hv_vcpu_destroy(vcpu);
	}
	vcpu_new(g_cntp);
	{ int r2, guard = 0; while ((r2 = run()) != 9 && guard++ < 20) {
		printf("cntp: exit %d EC=0x%llx ISS=0x%llx (op2=%llu CRm=%llu %s)\n", r2, ec(), iss(), (iss() >> 17) & 7, (iss() >> 1) & 15, (iss() & 1) ? "read" : "write"); skip(); }
	  printf("cntp: no further exits; guest read CNTP_TVAL=%lld CNTP_CTL=0x%llx CNTV_CTL=0x%llx\n", (long long)rd(HV_REG_X1), rd(HV_REG_X4), rd(HV_REG_X5)); }
	hv_vcpu_destroy(vcpu);
	vcpu_new(g_wfi);
	int r = run(); printf("wfi: exit %d EC=0x%llx ISS=0x%llx (EC 0x01 = WFI/WFE trapped)\n", r, ec(), iss());
}

int main(int argc, char **argv)
{
	const char *m = argc > 1 ? argv[1] : "";
	if (!strcmp(m, "lifecycle")) p_lifecycle();
	else if (!strcmp(m, "time")) p_time();
	else if (!strcmp(m, "pmu")) p_pmu();
	else if (!strcmp(m, "step")) p_step();
	else if (!strcmp(m, "irq")) p_irq();
	else if (!strcmp(m, "snapshot")) p_snapshot();
	else if (!strcmp(m, "ids")) p_ids();
	else if (!strcmp(m, "spin")) {   /* does a long exit-free run ever return early (host interrupts)? */
		vm_setup(0);
		for (int t = 0; t < 3; t++) {
			vcpu_new(g_spin); wr(HV_REG_X3, 3000000000ULL);
			int ret = 0, r; double t0 = now_s();
			while ((r = run()) != 9) { ret++; if (ret < 4) printf("spin: early return reason=%d EC=0x%llx x3=%llu\n", r, ec(), rd(HV_REG_X3)); }
			printf("spin: 3e9-iteration loop: %d early returns, %.2f s\n", ret, now_s() - t0);
			hv_vcpu_destroy(vcpu);
		}
		vcpu_new(g_wfe);
		int r, guard = 0; while ((r = run()) != 9 && guard++ < 10) { printf("wfe: exit %d EC=0x%llx ISS=0x%llx at +0x%llx\n", r, ec(), iss(), rd(HV_REG_PC) - BASE); skip(); }
		printf("wfe: done (sevl; wfe; wfe; yield)\n");
	}
	else if (!strcmp(m, "gic")) { vm_setup(1); }
	else { fprintf(stderr, "usage: probe lifecycle|time|pmu|step|irq|snapshot|ids|gic\n"); return 2; }
	return 0;
}

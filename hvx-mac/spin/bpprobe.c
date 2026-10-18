/*
 * bpprobe.c -- probes for working around exit-free spins on Hypervisor.framework (M2).
 *   ./bpprobe bp      hardware breakpoint on a loop instruction: does each hit exit? rate?
 *   ./bpprobe wp      data watchpoint on the spin flag: does each load exit? rate?
 *   ./bpprobe prot    stage-2 write protection: does a store exit, and can it be resumed?
 *   ./bpprobe kick    hv_vcpus_exit from another thread: where does the vCPU stop, is state intact?
 *   ./bpprobe privsym call the private _hv_vcpu_get_control_field / _hv_capability and print what they return
 * Guests: sguest.s (EL1, MMU off).
 */
#include <Hypervisor/Hypervisor.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <dlfcn.h>
#include <sys/mman.h>

extern char sg_blob[], sg_blob_end[], sg_count[], sg_flag[], sg_store[], sg_drift[], sg_wfe[], sg_vectors[];
#define BASE  0x10000000ULL
#define MEMSZ (1u << 20)
#define DATA  (BASE + 0x80000)
#define CHK(x) do { hv_return_t _r = (x); if (_r) { printf("FAIL %s = 0x%x\n", #x, _r); exit(1); } } while (0)

static uint8_t *mem;
static hv_vcpu_t vcpu;
static hv_vcpu_exit_t *ex;
static uint64_t entry(char *s) { return BASE + (uint64_t)(s - sg_blob); }
static uint64_t ec(void) { return (ex->exception.syndrome >> 26) & 0x3f; }
static uint64_t rd(hv_reg_t r) { uint64_t v; CHK(hv_vcpu_get_reg(vcpu, r, &v)); return v; }
static void wr(hv_reg_t r, uint64_t v) { CHK(hv_vcpu_set_reg(vcpu, r, v)); }
static void swr(hv_sys_reg_t r, uint64_t v) { CHK(hv_vcpu_set_sys_reg(vcpu, r, v)); }
static double now_s(void) { static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb);
	return (double)mach_absolute_time() * tb.numer / tb.denom / 1e9; }

static void setup(char *sym)
{
	CHK(hv_vm_create(NULL));
	mem = mmap(NULL, MEMSZ, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
	memcpy(mem, sg_blob, sg_blob_end - sg_blob);
	CHK(hv_vm_map(mem, BASE, MEMSZ, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	CHK(hv_vcpu_create(&vcpu, &ex, NULL));
	wr(HV_REG_PC, entry(sym));
	wr(HV_REG_CPSR, 0x3c5);
	swr(HV_SYS_REG_SP_EL1, BASE + MEMSZ - 16);
	swr(HV_SYS_REG_VBAR_EL1, entry(sg_vectors));
	wr(HV_REG_X2, DATA);
}

/* step over the instruction at PC with breakpoints/watchpoints off (one software step) */
static void step_over(hv_sys_reg_t ctl_reg, uint64_t ctl_val, uint64_t mdscr)
{
	swr(ctl_reg, 0);
	swr(HV_SYS_REG_MDSCR_EL1, mdscr | 1);          /* SS */
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) | (1ULL << 21));
	CHK(hv_vcpu_run(vcpu));
	if (ec() != 0x32 && ec() != 0x33) { printf("step_over: unexpected EC 0x%llx\n", ec()); exit(1); }
	swr(HV_SYS_REG_MDSCR_EL1, mdscr);
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
	swr(ctl_reg, ctl_val);
}

static void p_bp(void)
{
	setup(sg_count);
	uint64_t n = 200000; wr(HV_REG_X3, n);
	CHK(hv_vcpu_set_trap_debug_exceptions(vcpu, true));
	CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, true));     /* the guest cannot see or change them */
	uint64_t mdscr = (1 << 15) | (1 << 13);                   /* MDE | KDE */
	uint64_t bcr = (0xfULL << 5) | (3 << 1) | 1;              /* BAS=all, PMC=EL1+EL0, enabled */
	swr(HV_SYS_REG_MDSCR_EL1, mdscr);
	swr(HV_SYS_REG_DBGBVR0_EL1, entry(sg_count));           /* the loop head (subs) */
	swr(HV_SYS_REG_DBGBCR0_EL1, bcr);
	uint64_t hits = 0, bad = 0; uint64_t first_ec = 0; double t0 = now_s();
	for (;;) {
		CHK(hv_vcpu_run(vcpu));
		uint64_t e = ec();
		if (e == 0x16) break;                                  /* hvc #9: done */
		if (!first_ec) first_ec = e;
		if (e != 0x30 && e != 0x31) { printf("bp: unexpected EC 0x%llx pc 0x%llx\n", e, rd(HV_REG_PC)); exit(1); }
		if (rd(HV_REG_PC) != entry(sg_count)) bad++;
		/* the loop counter at the k-th hit must be n - k (exact, no skid) */
		if (rd(HV_REG_X3) != n - hits) bad++;
		hits++;
		step_over(HV_SYS_REG_DBGBCR0_EL1, bcr, mdscr);
	}
	double dt = now_s() - t0;
	printf("bp: EC 0x%llx, %llu hits for %llu iterations, %llu wrong PC/count, %.2f s = %.0f hits/s (%.2f us per hit incl. step-over)\n",
	       first_ec, hits, n, bad, dt, hits / dt, dt / hits * 1e6);
}

static void p_wp(void)
{
	setup(sg_flag);
	volatile uint32_t *flag = (uint32_t *)(mem + (DATA - BASE));
	*flag = 1;
	CHK(hv_vcpu_set_trap_debug_exceptions(vcpu, true));
	CHK(hv_vcpu_set_trap_debug_reg_accesses(vcpu, true));
	uint64_t mdscr = (1 << 15) | (1 << 13);
	uint64_t wcr = (0xffULL << 5) | (3 << 3) | (3 << 1) | 1;  /* BAS=8 bytes, LSC=load+store, PAC=EL1+EL0, enabled */
	swr(HV_SYS_REG_MDSCR_EL1, mdscr);
	swr(HV_SYS_REG_DBGWVR0_EL1, DATA);
	swr(HV_SYS_REG_DBGWCR0_EL1, wcr);
	uint64_t hits = 0, n = 100000; uint64_t first_ec = 0, far = 0, pcbad = 0; double t0 = now_s();
	for (;;) {
		CHK(hv_vcpu_run(vcpu));
		uint64_t e = ec();
		if (e == 0x16) break;
		if (!first_ec) { first_ec = e; far = ex->exception.virtual_address; }
		if (e != 0x34 && e != 0x35) { printf("wp: unexpected EC 0x%llx\n", e); exit(1); }
		if (rd(HV_REG_PC) != entry(sg_flag)) pcbad++;          /* taken before the load executes */
		if (++hits == n) *flag = 0;
		step_over(HV_SYS_REG_DBGWCR0_EL1, wcr, mdscr);
	}
	double dt = now_s() - t0;
	printf("wp: EC 0x%llx far 0x%llx, %llu hits, PC not at the load %llu times, %.2f s = %.0f hits/s\n",
	       first_ec, far, hits, pcbad, dt, hits / dt);
}

static void p_prot(void)
{
	setup(sg_store);
	volatile uint32_t *flag = (uint32_t *)(mem + (DATA - BASE));
	*flag = 1; wr(HV_REG_X5, 0x1234);
	/* write-protect the data page only (the code page must stay executable) */
	CHK(hv_vm_protect(DATA, 0x4000, HV_MEMORY_READ | HV_MEMORY_EXEC));
	CHK(hv_vcpu_run(vcpu));
	uint64_t e = ec(), iss = ex->exception.syndrome & 0x1ffffff;
	printf("prot: store to a read-only page -> EC 0x%llx ISS 0x%llx (WnR=%llu DFSC=0x%llx) IPA 0x%llx pc 0x%llx\n",
	       e, iss, (iss >> 6) & 1, iss & 0x3f, ex->exception.physical_address, rd(HV_REG_PC));
	CHK(hv_vm_protect(DATA, 0x4000, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	*flag = 0;
	CHK(hv_vcpu_run(vcpu));
	printf("prot: after unprotect + re-run: EC 0x%llx (0x16 = reached hvc), stored value 0x%llx\n", ec(),
	       *(uint64_t *)(mem + (DATA - BASE) + 8));
	/* cost of protecting / unprotecting a large range */
	hv_vm_destroy();
	CHK(hv_vm_create(NULL));
	size_t big = 256u << 20; uint8_t *b = mmap(NULL, big, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
	memset(b, 1, big);
	CHK(hv_vm_map(b, 0x40000000, big, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	double t0 = now_s();
	for (int i = 0; i < 20; i++) {
		CHK(hv_vm_protect(0x40000000, big, HV_MEMORY_READ | HV_MEMORY_EXEC));
		CHK(hv_vm_protect(0x40000000, big, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	}
	printf("prot: protect+unprotect of 256 MiB: %.2f ms per pair\n", (now_s() - t0) / 20 * 1e3);
}

static volatile int kick_go;
static void *kicker(void *a) { (void)a; while (!kick_go) {} usleep(50000); hv_vcpus_exit(&vcpu, 1); return NULL; }
static void p_kick(void)
{
	setup(sg_count);
	uint64_t n = 4000000000ULL; wr(HV_REG_X3, n);
	pthread_t t; pthread_create(&t, NULL, kicker, NULL);
	kick_go = 1;
	CHK(hv_vcpu_run(vcpu));
	uint64_t pc = rd(HV_REG_PC), x3 = rd(HV_REG_X3);
	printf("kick: reason %d (CANCELED=%d) pc 0x%llx (loop at 0x%llx) x3 %llu (%llu iterations in ~50 ms)\n",
	       ex->reason, HV_EXIT_REASON_CANCELED, pc, entry(sg_count), x3, n - x3);
	wr(HV_REG_X3, 1000); wr(HV_REG_PC, entry(sg_count));
	CHK(hv_vcpu_run(vcpu));
	printf("kick: resumed, next exit EC 0x%llx (0x16 = finished normally)\n", ec());
	/* a cancel requested while the vCPU is NOT running: does the next run return at once? */
	hv_vcpus_exit(&vcpu, 1);
	wr(HV_REG_X3, 1000000); wr(HV_REG_PC, entry(sg_count));
	CHK(hv_vcpu_run(vcpu));
	printf("kick: cancel before run -> reason %d, x3 %llu (1000000 = no progress)\n", ex->reason, rd(HV_REG_X3));
	CHK(hv_vcpu_run(vcpu));
	printf("kick: and the run after that -> reason %d EC 0x%llx\n", ex->reason, ec());
}

static void p_privsym(void)
{
	void *h = dlopen("/System/Library/Frameworks/Hypervisor.framework/Hypervisor", RTLD_NOW);
	const char *names[] = { "_hv_vcpu_get_control_field", "_hv_vcpu_set_control_field", "_hv_capability",
	                        "_hv_vcpu_config_get_fgt_enabled", "_hv_vcpu_config_set_fgt_enabled", "_hv_vcpu_get_context", NULL };
	for (int i = 0; names[i]; i++) printf("privsym: %s -> %p\n", names[i], dlsym(h, names[i]));
	/* _hv_capability(uint64_t id, uint64_t *value): probe ids 0..31 */
	hv_return_t (*cap)(uint64_t, uint64_t *) = dlsym(h, "_hv_capability");
	if (cap) for (uint64_t id = 0; id < 32; id++) { uint64_t v = 0; hv_return_t r = cap(id, &v); if (!r) printf("privsym: _hv_capability(%llu) = 0x%llx\n", id, v); }
	setup(sg_count);
	/* _hv_vcpu_get_control_field(vcpu, field, &value): probe field ids 0..63 */
	hv_return_t (*gcf)(hv_vcpu_t, uint64_t, uint64_t *) = dlsym(h, "_hv_vcpu_get_control_field");
	if (gcf) for (uint64_t f = 0; f < 64; f++) { uint64_t v = 0; hv_return_t r = gcf(vcpu, f, &v); if (!r) printf("privsym: control_field(%llu) = 0x%llx\n", f, v); }
}

/* private: set HCR_EL2.TWE (bit 14) in the guest control field 0 and see whether WFE now exits */
static void p_twe(void)
{
	void *h = dlopen("/System/Library/Frameworks/Hypervisor.framework/Hypervisor", RTLD_NOW);
	hv_return_t (*gcf)(hv_vcpu_t, uint64_t, uint64_t *) = dlsym(h, "_hv_vcpu_get_control_field");
	hv_return_t (*scf)(hv_vcpu_t, uint64_t, uint64_t) = dlsym(h, "_hv_vcpu_set_control_field");
	setup(sg_wfe);
	uint64_t v = 0; CHK(gcf(vcpu, 0, &v));
	hv_return_t r = scf(vcpu, 0, v | (1ULL << 14));
	uint64_t v2 = 0; gcf(vcpu, 0, &v2);
	printf("twe: field0 0x%llx -> set 0x%llx returned 0x%x, reads back 0x%llx\n", v, v | (1ULL << 14), r, v2);
	for (int i = 0; i < 6; i++) {
		CHK(hv_vcpu_run(vcpu));
		uint64_t e = ec(), iss = ex->exception.syndrome & 0x1ffffff;
		printf("twe: exit %d reason %d EC 0x%llx ISS 0x%llx pc 0x%llx\n", i, ex->reason, e, iss, rd(HV_REG_PC));
		if (e == 0x16) break;
		if (e == 0x01) wr(HV_REG_PC, rd(HV_REG_PC) + 4);
	}
}

int main(int argc, char **argv)
{
	if (argc < 2) { fprintf(stderr, "usage: bpprobe bp|wp|prot|kick|privsym\n"); return 2; }
	if (!strcmp(argv[1], "bp")) p_bp();
	else if (!strcmp(argv[1], "wp")) p_wp();
	else if (!strcmp(argv[1], "prot")) p_prot();
	else if (!strcmp(argv[1], "kick")) p_kick();
	else if (!strcmp(argv[1], "privsym")) p_privsym();
	else if (!strcmp(argv[1], "twe")) p_twe();
	return 0;
}

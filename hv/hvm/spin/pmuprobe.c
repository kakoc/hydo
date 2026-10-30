/*
 * pmuprobe.c -- can Apple's own core PMU, programmed from macOS through the PRIVATE
 * kperf / kperfdata frameworks, count a Hypervisor.framework guest's instructions and
 * branches exactly? (The guest itself has no PMU on M2: PROBE.md probe 4.)
 *
 * UNDOCUMENTED: kperf.framework and kperfdata.framework are private Apple frameworks with
 * no public headers; the signatures below follow XNU's open-source kpc interface
 * (osfmk/kern/kpc.h, osfmk/arm64/kpc.c) and public research tools. They can change in
 * any macOS update. Programming the counters needs root (kpc_force_all_ctrs_set).
 *
 *   ./pmuprobe          what works without root: framework symbols, the event database
 *   sudo ./pmuprobe run the measurements (see README-pmu.md for what each line means)
 *
 * The guest is the 2-instruction loop sg_count (subs x3,x3,#1; b.ne) from sguest.s, so a
 * run of N iterations retires 2N guest instructions and N guest branches, plus the final
 * hvc. Counting modes come from XNU's config word (osfmk/arm64/kpc.c): bit 17 = EL0
 * AArch64, bit 18 = EL1. Which of those catch the GUEST is what this probe measures.
 */
#include <Hypervisor/Hypervisor.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>
#include <unistd.h>

extern char sg_blob[], sg_blob_end[], sg_count[], sg_flag[], sg_store[], sg_drift[], sg_wfe[], sg_vectors[];
#define BASE  0x10000000ULL
#define MEMSZ (1u << 20)
#define CHK(x) do { hv_return_t _r = (x); if (_r) { printf("FAIL %s = 0x%x\n", #x, _r); exit(1); } } while (0)

typedef uint64_t kpc_config_t;
#define KPC_CLASS_FIXED_MASK        1u
#define KPC_CLASS_CONFIGURABLE_MASK 2u
#define KPC_MAX_COUNTERS 32
#define CFG_EL0A64 0x20000ULL
#define CFG_EL1    0x40000ULL
#define CFG_MODES  0xf0000ULL

/* kperf */
static int (*kpc_force_all_ctrs_set)(int);
static int (*kpc_set_config)(uint32_t, kpc_config_t *);
static int (*kpc_set_counting)(uint32_t);
static int (*kpc_set_thread_counting)(uint32_t);
static int (*kpc_get_thread_counters)(uint32_t, uint32_t, uint64_t *);
static uint32_t (*kpc_get_counter_count)(uint32_t);
/* kperfdata */
typedef struct kpep_db kpep_db; typedef struct kpep_config kpep_config; typedef struct kpep_event kpep_event;
static int (*kpep_db_create)(const char *, kpep_db **);
static int (*kpep_db_name)(kpep_db *, const char **);
static int (*kpep_db_events_count)(kpep_db *, size_t *);
static int (*kpep_db_events)(kpep_db *, kpep_event **, size_t);
static int (*kpep_event_name)(kpep_event *, const char **);
static int (*kpep_db_event)(kpep_db *, const char *, kpep_event **);
static int (*kpep_config_create)(kpep_db *, kpep_config **);
static int (*kpep_config_force_counters)(kpep_config *);
static int (*kpep_config_add_event)(kpep_config *, kpep_event **, uint32_t, uint32_t *);
static int (*kpep_config_kpc_classes)(kpep_config *, uint32_t *);
static int (*kpep_config_kpc_count)(kpep_config *, size_t *);
static int (*kpep_config_kpc)(kpep_config *, kpc_config_t *, size_t);
static int (*kpep_config_kpc_map)(kpep_config *, size_t *, size_t);

#define SYM(h, n) do { *(void **)&n = dlsym(h, #n); printf("sym: %-28s %s\n", #n, n ? "found" : "MISSING"); } while (0)

static uint8_t *mem;
static hv_vcpu_t vcpu;
static hv_vcpu_exit_t *ex;
static uint64_t entry(char *s) { return BASE + (uint64_t)(s - sg_blob); }
static void wr(hv_reg_t r, uint64_t v) { CHK(hv_vcpu_set_reg(vcpu, r, v)); }

static void vm_setup(void)
{
	CHK(hv_vm_create(NULL));
	mem = mmap(NULL, MEMSZ, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
	memcpy(mem, sg_blob, sg_blob_end - sg_blob);
	CHK(hv_vm_map(mem, BASE, MEMSZ, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	CHK(hv_vcpu_create(&vcpu, &ex, NULL));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_VBAR_EL1, entry(sg_vectors)));
}
static void guest_loop(uint64_t n)          /* run sg_count for n iterations, to its hvc #9 */
{
	wr(HV_REG_PC, entry(sg_count)); wr(HV_REG_CPSR, 0x3c5); wr(HV_REG_X3, n);
	CHK(hv_vcpu_run(vcpu));
	if (((ex->exception.syndrome >> 26) & 0x3f) != 0x16) { printf("guest: unexpected exit\n"); exit(1); }
}
static void host_loop(uint64_t n)           /* the same loop shape on the host, at EL0 */
{
	asm volatile("mov x3, %0\n1: subs x3, x3, #1\n b.ne 1b\n" :: "r"(n) : "x3", "cc");
}

static const char *EV[] = { "FIXED_INSTRUCTIONS", "INST_ALL", "INST_BRANCH", "FIXED_CYCLES", NULL };
static int nev, ev_idx[8];                  /* counter index of each requested event */
static size_t ncnt;

static int program(kpep_db *db, uint64_t modes)
{
	kpep_config *cfg = NULL;
	if (kpep_config_create(db, &cfg) || kpep_config_force_counters(cfg)) { printf("config: create failed\n"); return -1; }
	nev = 0;
	for (int i = 0; EV[i]; i++) {
		kpep_event *e = NULL;
		if (kpep_db_event(db, EV[i], &e) || !e) { printf("config: event %s not in the database\n", EV[i]); ev_idx[nev++] = -1; continue; }
		uint32_t err = 0;
		if (kpep_config_add_event(cfg, &e, 0, &err)) { printf("config: add %s failed (%u)\n", EV[i], err); ev_idx[nev++] = -1; continue; }
		ev_idx[nev++] = 0;
	}
	uint32_t classes = 0; kpc_config_t regs[KPC_MAX_COUNTERS] = { 0 }; size_t map[KPC_MAX_COUNTERS] = { 0 };
	if (kpep_config_kpc_classes(cfg, &classes) || kpep_config_kpc_count(cfg, &ncnt) ||
	    kpep_config_kpc(cfg, regs, sizeof regs) || kpep_config_kpc_map(cfg, map, sizeof map)) { printf("config: kpc export failed\n"); return -1; }
	for (int i = 0, k = 0; i < nev; i++) if (ev_idx[i] >= 0) ev_idx[i] = (int)map[k++];
	/* configurable counters: replace the mode bits (fixed counters always count EL0+EL1: XNU "monotonic compatibility") */
	for (size_t i = 0; i < ncnt; i++) if (regs[i]) regs[i] = (regs[i] & ~CFG_MODES) | modes;
	if (kpc_set_config(classes, regs)) { printf("kpc_set_config failed (root?)\n"); return -1; }
	if (kpc_set_counting(classes) || kpc_set_thread_counting(classes)) { printf("kpc_set_counting failed\n"); return -1; }
	return 0;
}
static void counters(uint64_t *out)
{
	uint64_t buf[KPC_MAX_COUNTERS] = { 0 };
	uint32_t n = kpc_get_counter_count(KPC_CLASS_FIXED_MASK | KPC_CLASS_CONFIGURABLE_MASK);
	if (kpc_get_thread_counters(0, n, buf)) { printf("kpc_get_thread_counters failed\n"); exit(1); }
	for (int i = 0; i < nev; i++) out[i] = ev_idx[i] >= 0 ? buf[ev_idx[i]] : 0;
}

/* run f(n) R times; print per event: the delta for each repetition's min/max, and the slope per iteration */
static void measure(const char *what, void (*f)(uint64_t), uint64_t n1, uint64_t n2, int reps)
{
	uint64_t d[2][16][8];
	uint64_t ns[2] = { n1, n2 };
	for (int s = 0; s < 2; s++)
		for (int r = 0; r < reps; r++) {
			uint64_t a[8], b[8];
			counters(a); f(ns[s]); counters(b);
			for (int i = 0; i < nev; i++) d[s][r][i] = b[i] - a[i];
		}
	for (int i = 0; i < nev; i++) {
		if (ev_idx[i] < 0) continue;
		uint64_t lo[2] = { ~0ULL, ~0ULL }, hi[2] = { 0, 0 };
		for (int s = 0; s < 2; s++) for (int r = 0; r < reps; r++) {
			if (d[s][r][i] < lo[s]) lo[s] = d[s][r][i];
			if (d[s][r][i] > hi[s]) hi[s] = d[s][r][i];
		}
		printf("  %-6s %-18s N=%-9llu min %-12llu max %-12llu spread %-8llu | N=%-9llu min %-12llu max %-12llu spread %-8llu | slope %.4f per iteration\n",
		       what, EV[i], n1, lo[0], hi[0], hi[0] - lo[0], n2, lo[1], hi[1], hi[1] - lo[1],
		       ((double)lo[1] - (double)lo[0]) / (double)(n2 - n1));
	}
}
static void run_guest(uint64_t n) { guest_loop(n); }

int main(int argc, char **argv)
{
	(void)argc; (void)argv;
	void *kp = dlopen("/System/Library/PrivateFrameworks/kperf.framework/kperf", RTLD_LAZY);
	void *kd = dlopen("/System/Library/PrivateFrameworks/kperfdata.framework/kperfdata", RTLD_LAZY);
	printf("frameworks: kperf %s, kperfdata %s\n", kp ? "loaded" : "NOT LOADED", kd ? "loaded" : "NOT LOADED");
	if (!kp || !kd) return 1;
	SYM(kp, kpc_force_all_ctrs_set); SYM(kp, kpc_set_config); SYM(kp, kpc_set_counting);
	SYM(kp, kpc_set_thread_counting); SYM(kp, kpc_get_thread_counters); SYM(kp, kpc_get_counter_count);
	SYM(kd, kpep_db_create); SYM(kd, kpep_db_name); SYM(kd, kpep_db_events_count); SYM(kd, kpep_db_events);
	SYM(kd, kpep_event_name); SYM(kd, kpep_db_event); SYM(kd, kpep_config_create); SYM(kd, kpep_config_force_counters);
	SYM(kd, kpep_config_add_event); SYM(kd, kpep_config_kpc_classes); SYM(kd, kpep_config_kpc_count);
	SYM(kd, kpep_config_kpc); SYM(kd, kpep_config_kpc_map);

	kpep_db *db = NULL;
	if (kpep_db_create(NULL, &db) || !db) { printf("db: kpep_db_create failed\n"); return 1; }
	const char *name = "?"; kpep_db_name(db, &name);
	size_t nall = 0; kpep_db_events_count(db, &nall);
	printf("db: %s, %zu events\n", name, nall);
	for (int i = 0; EV[i]; i++) { kpep_event *e = NULL; printf("db: event %-18s %s\n", EV[i], !kpep_db_event(db, EV[i], &e) && e ? "present" : "absent"); }

	if (geteuid() != 0) { printf("not root: stopping before programming the counters (run with sudo for the measurements)\n"); return 0; }
	if (kpc_force_all_ctrs_set(1)) { printf("kpc_force_all_ctrs_set(1) failed\n"); return 1; }

	vm_setup();
	struct { const char *name; uint64_t modes; } M[] = { { "EL0", CFG_EL0A64 }, { "EL1", CFG_EL1 }, { "EL0+1", CFG_EL0A64 | CFG_EL1 } };
	for (int m = 0; m < 3; m++) {
		if (program(db, M[m].modes)) return 1;
		printf("mode %s (configurable counters; FIXED_* always count EL0+EL1):\n", M[m].name);
		measure("guest", run_guest, 1000000, 10000000, 10);
		measure("host", host_loop, 1000000, 10000000, 10);
	}
	kpc_set_counting(0); kpc_set_thread_counting(0); kpc_force_all_ctrs_set(0);
	return 0;
}

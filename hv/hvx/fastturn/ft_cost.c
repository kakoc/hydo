// SPDX-License-Identifier: GPL-2.0
/*
 * ft_cost - measure what one vCPU turn end costs (hv/hvx/FASTTURN.md).
 *
 * Two vCPUs run a loop of N guest conditional branches each and HLT; one HVX_RUN drives
 * both in turns of Q branches. The run is timed, and HVX_GET_FTSTAT gives the host-cycle
 * breakdown of every hvx_run_vcpu call (one call per turn): FPU/IRQ, VMXON+VMCLEAR+
 * VMPTRLD, PMU take-over, setup_vmcs, MSR/FPU load, landing steps, free running,
 * save, PMU give-back, VMCLEAR+VMXOFF, plus the PMU overflow skid histogram.
 *
 *   ft_cost <Q> [N] [kind] [reps] [burst]
 *     kind  0: `dec rcx; jnz` (2 instructions per branch, the tightest loop)
 *           1: 8 adds + `dec rcx; jnz` (10 instructions per branch)
 *           2: a 6-instruction body with a load and a store per branch
 *           3: 16 NOT-taken `test; jnz` + `dec rcx; jnz` (17 branches per 34
 *              instructions: the densest branch stream, for the worst skid); N/17 loops
 *           4: a pointer chase through 112 MiB (one cache miss per loop) followed by
 *              150 NOT-taken `test; jnz` independent of it, so retirement stalls on the
 *              load and then retires the 150 branches in a burst; N/151 loops
 *     burst B:s:p (optional) -- HVX_SET_TURNCTL bursts with seed 1; env FT_MARGIN = turn margin
 * Prints one line per run plus the breakdown per call (cycles and microseconds).
 */
#include <time.h>
#include "hvx_sched_util.h"

#define MEM	(2UL * 1024 * 1024)
#define CODE	HVX_GUEST_CODE_BASE

static const uint8_t code0[] = { 0x48, 0xff, 0xc9, 0x75, 0xfb, 0xf4 };	/* dec rcx; jnz; hlt */
static const uint8_t code1[] = {
	0x48, 0x83, 0xc0, 0x01, 0x48, 0x83, 0xc0, 0x01, 0x48, 0x83, 0xc0, 0x01,
	0x48, 0x83, 0xc0, 0x01, 0x48, 0x83, 0xc0, 0x01, 0x48, 0x83, 0xc0, 0x01,
	0x48, 0x83, 0xc0, 0x01, 0x48, 0x83, 0xc0, 0x01,	/* 8 x add $1,%rax */
	0x48, 0xff, 0xc9,				/* dec %rcx */
	0x75, 0xdb,					/* jnz -37 */
	0xf4,
};
static const uint8_t code2[] = {
	0x48, 0x8b, 0x07,		/* 1: mov (%rdi),%rax */
	0x48, 0x01, 0xc8,		/* add %rcx,%rax */
	0x48, 0x89, 0x07,		/* mov %rax,(%rdi) */
	0x48, 0x31, 0xd2,		/* xor %rdx,%rdx */
	0x48, 0xff, 0xc9,		/* dec %rcx */
	0x75, 0xef,			/* jnz 1b (-17) */
	0xf4,
};

static uint8_t code3[16 * 5 + 3 + 2 + 1];

static void build3(void)
{
	uint8_t *p = code3;
	int i;

	for (i = 0; i < 16; i++) {
		*p++ = 0x48; *p++ = 0x85; *p++ = 0xd2;	/* test %rdx,%rdx (rdx = 0) */
		*p++ = 0x75; *p++ = 0x00;		/* jnz .+2: never taken */
	}
	*p++ = 0x48; *p++ = 0xff; *p++ = 0xc9;		/* dec %rcx */
	*p++ = 0x75; *p++ = (uint8_t)(-(16 * 5 + 5));	/* jnz loop */
	*p++ = 0xf4;
}

static uint8_t code4[3 + 400 * 5 + 3 + 6 + 1];
static int k4n = 150;
static size_t code4_len;

static void build4(void)
{
	uint8_t *p = code4;
	int32_t rel;
	int i;

	*p++ = 0x48; *p++ = 0x8b; *p++ = 0x36;		/* 1: mov (%rsi),%rsi */
	for (i = 0; i < k4n; i++) {
		*p++ = 0x48; *p++ = 0x85; *p++ = 0xd2;	/* test %rdx,%rdx */
		*p++ = 0x75; *p++ = 0x00;		/* jnz .+2 (never taken) */
	}
	*p++ = 0x48; *p++ = 0xff; *p++ = 0xc9;		/* dec %rcx */
	*p++ = 0x0f; *p++ = 0x85;			/* jnz rel32 1b */
	rel = -(int32_t)(p + 4 - code4);
	memcpy(p, &rel, 4);
	p += 4;
	*p++ = 0xf4;
	code4_len = p - code4;
}

/* a random cycle over 4 KiB nodes in [16 MiB, mem) */
static void chain(uint8_t *mem, uint64_t memsz)
{
	uint64_t lo = 16ULL << 20, n = (memsz - lo) >> 12, i, *perm = malloc(n * 8);
	uint64_t x = 12345;

	for (i = 0; i < n; i++)
		perm[i] = i;
	for (i = n - 1; i > 0; i--) {
		uint64_t j, t;

		x ^= x << 13; x ^= x >> 7; x ^= x << 17;
		j = x % (i + 1);
		t = perm[i]; perm[i] = perm[j]; perm[j] = t;
	}
	for (i = 0; i < n; i++)
		*(uint64_t *)(mem + lo + (perm[i] << 12)) = lo + (perm[(i + 1) % n] << 12);
	free(perm);
}

static double now(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec + ts.tv_nsec * 1e-9;
}

int main(int argc, char **argv)
{
	uint64_t q = argc > 1 ? strtoull(argv[1], 0, 0) : 1000;
	uint64_t n = argc > 2 ? strtoull(argv[2], 0, 0) : 20000000;
	int kind = argc > 3 ? atoi(argv[3]) : 0;
	int reps = argc > 4 ? atoi(argv[4]) : 1;
	const char *burst = argc > 5 ? argv[5] : NULL;
	const uint8_t *code = kind == 1 ? code1 : kind == 2 ? code2 : kind == 3 ? code3 :
			      kind == 4 ? code4 : code0;
	size_t clen;
	uint64_t memsz = kind == 4 ? (128ULL << 20) : MEM;
	int rep;

	if (getenv("FT_K4N"))
		k4n = atoi(getenv("FT_K4N"));
	if (k4n < 1 || k4n > 400)
		k4n = 150;
	build3();
	build4();
	clen = kind == 1 ? sizeof(code1) : kind == 2 ? sizeof(code2) :
	       kind == 3 ? sizeof(code3) : kind == 4 ? code4_len : sizeof(code0);

	for (rep = 0; rep < reps; rep++) {
		struct su_vm vm;
		struct hvx_regs r;
		struct hvx_tsc t;
		struct hvx_sched s;
		struct hvx_ftstat f;
		struct hvx_pmu pm;
		unsigned int i;
		double t0, t1, khz;
		uint64_t calls = 0, tot = 0, steps = 0;

		if (su_create(&vm, memsz, 2, HVX_VM_PMU_CLOCK | HVX_VM_SMP_SCHED) < 0) {
			if (errno == ENODEV) {
				printf("ft_cost: SKIP (no PMU)\n");
				return 0;
			}
			su_die("HVX_CREATE_VM");
		}
		memcpy(vm.mem + CODE, code, clen);
		if (kind == 4)
			chain(vm.mem, memsz);
		for (i = 0; i < 2; i++) {
			memset(&r, 0, sizeof(r));
			r.rip = CODE;
			r.rcx = kind == 3 ? n / 17 : kind == 4 ? n / (k4n + 1) : n;
			r.rdi = 0x100000 + 0x1000 * i;
			r.rsi = (16ULL << 20) + 0x1000 * i;
			r.rsp = 0x100000 - 16 - 0x1000 * i;
			r.rflags = 0x2;
			su_set_regs(&vm, i, &r);
		}
		memset(&t, 0, sizeof(t));
		t.tsc = 1000;
		t.step = 1;
		t.mode = HVX_TSC_COUNTER;
		if (ioctl(vm.fd, HVX_SET_TSC, &t) < 0)
			su_die("SET_TSC");
		su_pmu(&vm, 1ULL << 40, 1000000);	/* no ticks: only turn ends land */
		su_policy(&vm, q, 0, 0);
		if (burst || getenv("FT_MARGIN")) {
			struct hvx_turnctl b;
			unsigned long long bl = 0, bq = 0;
			unsigned int pm = 0;

			memset(&b, 0, sizeof(b));
			if (getenv("FT_MARGIN"))
				b.margin = strtoull(getenv("FT_MARGIN"), 0, 0);
			if (burst) {
				if (sscanf(burst, "%llu:%llu:%u", &bl, &bq, &pm) != 3)
					su_die("burst B:s:p");
				b.burst_seed = 1;
				b.burst_len = bl;
				b.burst_q = bq;
				b.burst_permille = pm;
			}
			if (ioctl(vm.fd, HVX_SET_TURNCTL, &b) < 0)
				su_die("HVX_SET_TURNCTL");
		}
		if (ioctl(vm.fd, HVX_RESET_FTSTAT) < 0)
			su_die("HVX_RESET_FTSTAT");
		t0 = now();
		su_run(&vm, NULL);
		t1 = now();
		su_get_sched(&vm, &s);
		if (ioctl(vm.fd, HVX_GET_FTSTAT, &f) < 0)
			su_die("HVX_GET_FTSTAT");
		khz = (double)f.tsc_khz;
		for (i = 0; i < 2; i++) {
			su_set_vcpu(&vm, i);
			if (ioctl(vm.fd, HVX_GET_PMU, &pm) < 0)
				su_die("GET_PMU");
		}
		printf("ft_cost q=%" PRIu64 " n=%" PRIu64 " kind=%d: %.3f s, turns=%" PRIu64
		       " (%.1f us/turn) overshoots=%" PRIu64 " deferred=%" PRIu64 " total=%" PRIu64
		       " turnhash=%016" PRIx64 "\n",
		       q, n, kind, t1 - t0, s.turns, s.turns ? (t1 - t0) * 1e6 / s.turns : 0.0,
		       s.overshoots, s.deferred, s.pmu_total, s.turn_hash);
		{
			struct hvx_ftstat_vcpu a;
			__u64 *dst = (__u64 *)&a;
			size_t k, nf = sizeof(a) / 8;

			memset(&a, 0, sizeof(a));
			for (i = 0; i < 2; i++) {
				const __u64 *src = (const __u64 *)&f.v[i];

				for (k = 0; k < nf; k++)
					dst[k] += src[k];
				if (f.v[i].skid_max > a.skid_max)
					a.skid_max = f.v[i].skid_max;
			}
			a.skid_max = f.v[0].skid_max > f.v[1].skid_max ? f.v[0].skid_max : f.v[1].skid_max;
			calls = a.calls ? a.calls : 1;
			tot = a.total_cyc;
			steps = a.step_entries;
#define PC(name, v) printf("  %-10s %10.0f cyc %8.2f us per call\n", name, (double)(v) / calls, (double)(v) / calls / khz * 1000.0)
			printf("  calls=%" PRIu64 " sched_cyc/turn=%.0f (%.2f us) steps/call=%.1f free entries/call=%.1f\n",
			       a.calls, f.sched_turns ? (double)f.sched_cyc / f.sched_turns : 0.0,
			       f.sched_turns ? (double)f.sched_cyc / f.sched_turns / khz * 1000 : 0.0,
			       (double)steps / calls, (double)a.free_entries / calls);
			PC("total", tot);
			PC("fpu+irq", a.fpu_cyc);
			PC("vmxon", a.vmxon_cyc);
			PC("pmu-on", a.pmu_cyc);
			PC("setupvmcs", a.vmcs_cyc);
			PC("load", a.load_cyc);
			PC("steps", a.step_cyc);
			PC("free", a.free_cyc);
			PC("save", a.save_cyc);
			PC("pmu-off", a.pmuoff_cyc);
			PC("vmxoff", a.vmxoff_cyc);
			printf("  per step %.0f cyc (VMRESUME..exit %.0f); ovf=%" PRIu64 " skid max=%" PRIu64 " mean=%.2f rearms=%" PRIu64
			       " resident=%" PRIu64 "\n  skid hist (0,1,2-3,4-7,..,>=256):",
			       steps ? (double)a.step_cyc / steps : 0.0,
			       steps ? (double)a.step_hw_cyc / steps : 0.0, a.ovf, a.skid_max,
			       a.ovf ? (double)a.skid_sum / a.ovf : 0.0, a.rearms, a.resident);
			for (k = 0; k < HVX_FTSTAT_SKID_BUCKETS; k++)
				printf(" %" PRIu64, a.skid_hist[k]);
			printf("\n");
		}
		su_destroy(&vm);
	}
	return 0;
}

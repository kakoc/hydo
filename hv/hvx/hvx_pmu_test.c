// SPDX-License-Identifier: GPL-2.0
/*
 * PMU branch clock test (HVX_VM_PMU_CLOCK). Needs a bare-metal Intel PMU.
 *
 * Guest:  mov $N,%rcx ; 1: dec %rcx ; jnz 1b ; hlt     -- exactly N conditional branches.
 *
 *   A. EXACT COUNT   -- with a period too long to ever tick, the guest-only counter must
 *                       read exactly N at the HLT, every run, although the run is split
 *                       by thousands of host-interrupt exits.
 *   B. EXACT TICKS   -- with a short period the guest takes N/period ticks. Each tick is
 *                       landed by counter overflow + MTF single-steps; the hash of the
 *                       landing points (rip, rcx, rflags) and the final icount/TSC must be
 *                       identical across runs, with zero overshoots.
 *   C. SNAPSHOT      -- stop mid-run at a THROTTLE boundary, snapshot, finish; restore,
 *                       finish again: same final count, icount and TSC.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include "hvx_abi.h"

#define MEM   (2 * 1024 * 1024)
#define N     200000000ULL

struct res { uint64_t total, ticks, over, steps, land, icount, tsc, rcx; };

static int vm_open(uint8_t **mem)
{
	struct hvx_config cfg; int fd = open("/dev/hvx", O_RDWR);
	static const uint8_t code[] = {
		0x48, 0xb9, 0,0,0,0,0,0,0,0,		/* movabs $N,%rcx */
		0x48, 0xff, 0xc9,			/* 1: dec %rcx */
		0x75, 0xfb,				/* jnz 1b */
		0xf4,					/* hlt */
	};
	uint64_t n = N;
	if (fd < 0) { perror("open /dev/hvx"); exit(1); }
	memset(&cfg, 0, sizeof(cfg)); cfg.mem_size = MEM; cfg.flags = HVX_VM_PMU_CLOCK;
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) {
		if (errno == ENODEV) { printf("hvx_pmu_test: SKIP (no usable PMU on this host)\n"); exit(0); }
		perror("HVX_CREATE_VM"); exit(1);
	}
	*mem = mmap(NULL, MEM, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (*mem == MAP_FAILED) { perror("mmap"); exit(1); }
	memcpy(*mem + HVX_GUEST_CODE_BASE, code, sizeof(code));
	memcpy(*mem + HVX_GUEST_CODE_BASE + 2, &n, 8);
	return fd;
}

static void start(int fd, uint64_t period)
{
	struct hvx_regs r; struct hvx_tsc t; struct hvx_pmu pm;
	memset(&r, 0, sizeof(r)); r.rip = HVX_GUEST_CODE_BASE; r.rsp = MEM - 16; r.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &r) < 0) { perror("SET_REGS"); exit(1); }
	memset(&t, 0, sizeof(t)); t.tsc = 1000; t.step = 1; t.mode = HVX_TSC_COUNTER;
	if (ioctl(fd, HVX_SET_TSC, &t) < 0) { perror("SET_TSC"); exit(1); }
	memset(&pm, 0, sizeof(pm)); pm.period = period; pm.tick_tsc = 1000000;
	if (ioctl(fd, HVX_SET_PMU, &pm) < 0) { perror("SET_PMU"); exit(1); }
}

/* run to HLT; if stop_after_throttles > 0, return after that many THROTTLEs instead */
static int run(int fd, int stop_after_throttles)
{
	struct hvx_exit ex; int th = 0;
	for (;;) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(fd, HVX_RUN, &ex) < 0) { if (errno == EINTR) continue; perror("HVX_RUN"); exit(1); }
		if (ex.reason == 12) return 0;			/* HLT */
		if (ex.reason == HVX_EXIT_THROTTLE) { if (stop_after_throttles && ++th >= stop_after_throttles) return 1; continue; }
		fprintf(stderr, "unexpected exit reason %u\n", ex.reason); exit(1);
	}
}

static void collect(int fd, struct res *o)
{
	struct hvx_pmu pm; struct hvx_snapshot *s = calloc(1, sizeof(*s));
	if (ioctl(fd, HVX_GET_PMU, &pm) < 0) { perror("GET_PMU"); exit(1); }
	if (ioctl(fd, HVX_GET_SNAPSHOT, s) < 0) { perror("GET_SNAPSHOT"); exit(1); }
	o->total = pm.total; o->ticks = pm.ticks; o->over = pm.overshoots; o->steps = pm.steps;
	o->land = pm.land_hash; o->icount = s->icount; o->tsc = s->tsc; o->rcx = s->regs.rcx;
	free(s);
}

static void one(uint64_t period, struct res *o)
{
	uint8_t *mem; int fd = vm_open(&mem);
	start(fd, period); run(fd, 0); collect(fd, o);
	munmap(mem, MEM); close(fd);
}

int main(void)
{
	struct res a[3], b[4], c1, c2; int i, ok = 1;
	const uint64_t P = 1000003ULL;

	for (i = 0; i < 3; i++) {
		one(1ULL << 40, &a[i]);
		printf("A run %d: branches=%llu (want %llu) ticks=%llu rcx=%llu\n", i,
		       (unsigned long long)a[i].total, (unsigned long long)N,
		       (unsigned long long)a[i].ticks, (unsigned long long)a[i].rcx);
		if (a[i].total != N || a[i].ticks != 0 || a[i].rcx != 0) ok = 0;
	}
	for (i = 0; i < 4; i++) {
		one(P, &b[i]);
		printf("B run %d: branches=%llu ticks=%llu (want %llu) overshoots=%llu steps=%llu land=%016llx icount=%llu tsc=%llu\n",
		       i, (unsigned long long)b[i].total, (unsigned long long)b[i].ticks,
		       (unsigned long long)(N / P), (unsigned long long)b[i].over,
		       (unsigned long long)b[i].steps, (unsigned long long)b[i].land,
		       (unsigned long long)b[i].icount, (unsigned long long)b[i].tsc);
		if (b[i].total != N || b[i].ticks != N / P || b[i].over != 0) ok = 0;
		if (b[i].land != b[0].land || b[i].icount != b[0].icount || b[i].tsc != b[0].tsc) ok = 0;
	}
	{	/* C: stop after 100 ticks, snapshot, finish; restore, finish again */
		uint8_t *mem; int fd = vm_open(&mem);
		struct hvx_snapshot *s = calloc(1, sizeof(*s)); uint8_t *save = malloc(MEM);
		struct hvx_run_ctl ctl;
		start(fd, P);
		memset(&ctl, 0, sizeof(ctl)); ctl.max_insns = 100;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0 || ctl.reason != HVX_EXIT_STEP) {
			fprintf(stderr, "C: RUNC did not stop at tick 100 (reason %u)\n", ctl.reason); return 1;
		}
		if (ioctl(fd, HVX_GET_SNAPSHOT, s) < 0) { perror("GET_SNAPSHOT"); return 1; }
		memcpy(save, mem, MEM);
		run(fd, 0); collect(fd, &c1);
		memcpy(mem, save, MEM);
		if (ioctl(fd, HVX_SET_SNAPSHOT, s) < 0) { perror("SET_SNAPSHOT"); return 1; }
		run(fd, 0); collect(fd, &c2);
		printf("C snapshot at branches=%llu rcx=%llu | straight: branches=%llu icount=%llu tsc=%llu | restored: branches=%llu icount=%llu tsc=%llu\n",
		       (unsigned long long)s->pmu_total, (unsigned long long)s->regs.rcx,
		       (unsigned long long)c1.total, (unsigned long long)c1.icount, (unsigned long long)c1.tsc,
		       (unsigned long long)c2.total, (unsigned long long)c2.icount, (unsigned long long)c2.tsc);
		if (s->pmu_total != 100 * P || s->regs.rcx != N - 100 * P) ok = 0;	/* the tick is instruction-exact */
		if (c1.total != N || c2.total != N || c1.icount != c2.icount || c1.tsc != c2.tsc ||
		    c1.icount != b[0].icount || c1.tsc != b[0].tsc) ok = 0;
		munmap(mem, MEM); close(fd);
	}
	printf(ok ? "hvx_pmu_test: PASS (exact guest branch count, instruction-exact ticks, snapshot-safe)\n"
		  : "hvx_pmu_test: FAIL\n");
	return ok ? 0 : 1;
}

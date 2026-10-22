/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hvx_sched_util.h - shared helpers for the in-kernel vCPU scheduler tests
 * (HVX_VM_SMP_SCHED): create an SMP VM, run it to the end, take and restore a full
 * snapshot (every vCPU + scheduler + RAM), and a canonical fingerprint.
 *
 * The device is $HVX_DEV if set (e.g. /dev/hvxsmp, a side-by-side module), else /dev/hvx.
 */
#ifndef HVX_SCHED_UTIL_H
#define HVX_SCHED_UTIL_H

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include "hvx_abi.h"

#define SU_EXIT_HLT	12
#define SU_GPA_ISR	0x2800UL	/* must match GPA_ISR in hvx.c (IDT gates 0..0x20 point here) */
#define SU_GPA_INTR	0x2f00UL	/* must match GPA_INTR_COUNT */

struct su_vm {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	unsigned int nv;
};

static inline uint64_t su_fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;

	while (n--) {
		h ^= *b++;
		h *= 0x100000001b3ULL;
	}
	return h;
}

static inline void su_die(const char *what)
{
	perror(what);
	exit(2);
}

/* returns 0, or -1 with errno (ENODEV: no usable PMU) */
static inline int su_create(struct su_vm *vm, uint64_t mem_size, unsigned int nv, uint32_t flags)
{
	struct hvx_config cfg;
	const char *dev = getenv("HVX_DEV");

	vm->fd = open(dev ? dev : "/dev/hvx", O_RDWR);
	if (vm->fd < 0)
		su_die("open hvx device");
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	cfg.n_vcpus = nv;
	cfg.flags = flags;
	if (ioctl(vm->fd, HVX_CREATE_VM, &cfg) < 0) {
		int e = errno;

		close(vm->fd);
		errno = e;
		return -1;
	}
	vm->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, vm->fd, 0);
	if (vm->mem == MAP_FAILED)
		su_die("mmap");
	vm->mem_size = mem_size;
	vm->nv = nv;
	return 0;
}

static inline void su_destroy(struct su_vm *vm)
{
	munmap(vm->mem, vm->mem_size);
	close(vm->fd);
}

static inline void su_set_vcpu(struct su_vm *vm, uint32_t i)
{
	if (ioctl(vm->fd, HVX_SET_VCPU, &i) < 0)
		su_die("HVX_SET_VCPU");
}

static inline void su_set_regs(struct su_vm *vm, uint32_t i, const struct hvx_regs *r)
{
	su_set_vcpu(vm, i);
	if (ioctl(vm->fd, HVX_SET_REGS, r) < 0)
		su_die("HVX_SET_REGS");
}

static inline void su_get_sched(struct su_vm *vm, struct hvx_sched *s)
{
	if (ioctl(vm->fd, HVX_GET_SCHED, s) < 0)
		su_die("HVX_GET_SCHED");
}

static inline void su_set_sched(struct su_vm *vm, const struct hvx_sched *s)
{
	if (ioctl(vm->fd, HVX_SET_SCHED, s) < 0)
		su_die("HVX_SET_SCHED");
}

/* policy knobs: quantum, seed, pause_k (the rest of the state is kept) */
static inline void su_policy(struct su_vm *vm, uint64_t quantum, uint64_t seed, uint32_t pause_k)
{
	struct hvx_sched s;

	su_get_sched(vm, &s);
	s.quantum = quantum;
	s.seed = seed;
	s.pause_k = pause_k;
	su_set_sched(vm, &s);
}

static inline void su_stop_turn(struct su_vm *vm, uint64_t t)
{
	struct hvx_sched s;

	su_get_sched(vm, &s);
	s.stop_turn = t;
	su_set_sched(vm, &s);
}

static inline void su_pmu(struct su_vm *vm, uint64_t period, uint64_t tick_tsc)
{
	struct hvx_pmu pm;

	memset(&pm, 0, sizeof(pm));
	pm.period = period;
	pm.tick_tsc = tick_tsc;
	if (ioctl(vm->fd, HVX_SET_PMU, &pm) < 0)
		su_die("HVX_SET_PMU");
}

/*
 * Run until every vCPU has halted with nothing left to wake it (HLT returned) or, with
 * stop_turn set, until HVX_EXIT_TURN. Returns the final exit reason. THROTTLE is
 * transparent. Any other exit is fatal.
 */
static inline uint32_t su_run(struct su_vm *vm, unsigned long *throttles)
{
	struct hvx_exit ex;

	for (;;) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(vm->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			su_die("HVX_RUN");
		}
		if (ex.reason == HVX_EXIT_THROTTLE) {
			if (throttles)
				(*throttles)++;
			continue;
		}
		if (ex.reason == SU_EXIT_HLT || ex.reason == HVX_EXIT_TURN)
			return ex.reason;
		fprintf(stderr, "su_run: unexpected exit reason 0x%x qual=0x%" PRIx64 " gpa=0x%" PRIx64 "\n",
			ex.reason, (uint64_t)ex.qualification, (uint64_t)ex.guest_phys);
		exit(2);
	}
}

/* one HVX_RUNC with a max_insns budget (stop mid-turn); returns the reason */
static inline uint32_t su_runc(struct su_vm *vm, uint64_t max_insns)
{
	struct hvx_run_ctl c;

	memset(&c, 0, sizeof(c));
	c.max_insns = max_insns;
	if (ioctl(vm->fd, HVX_RUNC, &c) < 0)
		su_die("HVX_RUNC");
	return c.reason;
}

/* full state: every vCPU's snapshot + scheduler + RAM */
struct su_snap {
	struct hvx_snapshot v[HVX_SCHED_MAX_VCPUS];
	struct hvx_sched sch;
	uint8_t *ram;
};

static inline void su_save(struct su_vm *vm, struct su_snap *s)
{
	unsigned int i;

	for (i = 0; i < vm->nv; i++) {
		su_set_vcpu(vm, i);
		if (ioctl(vm->fd, HVX_GET_SNAPSHOT, &s->v[i]) < 0)
			su_die("HVX_GET_SNAPSHOT");
	}
	su_get_sched(vm, &s->sch);
	if (!s->ram && !(s->ram = malloc(vm->mem_size)))
		su_die("malloc");
	memcpy(s->ram, vm->mem, vm->mem_size);
}

static inline void su_restore(struct su_vm *vm, const struct su_snap *s, uint64_t stop_turn)
{
	struct hvx_sched sch = s->sch;
	unsigned int i;

	memcpy(vm->mem, s->ram, vm->mem_size);
	for (i = 0; i < vm->nv; i++) {
		su_set_vcpu(vm, i);
		if (ioctl(vm->fd, HVX_SET_SNAPSHOT, &s->v[i]) < 0)
			su_die("HVX_SET_SNAPSHOT");
	}
	sch.stop_turn = stop_turn;
	su_set_sched(vm, &sch);
}

/*
 * Canonical fingerprint: each vCPU's whole snapshot struct (the kernel zero-fills it, so
 * padding is stable), the scheduler STATE (not its statistics, not stop_turn), and RAM.
 */
static inline uint64_t su_fingerprint(struct su_vm *vm)
{
	uint64_t h = 0xcbf29ce484222325ULL;
	struct hvx_snapshot sn;
	struct hvx_sched s;
	unsigned int i;

	h = su_fnv(h, "hvx-sched-v1", 12);
	for (i = 0; i < vm->nv; i++) {
		su_set_vcpu(vm, i);
		memset(&sn, 0, sizeof(sn));
		if (ioctl(vm->fd, HVX_GET_SNAPSHOT, &sn) < 0)
			su_die("HVX_GET_SNAPSHOT");
		/* the guest-AVX fields at the end only for an AVX VM (xcr0 != 0), so other
		 * VMs hash exactly the bytes they hashed before those fields were appended */
		h = su_fnv(h, &sn, sn.xcr0 ? sizeof(sn) : offsetof(struct hvx_snapshot, xcr0));
	}
	su_get_sched(vm, &s);
	h = su_fnv(h, &s.quantum, sizeof(s.quantum));
	h = su_fnv(h, &s.seed, sizeof(s.seed));
	h = su_fnv(h, &s.pause_k, sizeof(s.pause_k));
	h = su_fnv(h, &s.cur, sizeof(s.cur));
	h = su_fnv(h, &s.in_turn, sizeof(s.in_turn));
	h = su_fnv(h, &s.turn_over, sizeof(s.turn_over));
	h = su_fnv(h, &s.pause_run, sizeof(s.pause_run));
	h = su_fnv(h, &s.turn_end, sizeof(s.turn_end));
	h = su_fnv(h, &s.turns, sizeof(s.turns));
	h = su_fnv(h, &s.guard, sizeof(s.guard));
	h = su_fnv(h, &s.turn_hash, sizeof(s.turn_hash));
	h = su_fnv(h, &s.tsc, 6 * sizeof(uint64_t));		/* tsc .. pmu_next */
	for (i = 0; i < vm->nv; i++)
		h = su_fnv(h, &s.v[i], sizeof(s.v[i]));
	h = su_fnv(h, vm->mem, vm->mem_size);
	return h;
}

#endif

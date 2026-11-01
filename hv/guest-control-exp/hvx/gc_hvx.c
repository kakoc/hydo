// SPDX-License-Identifier: GPL-2.0
/*
 * M116 feasibility experiment (docs/design/guest-control.md): does a host command typed on the
 * serial input at an exact, reproducible point of virtual time keep every fingerprint
 * reproducible -- across runs, from a restored S1 (serve) against a fresh boot (one), and from a
 * mid-run branch (BRANCH)?
 *
 * This is hv/hvx/hvx_explore.c compiled UNCHANGED, with every ioctl() it makes routed through
 * gc_ioctl() below. Nothing here changes the module, hvx_abi.h or hvx_explore.c. With no GC_*
 * variable set, gc_ioctl only forwards, so the binary behaves exactly like hvx_explore.
 *
 * The point of time is the module's instruction clock `icount` (retired deterministic exits +
 * 2048 per PMU tick; struct hvx_run_ctl.icount), taken relative to the icount at which the
 * schedule's "run ..." line was typed (S1). Once a schedule is running, every HVX_RUN becomes an
 * HVX_RUNC whose event budget is capped so that no call runs past a pending target; the line is
 * typed (HVX_PUT_INPUT) at the first stop at which icount >= target. That stop is a pure function
 * of the guest's execution, so it is the same in every run and in every restored copy.
 *
 *   GC_AT="<delta>:<text>[;<delta>:<text>...]"   type <text>\n at icount(S1) + delta
 *   GC_REACT="<marker>@<delay>:<text>"           when <marker> first appears in the console of
 *                                                a run (seen at a stop), type <text>\n at the
 *                                                icount of that stop + delay (a reactive decision)
 *   GC_RESTORE=gt|ge                             after HVX_SET_SNAPSHOT, deliver the targets
 *                                                > (gt, default) or >= (ge) the restored icount
 * Every delivery is logged on stderr: "GC put icount=<n> branches=<pmu total> rel=<n> text=...".
 */
#include <sys/ioctl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int gc_ioctl(int fd, unsigned long req, ...);
#define ioctl gc_ioctl
#include "hvx_explore.c"
#undef ioctl

#define GC_MAX 16
static struct gc_t { uint64_t at; int abs; int done; char text[256]; } gc_t[GC_MAX];
static int gc_n = -1, gc_live;
static uint64_t gc_run0, gc_cur;
static int gc_cur_known, gc_done_seen;
static char gc_marker[128], gc_rtext[256];
static uint64_t gc_rdelay;
static int gc_rslot = -1;

static void gc_parse(void)
{
	const char *at = getenv("GC_AT"), *re = getenv("GC_REACT");
	gc_n = 0;
	if (at && *at) {
		char buf[4096], *save = NULL, *tok;
		snprintf(buf, sizeof(buf), "%s", at);
		for (tok = strtok_r(buf, ";", &save); tok && gc_n < GC_MAX; tok = strtok_r(NULL, ";", &save)) {
			char *c = strchr(tok, ':');
			if (!c) continue;
			*c = 0;
			gc_t[gc_n].at = strtoull(tok, NULL, 0);
			snprintf(gc_t[gc_n].text, sizeof(gc_t[gc_n].text), "%s", c + 1);
			gc_n++;
		}
	}
	if (re && *re) {
		const char *a = strrchr(re, '@'), *c = a ? strchr(a, ':') : NULL;
		if (a && c && (size_t)(a - re) < sizeof(gc_marker)) {
			memcpy(gc_marker, re, (size_t)(a - re));
			gc_marker[a - re] = 0;
			gc_rdelay = strtoull(a + 1, NULL, 0);
			snprintf(gc_rtext, sizeof(gc_rtext), "%s", c + 1);
		}
	}
}

static uint64_t gc_branches(int fd)
{
	struct hvx_pmu pm;
	memset(&pm, 0, sizeof(pm));
	if (ioctl(fd, HVX_GET_PMU, &pm) < 0) return 0;
	return pm.total;
}

static void gc_put(int fd, struct gc_t *t)
{
	struct hvx_input in;
	size_t len = strlen(t->text);
	memset(&in, 0, sizeof(in));
	if (len + 1 > HVX_INPUT_MAX) len = HVX_INPUT_MAX - 1;
	memcpy(in.buf, t->text, len);
	/* GC_NONL=1: no newline. The image's init reads its next command line from the same tty
	 * after the workload; a complete stray line would be taken as that command (and the guest
	 * would run on without waiting for the host's "go"), a partial one is only echoed. */
	if (getenv("GC_NONL")) {
		in.len = (uint32_t)len;
	} else {
		in.buf[len] = '\n';
		in.len = (uint32_t)(len + 1);
	}
	if (ioctl(fd, HVX_PUT_INPUT, &in) < 0) perror("GC HVX_PUT_INPUT");
	t->done = 1;
	fprintf(stderr, "GC put icount=%llu branches=%llu rel=%llu text=%s\n",
		(unsigned long long)gc_cur, (unsigned long long)gc_branches(fd),
		(unsigned long long)(gc_cur - gc_run0), t->text);
}

static uint64_t gc_target(const struct gc_t *t) { return t->abs ? t->at : gc_run0 + t->at; }

/* deliver what is due at the current stop; return the event budget cap (0 = none) */
static uint64_t gc_due(int fd)
{
	uint64_t cap = 0;
	int i;
	if (!gc_live || !gc_cur_known) return 0;
	for (i = 0; i < gc_n; i++) {
		uint64_t tg;
		if (gc_t[i].done) continue;
		tg = gc_target(&gc_t[i]);
		if (gc_cur >= tg) { gc_put(fd, &gc_t[i]); continue; }
		{	/* an event adds 1 (a retired exit) or 2048 (a PMU tick) to icount */
			uint64_t c = (tg - gc_cur) / 2048;
			if (c < 1) c = 1;
			if (!cap || c < cap) cap = c;
		}
	}
	return cap;
}

static void gc_snapshot_icount(int fd)
{
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	if (s && ioctl(fd, HVX_GET_SNAPSHOT, s) == 0) { gc_cur = s->icount; gc_cur_known = 1; }
	free(s);
}

static int gc_ioctl(int fd, unsigned long req, ...)
{
	va_list ap;
	void *arg;
	int r;
	va_start(ap, req);
	arg = va_arg(ap, void *);
	va_end(ap);
	if (gc_n < 0) gc_parse();
	if (!gc_n && !gc_marker[0]) return ioctl(fd, req, arg);

	if (req == HVX_CREATE_VM) {
		gc_cur_known = 0;
		return ioctl(fd, req, arg);
	}
	if (req == HVX_PUT_INPUT) {
		const struct hvx_input *in = arg;
		r = ioctl(fd, req, arg);
		if (in->len >= 4 && !memcmp(in->buf, "run", 3) && (in->buf[3] == ' ' || in->buf[3] == '\n')) {
			int i;
			gc_snapshot_icount(fd);
			gc_run0 = gc_cur;
			gc_live = 1;
			gc_done_seen = 0;
			for (i = 0; i < gc_n; i++) gc_t[i].done = 0;
			if (gc_rslot >= 0) { gc_n = gc_rslot; gc_rslot = -1; }	/* forget the last run's reaction */
			fprintf(stderr, "GC run icount=%llu branches=%llu\n",
				(unsigned long long)gc_run0, (unsigned long long)gc_branches(fd));
		}
		return r;
	}
	if (req == HVX_SET_SNAPSHOT) {
		const struct hvx_snapshot *s = arg;
		int ge = getenv("GC_RESTORE") && !strcmp(getenv("GC_RESTORE"), "ge"), i;
		r = ioctl(fd, req, arg);
		gc_cur = s->icount;
		gc_done_seen = 0;
		gc_cur_known = 1;
		if (gc_live)
			for (i = 0; i < gc_n; i++) {
				uint64_t tg = gc_target(&gc_t[i]);
				gc_t[i].done = ge ? tg < gc_cur : tg <= gc_cur;
			}
		if (gc_live) fprintf(stderr, "GC restore icount=%llu rel=%llu\n",
				     (unsigned long long)gc_cur, (unsigned long long)(gc_cur - gc_run0));
		return r;
	}
	if (req == HVX_GET_CONSOLE) {
		r = ioctl(fd, req, arg);
		if (r == 0 && gc_live && gc_cur_known && !gc_done_seen) {	/* where the workload ended */
			const struct hvx_console *c = arg;
			size_t len = c->len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : c->len;
			if (mem_find(c->buf, len, DONE_MARKER) >= 0) {
				gc_done_seen = 1;
				fprintf(stderr, "GC workload-done seen at icount=%llu rel=%llu\n",
					(unsigned long long)gc_cur, (unsigned long long)(gc_cur - gc_run0));
			}
		}
		if (r == 0 && gc_live && gc_marker[0] && gc_rslot < 0 && gc_n < GC_MAX && gc_cur_known) {
			const struct hvx_console *c = arg;
			size_t len = c->len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : c->len;
			if (mem_find(c->buf, len, gc_marker) >= 0) {
				gc_rslot = gc_n;
				gc_t[gc_n].at = gc_cur + gc_rdelay;
				gc_t[gc_n].abs = 1;
				gc_t[gc_n].done = 0;
				snprintf(gc_t[gc_n].text, sizeof(gc_t[gc_n].text), "%s", gc_rtext);
				gc_n++;
				fprintf(stderr, "GC react marker seen at icount=%llu rel=%llu -> target %llu\n",
					(unsigned long long)gc_cur, (unsigned long long)(gc_cur - gc_run0),
					(unsigned long long)(gc_cur + gc_rdelay));
			}
		}
		return r;
	}
	if (gc_live && (req == HVX_RUN || req == HVX_RUNC)) {
		struct hvx_run_ctl c;
		uint64_t cap;
		if (!gc_cur_known) gc_snapshot_icount(fd);
		cap = gc_due(fd);
		memset(&c, 0, sizeof(c));
		if (req == HVX_RUNC) c = *(struct hvx_run_ctl *)arg;
		if (cap && (!c.max_insns || cap < c.max_insns)) c.max_insns = cap;
		r = ioctl(fd, HVX_RUNC, &c);
		if (r < 0) return r;
		gc_cur = c.icount;
		gc_cur_known = 1;
		if (req == HVX_RUNC) {
			*(struct hvx_run_ctl *)arg = c;
		} else {
			struct hvx_exit *ex = arg;
			memset(ex, 0, sizeof(*ex));
			/* a capped stop looks like a THROTTLE to an HVX_RUN caller: re-run to continue */
			ex->reason = c.reason == HVX_EXIT_STEP ? HVX_EXIT_THROTTLE : c.reason;
			ex->insn_len = c.insn_len;
			ex->qualification = c.qualification;
			ex->guest_phys = c.guest_phys;
		}
		gc_due(fd);	/* a target reached exactly at this stop is typed now */
		return r;
	}
	return ioctl(fd, req, arg);
}

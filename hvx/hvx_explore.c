// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_explore - the run engine of the automatic fault explorer (milestone 38).
 *
 * Boots the four-service stack guest built with STACK_OUT=explore (build-stack.sh +
 * hvx/explore/stack-explore.sh) in its command-driven "serve" mode and snapshots it twice:
 *
 *   S0  root mounted, data disk not mounted yet       -> start of every recovery boot
 *   S1  all services up, provisioned, synced, idle    -> start of every fault schedule
 *
 * Each schedule then starts from S1 in a FRESH VM (the module's console buffer is per VM and
 * never reset, so a long-lived VM would eventually stop recording output): load the base
 * disk, restore S1 (CPU/device state + RAM + rw data disk), arm the schedule's disk fault,
 * and type the schedule's guest part on the serial console. Without a power cut the guest
 * heals its faults and checks its invariant in the same boot; with one, the flushed data
 * disk is carried into a fresh VM restored from S0, which runs the recovery and the check.
 *
 * A schedule has a host part (disk fault, tick timing) and a guest part (service kill,
 * network fault, power-cut point, durability knobs), separated by '|':
 *   disk=<none|ioerr:P|cut|lying:P> dseed=<hex> [tj=<hex> tjpm=<n> tjmax=<n> [tjlo=<k> tjhi=<k>]
 *   | tjx=<k:d,k:d,...>] | run [K=..] [N=..] [C=..] [M=..] [R=..]
 * The tj tokens delay PMU-clock ticks during the workload (HVX_SET_TJITTER: seeded, or an
 * explicit tick:delay list) so the guest's threads interleave differently; a schedule
 * without them runs exactly as before. When jitter is on, the RESULT line also reports the
 * workload's tick range and how many ticks were delayed (tjk=<first>-<last> tjn=<n>).
 * rb=<n> makes every run call return after at most n events (HVX_RUNC), from S1 on: the
 * return points, and so the end state, are then the same as in a branch (below).
 *
 * Branching (mid-run snapshots): "BRANCH <tag> <n> <marker> <schedule>" starts the schedule
 * from S1 without timing jitter and with rb=16, stops at the first return after <marker>
 * (no spaces) appears in the workload's console, snapshots there (S2), finishes that run
 * ("<tag>-base"), then runs n timing variants from S2 ("<tag>-v1".."-vn"), each in a fresh
 * VM with seeded jitter (seed v) on the ticks after S2 only. Variant v is exactly the plain
 * schedule  <host> tj=<v> tjpm=<p> tjmax=<m> tjlo=<k> rb=16 | <guest>, with k from the
 * "BRANCHPOINT <tag> tjlo=<k>" line, so a finding reproduces and shrinks like any other.
 * Not for power-cut schedules: the module's volatile write cache is not in the snapshot.
 * Output: BRANCHPOINT, then n+1 RESULT lines, then "BRANCHEND <tag>".
 * (guest tokens: see hvx/explore/stack-explore.sh). The schedule generator, the
 * reproduce/shrink logic and the report live in hvx/explore/explore.py.
 *
 * Modes (as root, hvx loaded, through ~/bin/hvx-run):
 *   hvx_explore serve <bzImage> <initramfs> <explore-disk.img> <outdir>
 *       reads "RUN <tag> <schedule>" lines on stdin, answers "RESULT <tag> ..." lines
 *   hvx_explore one <bzImage> <initramfs> <explore-disk.img> <outdir> "<schedule>"
 *       one schedule from a fresh boot with no snapshot restore before the workload --
 *       its fingerprint must equal the serve path's (snapshot branching changes nothing)
 */
#define HVX_STACK_NO_MAIN
#include "hvx_stack_verify.c"
#undef main
#include <time.h>
#include "explore/ptedges.h"

#define EXP_MEM_MIB	1024ULL
#define S0_MARKER	"hydo-explore-ready-S0"
#define S1_MARKER	"hydo-explore-ready-S1"
#define DONE_MARKER	"hydo-explore-workload-done"
#define ACKED_PREFIX	"hydo-explore-acked:"
#define FAILED_PREFIX	"hydo-explore-failed:"
#define RUN_WALL_SECS	900	/* safety net only; a run that hits it is reported as TIMEOUT */

struct snap {
	struct hvx_snapshot *s;
	struct savebuf ram, rw;
	size_t ulen;		/* console watermark at the snapshot */
	uint8_t *con;		/* console text up to the watermark (part of every fingerprint) */
	int have;
};

static struct env E;
static const char *g_outdir = ".";

static double now_s(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec / 1e9;
}

/* ---- Milestone 41: coverage feedback (Intel PT of the front door only) ----
 * Enabled by HVX_EXPLORE_PT_ELF=<the image's symbol-enabled hydostack, linked at a unique
 * address>. Each schedule's WORKLOAD VM traces guest user mode inside that program's text
 * range from the moment the schedule starts (S1) until the workload VM is torn down; the
 * trace is decoded to an edge file <outdir>/../cov/<tag>.edges. The recovery VM after a
 * power cut is not traced (it only runs the oracle). Tracing does not change the guest. */
static int g_pt;
static uint64_t g_pt_lo, g_pt_hi, g_pt_mib = 256;

/* ---- Milestone 44: whole-system coverage (hvx/richcov) ----
 * HVX_EXPLORE_PT_RANGES="<lo>-<hi>[,<lo>-<hi>]" (hex) programs up to two IP ranges instead of
 * one ELF's text, and HVX_EXPLORE_PTMULTI="<ptmulti binary> <name>=<elf>[@<bias>] ..." decodes
 * each workload trace into per-program edge files in <outdir>/../cov/<tag>/ (ptmulti's
 * output: <name>.edges, <name>.funcs, summary.txt). Without these variables nothing changes. */
static int g_ptm_nr;
static uint64_t g_ptm_lo[2], g_ptm_hi[2];
static const char *g_ptmulti;

static int ptm_parse_ranges(const char *s)
{
	char *end;
	while (*s && g_ptm_nr < 2) {
		g_ptm_lo[g_ptm_nr] = strtoull(s, &end, 16);
		if (*end != '-') return -1;
		g_ptm_hi[g_ptm_nr] = strtoull(end + 1, &end, 16);
		if (g_ptm_hi[g_ptm_nr] <= g_ptm_lo[g_ptm_nr]) return -1;
		g_ptm_nr++;
		s = (*end == ',') ? end + 1 : end;
	}
	return (*s || !g_ptm_nr) ? -1 : 0;
}

/* Run ptmulti on a saved trace; writes <cov>/<tag>/ and a one-line stats file. */
static void ptm_decode(const char *trace, const char *tag, uint64_t bytes, int truncated)
{
	char cmd[4096], dir[600], spath[600];
	double t0 = now_s();
	int rc;
	FILE *f;

	snprintf(dir, sizeof(dir), "%s/../cov/%s", g_outdir, tag);
	snprintf(spath, sizeof(spath), "%s/../cov/%s.stats", g_outdir, tag);
	mkdir(dir, 0755);
	/* g_ptmulti = "<binary> <name>=<elf> ...": the trace and outdir go right after the binary */
	{
		const char *sp = strchr(g_ptmulti, ' ');
		int bl = sp ? (int)(sp - g_ptmulti) : (int)strlen(g_ptmulti);
		snprintf(cmd, sizeof(cmd), "%.*s %s %s%s > %s/ptmulti.log 2>&1",
			 bl, g_ptmulti, trace, dir, sp ? sp : "", dir);
	}
	rc = system(cmd);
	if ((f = fopen(spath, "w"))) {
		fprintf(f, "trace_bytes=%llu truncated=%d ptmulti_rc=%d decode_secs=%.2f\n",
			(unsigned long long)bytes, truncated, rc, now_s() - t0);
		fclose(f);
	}
	if (!getenv("HVX_EXPLORE_PT_SAVE")) unlink(trace);
}

static void pt_arm(struct boot *b)
{
	struct hvx_pt_config pc;

	if (!g_pt) return;
	memset(&pc, 0, sizeof(pc));
	pc.buf_mib = g_pt_mib;
	pc.flags = HVX_PT_USER | HVX_PT_NORETCOMP;
	pc.lo[0] = g_pt_lo; pc.hi[0] = g_pt_hi;
	if (g_ptm_nr) {
		int i;
		for (i = 0; i < g_ptm_nr; i++) { pc.lo[i] = g_ptm_lo[i]; pc.hi[i] = g_ptm_hi[i]; }
	}
	if (ioctl(b->fd, HVX_SET_PT, &pc) < 0) perror("HVX_SET_PT");
}

/* Read + decode this VM's trace; writes the edge file and a one-line stats file. */
static void pt_collect(struct boot *b, const char *tag)
{
	struct hvx_pt_status ps;
	struct hvx_pt_read rq;
	uint8_t *buf;
	char path[600], spath[600];
	uint64_t errs = 0;
	double t0;
	long n;
	FILE *f;

	if (!g_pt) return;
	memset(&ps, 0, sizeof(ps));
	if (ioctl(b->fd, HVX_GET_PT, &ps) < 0) { perror("HVX_GET_PT"); return; }
	if (ps.saved) {		/* HVX_SET_PT failed (ENOMEM on a busy host): this VM has no trace of
				 * its own and the module returned the last FREED trace, which may be
				 * another process's. Never decode it; say so, the scheduler re-runs. */
		snprintf(spath, sizeof(spath), "%s/../cov/%s.stats", g_outdir, tag);
		if ((f = fopen(spath, "w"))) { fprintf(f, "no_trace=1\n"); fclose(f); }
		return;
	}
	buf = malloc(ps.bytes ? ps.bytes : 1);
	if (!buf) return;
	rq.offset = 0; rq.len = ps.bytes; rq.addr = (uint64_t)(uintptr_t)buf;
	if (ps.bytes && ioctl(b->fd, HVX_READ_PT, &rq) < 0) { perror("HVX_READ_PT"); free(buf); return; }
	snprintf(path, sizeof(path), "%s/../cov/%s.edges", g_outdir, tag);
	snprintf(spath, sizeof(spath), "%s/../cov/%s.stats", g_outdir, tag);
	if (g_ptmulti) {			/* whole-system: save the trace, decode it with ptmulti */
		char rp[600];
		snprintf(rp, sizeof(rp), "%s/../cov/%s.pt", g_outdir, tag);
		if ((f = fopen(rp, "w"))) { fwrite(buf, 1, ps.bytes, f); fclose(f); }
		free(buf);
		ptm_decode(rp, tag, ps.bytes, (ps.bytes && ps.bytes >= ps.buf_bytes) ? 1 : 0);
		return;
	}
	if (getenv("HVX_EXPLORE_PT_SAVE")) {	/* diagnostic: keep the raw trace */
		char rp[600];
		snprintf(rp, sizeof(rp), "%s/../cov/%s.pt", g_outdir, tag);
		if ((f = fopen(rp, "w"))) { fwrite(buf, 1, ps.bytes, f); fclose(f); }
	}
	t0 = now_s();
	n = pte_decode(buf, ps.bytes, path, &errs);
	if ((f = fopen(spath, "w"))) {
		fprintf(f, "trace_bytes=%llu truncated=%d edges=%ld decode_errors=%llu decode_secs=%.2f\n",
			(unsigned long long)ps.bytes, (ps.bytes && ps.bytes >= ps.buf_bytes) ? 1 : 0, n,
			(unsigned long long)errs, now_s() - t0);
		fclose(f);
	}
	free(buf);
}


/* Run until any of the markers appears in the console AFTER offset `from`. Returns the
 * marker index, -1 on a VM error, -2 when the run exceeds RUN_WALL_SECS. */
/* > 0: every run call returns after at most this many deterministic events (HVX_RUNC), so
 * return points do not depend on where earlier calls stopped (rb=, branching). 0: HVX_RUN. */
static uint64_t g_budget;
#define BRANCH_BUDGET 16

static int run_until_any(struct boot *b, const char *const *mk, int n, size_t from)
{
	struct hvx_exit ex;
	double t0 = now_s();
	int i;
	/* a marker may already be there (a branch request whose first stop already reached
	 * the end of the workload): stop where we are, as a single run would have */
	for (i = 0; i < n; i++)
		if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, mk[i]) >= 0)
			return i;
	for (;;) {
		memset(&ex, 0, sizeof(ex));
		if (g_budget) {
			struct hvx_run_ctl c;
			memset(&c, 0, sizeof(c));
			c.max_insns = g_budget;
			if (ioctl(b->fd, HVX_RUNC, &c) < 0) {
				if (errno == EINTR) continue;
				fprintf(stderr, "hvx_explore: HVX_RUNC: %s\n", strerror(errno));
				return -1;
			}
			ex.reason = c.reason == HVX_EXIT_STEP ? HVX_EXIT_THROTTLE : c.reason;
		} else if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR) continue;
			fprintf(stderr, "hvx_explore: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);
		for (i = 0; i < n; i++)
			if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, mk[i]) >= 0)
				return i;
		if (ex.reason != HVX_EXIT_THROTTLE) {
			fprintf(stderr, "hvx_explore: guest stopped: reason=%u (%s)\n", ex.reason, exit_reason_name(ex.reason));
			return -1;
		}
		if (now_s() - t0 > RUN_WALL_SECS)
			return -2;
	}
}

static int put_input(struct boot *b, const char *line)
{
	size_t len = strlen(line), off = 0;
	while (off < len) {
		struct hvx_input in;
		size_t c = len - off > HVX_INPUT_MAX ? HVX_INPUT_MAX : len - off;
		memset(&in, 0, sizeof(in));
		in.len = (uint32_t)c;
		memcpy(in.buf, line + off, c);
		if (ioctl(b->fd, HVX_PUT_INPUT, &in) < 0) { perror("HVX_PUT_INPUT"); return -1; }
		off += c;
	}
	return 0;
}

static int set_fault(struct boot *b, uint64_t seed, uint32_t classes, uint32_t prob)
{
	struct hvx_fault f;
	memset(&f, 0, sizeof(f));
	f.seed = seed; f.classes = classes; f.probability = prob;
	if (ioctl(b->fd, HVX_SET_FAULT, &f) < 0) { perror("HVX_SET_FAULT"); return -1; }
	return 0;	/* also empties the volatile write cache left by an earlier run */
}

static int snap_take(struct boot *b, struct snap *sn, const char *tag)
{
	memset(sn, 0, sizeof(*sn));
	sn->s = calloc(1, sizeof(*sn->s));
	if (!sn->s) return -1;
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, sn->s) < 0) { perror("HVX_GET_SNAPSHOT"); return -1; }
	if (savebuf_open(&sn->ram, E.mem_size, tag) || savebuf_open(&sn->rw, E.p2_len, tag)) return -1;
	if (savebuf_store(&sn->ram, b->mem, E.mem_size) || savebuf_store(&sn->rw, b->disk_mm + E.p2_off, E.p2_len)) return -1;
	sn->ulen = b->ulen;
	sn->con = malloc(b->ulen + 1);
	if (!sn->con) return -1;
	memcpy(sn->con, b->ubuf, b->ulen);
	sn->have = 1;
	return 0;
}

/* A fresh VM with the base disk attached (no kernel loaded: the snapshot supplies the state). */
static int vm_fresh(struct boot *b)
{
	struct boot nb;
	/* boot_setup loads a kernel too; it is harmless (RAM is overwritten by the restore) and
	 * keeps one code path for VM creation, PMU/flag setup and the disk window. */
	if (boot_setup(&nb, E.mem_size, E.image, E.image_len, E.initrd, E.initrd_len,
		       E.base, E.disk_len, "console=ttyS0", 0, 0, 0) != 0) {
		boot_teardown(&nb);
		return -1;
	}
	*b = nb;
	return 0;
}

/* Restore a snapshot into VM b; `rw` (if not NULL) replaces the snapshot's data disk. */
static int snap_restore(struct boot *b, struct snap *sn, struct savebuf *rw)
{
	if (ioctl(b->fd, HVX_SET_SNAPSHOT, sn->s) < 0) { perror("HVX_SET_SNAPSHOT"); return -1; }
	if (savebuf_load(&sn->ram, b->mem, E.mem_size)) return -1;
	if (savebuf_load(rw ? rw : &sn->rw, b->disk_mm + E.p2_off, E.p2_len)) return -1;
	memcpy(b->ubuf, sn->con, sn->ulen);	/* console as it was at the snapshot */
	b->ulen = sn->ulen;
	console_rebase(b);			/* a fresh VM: drained = its (empty) console */
	return 0;
}

/* Last HYDO-ORACLE line after `from`. */
static void last_verdict(const struct boot *b, size_t from, char *out, size_t n)
{
	long at = -1, i;
	for (i = (long)from; i + 12 <= (long)b->ulen; i++)
		if (memcmp(b->ubuf + i, "HYDO-ORACLE:", 12) == 0) at = i;
	out[0] = '\0';
	if (at < 0) { snprintf(out, n, "ERROR no verdict"); return; }
	for (i = 0; at + i < (long)b->ulen && b->ubuf[at + i] != '\n' && b->ubuf[at + i] != '\r' && (size_t)i + 1 < n; i++)
		out[i] = (char)b->ubuf[at + i];
	out[i] = '\0';
}

/* append the rest of the console line starting with `prefix` (after `from`) to out */
static size_t append_line(const struct boot *b, size_t from, const char *prefix, char *out, size_t o, size_t n)
{
	long at = b->ulen > from ? mem_find(b->ubuf + from, b->ulen - from, prefix) : -1;
	if (at >= 0) {
		const uint8_t *p = b->ubuf + from + at + strlen(prefix);
		while (p < b->ubuf + b->ulen && *p != '\n' && *p != '\r' && o + 2 < n)
			out[o++] = (char)*p++;
	}
	return o;
}

/* "hydo-explore-acked: 0 1 2 ..." and "hydo-explore-failed: 7 ..." after `from`
 * -> "oracle 0 1 2 ... failed 7 ...\n" (the ids after "failed" are the orders the client
 * was told definitely failed, Milestone 39) */
static void oracle_cmd(const struct boot *b, size_t from, char *out, size_t n)
{
	size_t o = (size_t)snprintf(out, n, "oracle");
	o = append_line(b, from, ACKED_PREFIX, out, o, n);
	/* only images that report definite failures get the extra field, so a pre-M39 image
	 * receives exactly the command it always did (its old findings reproduce bit-for-bit) */
	if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, FAILED_PREFIX) >= 0 && o + 8 < n) {
		memcpy(out + o, " failed", 7); o += 7;
		o = append_line(b, from, FAILED_PREFIX, out, o, n);
	}
	out[o++] = '\n'; out[o] = '\0';
}

static void save_console(const struct boot *b, size_t from, const char *tag, const char *suffix)
{
	char path[512];
	FILE *f;
	snprintf(path, sizeof(path), "%s/%s%s.log", g_outdir, tag, suffix);
	if (!(f = fopen(path, "a"))) return;
	if (b->ulen > from) fwrite(b->ubuf + from, 1, b->ulen - from, f);
	fclose(f);
}

struct sched {
	char host[1024];
	char guest[512];
	uint32_t classes, prob;
	uint64_t dseed;
	int cut;
	int tj_on;
	struct hvx_tjitter tj;	/* tick-timing jitter for the workload (tj / tjx tokens) */
	uint64_t rb;		/* rb=<n>: run budget (see g_budget) */
};

/* tj=<hex seed> tjpm=<per mille> tjmax=<branches> [tjlo=<k> tjhi=<k>]  or  tjx=<k:d,...> */
static int parse_tj(const char *host, struct sched *sc)
{
	const char *p;

	if ((p = strstr(host, "tjx="))) {
		char *end;
		p += 4;
		while (*p && *p != ' ' && sc->tj.n_explicit < HVX_TJ_MAX_EXPLICIT) {
			sc->tj.explicit_[sc->tj.n_explicit].tick = strtoull(p, &end, 10);
			if (*end != ':') return -1;
			sc->tj.explicit_[sc->tj.n_explicit].delay = strtoull(end + 1, &end, 10);
			sc->tj.n_explicit++;
			p = (*end == ',') ? end + 1 : end;
		}
		if (*p && *p != ' ') return -1;		/* more than HVX_TJ_MAX_EXPLICIT entries */
		sc->tj_on = sc->tj.n_explicit > 0;
		return 0;
	}
	if (!(p = strstr(host, "tj="))) return 0;
	sc->tj.seed = strtoull(p + 3, NULL, 16);
	sc->tj.permille = (p = strstr(host, "tjpm=")) ? (uint32_t)strtoul(p + 5, NULL, 10) : 10;
	sc->tj.max_delay = (p = strstr(host, "tjmax=")) ? strtoull(p + 6, NULL, 10) : 500000;
	if ((p = strstr(host, "tjlo="))) sc->tj.tick_lo = strtoull(p + 5, NULL, 10);
	if ((p = strstr(host, "tjhi="))) sc->tj.tick_hi = strtoull(p + 5, NULL, 10);
	sc->tj_on = sc->tj.permille && sc->tj.max_delay;
	return 0;
}

static int set_tj(struct boot *b, const struct hvx_tjitter *tj)
{
	static struct hvx_tjitter off;
	if (ioctl(b->fd, HVX_SET_TJITTER, tj ? tj : &off) < 0) { perror("HVX_SET_TJITTER"); return -1; }
	return 0;
}

static int parse_sched(const char *s, struct sched *sc)
{
	const char *bar = strchr(s, '|');
	char disk[64] = "none";
	const char *p;
	memset(sc, 0, sizeof(*sc));
	if (!bar) return -1;
	snprintf(sc->host, sizeof(sc->host), "%.*s", (int)(bar - s), s);
	p = bar + 1;
	while (*p == ' ') p++;
	snprintf(sc->guest, sizeof(sc->guest), "%s", p);
	{	/* strip a trailing newline */
		size_t l = strlen(sc->guest);
		while (l && (sc->guest[l - 1] == '\n' || sc->guest[l - 1] == '\r' || sc->guest[l - 1] == ' ')) sc->guest[--l] = '\0';
	}
	if ((p = strstr(sc->host, "disk="))) sscanf(p + 5, "%63s", disk);
	if ((p = strstr(sc->host, "dseed="))) sc->dseed = strtoull(p + 6, NULL, 16);
	if (!strcmp(disk, "none")) { sc->classes = 0; }
	else if (!strncmp(disk, "ioerr:", 6)) { sc->classes = HVX_FAULT_ERROR; sc->prob = (uint32_t)atoi(disk + 6); }
	else if (!strcmp(disk, "cut")) { sc->classes = HVX_FAULT_DROP_UNFLUSHED; sc->prob = 100; }
	else if (!strncmp(disk, "lying:", 6)) { sc->classes = HVX_FAULT_DROP; sc->prob = (uint32_t)atoi(disk + 6); }
	else return -1;
	if (parse_tj(sc->host, sc)) return -1;
	if ((p = strstr(sc->host, "rb="))) sc->rb = strtoull(p + 3, NULL, 10);
	sc->cut = strstr(sc->guest, " C=") != NULL;
	return 0;
}

/* Run one schedule in VM b, which is positioned at S1 (restored or reached by booting).
 * Prints the RESULT line. Consumes b (tears it down). */
/* RESULT line for a schedule that failed before its workload started; consumes b. */
static void sched_fail(struct boot *b, const char *tag, size_t from, double t0, const char *tjinfo,
		       const char *verdict)
{
	uint64_t zero = 0;
	if (b->fd >= 0) save_console(b, from, tag, "");
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f%s verdict=%s\n", tag,
	       (unsigned long long)fnv(0, &zero, sizeof(zero)), 0ULL, now_s() - t0, tjinfo, verdict);
	fflush(stdout);
	boot_teardown(b);
}

/* The rest of a schedule once its guest command has been typed: workload to its end, then
 * the check (same VM) or the power-cut recovery (fresh VM from S0). Prints the RESULT line
 * and consumes b. `from` is the console offset where the workload started. */
static void finish_sched(struct boot *b, const char *tag, const struct sched *sc, struct snap *s0,
			 size_t from, double t0, const char *tjinfo_in)
{
	static const char *const wk[] = { DONE_MARKER, FINAL_MARKER };
	static const char *const fin[] = { FINAL_MARKER };
	char verdict[256] = "", orc[512], tjinfo[96];
	uint64_t wfp = 0, ffp = 0;
	int r;
	const char *phase = "workload";

	snprintf(tjinfo, sizeof(tjinfo), "%s", tjinfo_in);
	r = run_until_any(b, wk, 2, from);
	if (r < 0) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT workload" : "ERROR vm workload"); goto out; }
	wfp = fingerprint(b);
	if (sc->tj_on) {		/* the jitter covers the workload only, like the disk fault */
		static struct hvx_tjitter got;
		size_t l = strlen(tjinfo);
		memset(&got, 0, sizeof(got));
		ioctl(b->fd, HVX_GET_TJITTER, &got);
		snprintf(tjinfo + l, sizeof(tjinfo) - l, "-%llu tjn=%llu",
			 (unsigned long long)got.last_tick, (unsigned long long)got.applied);
		set_tj(b, NULL);
	}
	if (r == 1) {			/* the guest gave a verdict during the workload (e.g. setup error) */
		last_verdict(b, from, verdict, sizeof(verdict));
		ffp = wfp;
		goto out;
	}
	oracle_cmd(b, from, orc, sizeof(orc));
	save_console(b, from, tag, "");
	if (!sc->cut) {			/* same boot: faults off, heal, recover, check */
		size_t f2 = b->ulen;
		phase = "check";
		if (set_fault(b, 0, 0, 0) || put_input(b, "go\n")) { snprintf(verdict, sizeof(verdict), "ERROR harness go"); goto out; }
		r = run_until_any(b, fin, 1, f2);
		if (r < 0) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT check" : "ERROR vm check"); goto out_save; }
		ffp = fingerprint(b);
		last_verdict(b, f2, verdict, sizeof(verdict));
		save_console(b, f2, tag, "");
		pt_collect(b, tag);
		goto out_done;
	}
	{	/* power cut: only the flushed disk bytes survive; recover in a fresh VM from S0 */
		struct savebuf durable;
		struct boot ob;
		phase = "recovery";
		if (savebuf_open(&durable, E.p2_len, "durable") || savebuf_store(&durable, b->disk_mm + E.p2_off, E.p2_len)) {
			snprintf(verdict, sizeof(verdict), "ERROR harness durable"); goto out;
		}
		if (getenv("HVX_EXPLORE_DURABLE_OUT")) {	/* diagnostic: keep the post-cut data disk */
			FILE *df = fopen(getenv("HVX_EXPLORE_DURABLE_OUT"), "w");
			if (df) { fwrite(b->disk_mm + E.p2_off, 1, E.p2_len, df); fclose(df); }
		}
		pt_collect(b, tag);
		boot_teardown(b);
		if (vm_fresh(&ob) || snap_restore(&ob, s0, &durable)) {
			savebuf_close(&durable);
			snprintf(verdict, sizeof(verdict), "ERROR harness recovery vm"); goto out;
		}
		savebuf_close(&durable);
		*b = ob;
		from = b->ulen;
		if (set_fault(b, 0, 0, 0) || put_input(b, orc)) { snprintf(verdict, sizeof(verdict), "ERROR harness oracle cmd"); goto out; }
		r = run_until_any(b, fin, 1, from);
		if (r < 0) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT recovery" : "ERROR vm recovery"); goto out_save; }
		ffp = fingerprint(b);
		last_verdict(b, from, verdict, sizeof(verdict));
		save_console(b, from, tag, ".recovery");
		goto out_done;
	}
out_save:
	save_console(b, from, tag, "");
	goto out_done;
out:
	if (b->fd >= 0) save_console(b, from, tag, "");
out_done:
	(void)phase;
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f%s verdict=%s\n", tag,
	       (unsigned long long)fnv(wfp, &ffp, sizeof(ffp)), (unsigned long long)wfp, now_s() - t0,
	       tjinfo, verdict);
	fflush(stdout);
	boot_teardown(b);
}

static void run_sched(struct boot *b, const char *tag, const struct sched *sc, struct snap *s0)
{
	char line[600], verdict[256] = "", tjinfo[96] = "";
	size_t from = b->ulen;
	double t0 = now_s();

	g_budget = sc->rb;
	if (set_fault(b, sc->dseed, sc->classes, sc->prob)) { snprintf(verdict, sizeof(verdict), "ERROR harness set_fault"); goto fail; }
	if (sc->tj_on) {
		struct hvx_pmu pm;
		memset(&pm, 0, sizeof(pm));
		if (set_tj(b, &sc->tj) || ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) {
			snprintf(verdict, sizeof(verdict), "ERROR harness tick jitter (needs the PMU clock)"); goto fail;
		}
		snprintf(tjinfo, sizeof(tjinfo), " tjk=%llu", (unsigned long long)(pm.next / pm.period));
	}
	pt_arm(b);
	snprintf(line, sizeof(line), "%s\n", sc->guest);
	if (put_input(b, line)) { snprintf(verdict, sizeof(verdict), "ERROR harness put_input"); goto fail; }
	finish_sched(b, tag, sc, s0, from, t0, tjinfo);
	g_budget = 0;
	return;
fail:
	sched_fail(b, tag, from, t0, tjinfo, verdict);
	g_budget = 0;
}

static void snap_free(struct snap *sn)
{
	if (!sn->have) return;
	savebuf_close(&sn->ram);
	savebuf_close(&sn->rw);
	free(sn->s);
	free(sn->con);
	sn->have = 0;
}

/* BRANCH (see the header): one base run to <mark>, a snapshot there, then n timing variants
 * from the snapshot. VM b is positioned at S1 and is consumed. */
static void run_branch(struct boot *b, const char *tag, const struct sched *sc, struct snap *s0,
		       const char *mark, int nvar, uint32_t pm_, uint64_t mx)
{
	const char *const mk[] = { mark, DONE_MARKER, FINAL_MARKER };
	char line[600], sub[96], verdict[256] = "";
	struct snap s2;
	struct hvx_pmu pm;
	size_t from = b->ulen;
	double t0 = now_s();
	uint64_t lo;
	int r, v;

	memset(&s2, 0, sizeof(s2));
	snprintf(sub, sizeof(sub), "%s-base", tag);
	g_budget = BRANCH_BUDGET;
	if (sc->cut || sc->tj_on || sc->rb) {
		snprintf(verdict, sizeof(verdict), "ERROR branch: not for power-cut, timed or rb= schedules");
		goto fail;
	}
	if (set_fault(b, sc->dseed, sc->classes, sc->prob)) { snprintf(verdict, sizeof(verdict), "ERROR harness set_fault"); goto fail; }
	snprintf(line, sizeof(line), "%s\n", sc->guest);
	if (put_input(b, line)) { snprintf(verdict, sizeof(verdict), "ERROR harness put_input"); goto fail; }
	r = run_until_any(b, mk, 3, from);
	if (r != 0) {			/* no branch point in this run: just finish it */
		printf("BRANCHPOINT %s none\n", tag);
		fflush(stdout);
		finish_sched(b, sub, sc, s0, from, t0, "");
		goto end;
	}
	memset(&pm, 0, sizeof(pm));
	if (ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) { snprintf(verdict, sizeof(verdict), "ERROR branch needs the PMU clock"); goto fail; }
	lo = pm.next / pm.period + 1;	/* the first tick whose target is not set yet */
	if (snap_take(b, &s2, "s2")) { snprintf(verdict, sizeof(verdict), "ERROR branch snapshot"); goto fail; }
	printf("BRANCHPOINT %s tjlo=%llu\n", tag, (unsigned long long)lo);
	fflush(stdout);
	finish_sched(b, sub, sc, s0, from, t0, "");
	for (v = 1; v <= nvar; v++) {
		struct boot vb;
		struct sched sv = *sc;
		char tji[64];
		double tv = now_s();

		memset(&vb, 0, sizeof(vb));
		vb.fd = -1;
		snprintf(sub, sizeof(sub), "%s-v%d", tag, v);
		memset(&sv.tj, 0, sizeof(sv.tj));
		sv.tj_on = 1;
		sv.tj.seed = (uint64_t)v;
		sv.tj.permille = pm_;
		sv.tj.max_delay = mx;
		sv.tj.tick_lo = lo;
		if (vm_fresh(&vb) || snap_restore(&vb, &s2, NULL) || set_tj(&vb, &sv.tj)) {
			sched_fail(&vb, sub, from, tv, "", "ERROR branch restore");
			continue;
		}
		snprintf(tji, sizeof(tji), " tjk=%llu", (unsigned long long)(lo - 1));
		finish_sched(&vb, sub, &sv, s0, from, tv, tji);
	}
	goto end;
fail:
	sched_fail(b, sub, from, t0, "", verdict);
end:
	snap_free(&s2);
	g_budget = 0;
	printf("BRANCHEND %s\n", tag);
	fflush(stdout);
}

/* Boot to S0, snapshot, provision to S1. Leaves b at S1 and fills s0 (and s1 if wanted). */
static int boot_to_s1(struct boot *b, struct snap *s0, struct snap *s1)
{
	static const char *const m0[] = { S0_MARKER, FINAL_MARKER };
	static const char *const m1[] = { S1_MARKER, FINAL_MARKER };
	char cmd[512];
	snprintf(cmd, sizeof(cmd), "%s hydo_jit=%s hydo_phase=serve", E.cmdbase, g_jit);
	/* Boot with the flush-aware power-loss class armed at probability 0: nothing is dropped,
	 * but the disk advertises a volatile write cache (VIRTIO_BLK_F_FLUSH), so the guest
	 * negotiates write-back caching and issues FLUSH on fsync -- like real hardware. A
	 * schedule's power cut later raises the probability; without the cache bit negotiated
	 * at boot the guest would never flush and every write since S1 would look lost. */
	if (boot_setup(b, E.mem_size, E.image, E.image_len, E.initrd, E.initrd_len,
		       E.base, E.disk_len, cmd, 0, HVX_FAULT_DROP_UNFLUSHED, 0) != 0) return -1;
	if (run_until_any(b, m0, 2, 0) != 0) { fprintf(stderr, "hvx_explore: no S0 marker\n"); console_dump(b); return -1; }
	if (snap_take(b, s0, "s0")) return -1;
	fprintf(stderr, "hvx_explore: S0 at %zu console bytes, state %016llx\n", b->ulen, (unsigned long long)fingerprint(b));
	if (put_input(b, "provision\n")) return -1;
	if (run_until_any(b, m1, 2, s0->ulen) != 0) { fprintf(stderr, "hvx_explore: no S1 marker\n"); console_dump(b); return -1; }
	fprintf(stderr, "hvx_explore: S1 at %zu console bytes, state %016llx\n", b->ulen, (unsigned long long)fingerprint(b));
	if (s1) {
		if (snap_take(b, s1, "s1")) return -1;
	}
	return 0;
}

static uint8_t *map_file(const char *path, size_t *len)
{
	int fd = open(path, O_RDONLY);
	struct stat st;
	void *p;
	if (fd < 0 || fstat(fd, &st) < 0) { perror(path); return NULL; }
	p = mmap(NULL, (size_t)st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
	close(fd);
	if (p == MAP_FAILED) { perror("mmap"); return NULL; }
	*len = (size_t)st.st_size;
	return p;
}

int main(int argc, char **argv)
{
	const char *mode;
	struct boot b, geom;
	struct snap s0, s1;
	size_t dlen = 0;
	double t0 = now_s();

	if (argc < 6) {
		fprintf(stderr, "usage: %s serve|one <bzImage> <initramfs> <explore-disk.img> <outdir> [\"<schedule>\"]\n", argv[0]);
		return 2;
	}
	mode = argv[1];
	g_outdir = argv[5];
	mkdir(g_outdir, 0755);
	memset(&E, 0, sizeof(E));
	E.mem_size = EXP_MEM_MIB << 20;
	E.cmdbase = ROOTFS_CMDLINE;
	if (!(E.image = map_file(argv[2], &E.image_len)) || !(E.initrd = map_file(argv[3], &E.initrd_len)) ||
	    !(E.base = map_file(argv[4], &dlen)))
		return 1;
	E.disk_len = dlen;
	memset(&geom, 0, sizeof(geom));
	geom.disk_len = dlen;
	if (mbr_partitions(E.base, dlen, &geom) < 0) return 1;
	E.p1_off = geom.p1_off; E.p1_len = geom.p1_len; E.p2_off = geom.p2_off; E.p2_len = geom.p2_len;
	setvbuf(stdout, NULL, _IOLBF, 0);
	if (getenv("HVX_EXPLORE_CMDLINE_EXTRA")) {	/* e.g. "norandmaps" for the richcov images */
		static char cmd[1024];
		snprintf(cmd, sizeof(cmd), "%s %s", E.cmdbase, getenv("HVX_EXPLORE_CMDLINE_EXTRA"));
		E.cmdbase = cmd;
	}
	if (getenv("HVX_EXPLORE_PT_RANGES")) {
		char cdir[600];
		if (ptm_parse_ranges(getenv("HVX_EXPLORE_PT_RANGES"))) {
			fprintf(stderr, "hvx_explore: bad HVX_EXPLORE_PT_RANGES\n");
			return 1;
		}
		g_ptmulti = getenv("HVX_EXPLORE_PTMULTI");
		if (getenv("HVX_EXPLORE_PT_MIB")) g_pt_mib = strtoull(getenv("HVX_EXPLORE_PT_MIB"), NULL, 0);
		setenv("HVX_PT_TRACE_VM", "1", 1);
		snprintf(cdir, sizeof(cdir), "%s/../cov", g_outdir);
		mkdir(cdir, 0755);
		g_pt = 1;
		fprintf(stderr, "hvx_explore: whole-system coverage on: %d range(s), %llu MiB buffer, decoder %s\n",
			g_ptm_nr, (unsigned long long)g_pt_mib, g_ptmulti ? g_ptmulti : "(none: edges via ptedges.h)");
	} else if (getenv("HVX_EXPLORE_PT_ELF")) {
		char cdir[600];
		if (pte_load_elf(getenv("HVX_EXPLORE_PT_ELF"), &g_pt_lo, &g_pt_hi)) {
			fprintf(stderr, "hvx_explore: cannot load %s\n", getenv("HVX_EXPLORE_PT_ELF"));
			return 1;
		}
		if (getenv("HVX_EXPLORE_PT_MIB")) g_pt_mib = strtoull(getenv("HVX_EXPLORE_PT_MIB"), NULL, 0);
		setenv("HVX_PT_TRACE_VM", "1", 1);
		snprintf(cdir, sizeof(cdir), "%s/../cov", g_outdir);
		mkdir(cdir, 0755);
		g_pt = 1;
		fprintf(stderr, "hvx_explore: coverage on: text [%#llx, %#llx), %llu MiB buffer\n",
			(unsigned long long)g_pt_lo, (unsigned long long)g_pt_hi, (unsigned long long)g_pt_mib);
	}

	if (!strcmp(mode, "one")) {
		struct sched sc;
		if (argc < 7 || parse_sched(argv[6], &sc)) { fprintf(stderr, "hvx_explore: bad schedule\n"); return 2; }
		if (boot_to_s1(&b, &s0, NULL)) return 1;
		fprintf(stderr, "hvx_explore: provisioned in %.1f s (fresh boot, no snapshot restore)\n", now_s() - t0);
		run_sched(&b, "one", &sc, &s0);
		return 0;
	}
	if (strcmp(mode, "serve")) { fprintf(stderr, "hvx_explore: unknown mode %s\n", mode); return 2; }
	if (boot_to_s1(&b, &s0, &s1)) return 1;
	boot_teardown(&b);
	printf("READY boot_secs=%.1f\n", now_s() - t0);
	{
		char in[4096];
		while (fgets(in, sizeof(in), stdin)) {
			char tag[64];
			int off = 0;
			struct sched sc;
			if (!strncmp(in, "QUIT", 4)) break;
			if (!strncmp(in, "BRANCH ", 7)) {
				char mark[128];
				int nv = 0;
				unsigned bpm = 0;
				unsigned long long bmx = 0;
				if (sscanf(in, "BRANCH %63s %d %u %llu %127s %n", tag, &nv, &bpm, &bmx, mark, &off) != 5 ||
				    off == 0 || parse_sched(in + off, &sc)) {
					printf("BRANCHEND ? bad request\n");
					continue;
				}
				if (vm_fresh(&b) || snap_restore(&b, &s1, NULL)) {
					printf("BRANCHEND %s restore failed\n", tag);
					boot_teardown(&b);
					continue;
				}
				run_branch(&b, tag, &sc, &s0, mark, nv, bpm, bmx);
				continue;
			}
			if (sscanf(in, "RUN %63s %n", tag, &off) != 1 || off == 0 || parse_sched(in + off, &sc)) {
				printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n");
				continue;
			}
			if (vm_fresh(&b) || snap_restore(&b, &s1, NULL)) {
				printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR harness restore\n", tag);
				boot_teardown(&b);
				continue;
			}
			run_sched(&b, tag, &sc, &s0);
		}
	}
	return 0;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_explore - the run engine of the automatic fault explorer (milestone 38).
 *
 * Boots the four-service stack guest built with STACK_OUT=explore (build-stack.sh +
 * hydo/explore/stack-explore.sh) in its command-driven "serve" mode and snapshots it twice:
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
 * Finer preemption during the workload (milestone 49; both inert when absent):
 *   pp=<branches> [pptsc=<tsc>]   PMU tick period for the workload (default 2^20 branches);
 *                 each tick still advances the virtual TSC by the default tick (2 ms) unless
 *                 pptsc= says otherwise, so a shorter period makes guest time run faster per
 *                 unit of work and the timer -- and a thread switch -- come more often
 *   xt=<hex> xtr=<n> xtpm=<n> [xtlo=<j> xthi=<j>] [xtsc=<tsc>]  |  xtx=<b,b,...>
 *                 extra ticks between the grid ticks (HVX_SET_XTICK): n slots per period,
 *                 each used with chance xtpm/1000, at seeded points; or the listed absolute
 *                 guest branch counts. RESULT then also reports xtn=<extra ticks taken>.
 *
 * Branching (mid-run snapshots): "BRANCH <tag> <n> <marker> <schedule>" starts the schedule
 * from S1 without timing jitter and with rb=16, stops at the first return after <marker>
 * (no spaces) appears in the workload's console -- or, for "@<N>", at the first return
 * N clock ticks into the workload, or for "@held", at the first return where the disk's
 * volatile write cache holds writes (the hard case for a snapshot) -- snapshots there
 * (S2), finishes that run
 * ("<tag>-base"), then runs n timing variants from S2 ("<tag>-v1".."-vn"), each in a fresh
 * VM with seeded jitter (seed v) on the ticks after S2 only. Variant v is exactly the plain
 * schedule  <host> tj=<v> tjpm=<p> tjmax=<m> tjlo=<k> rb=16 | <guest>, with k from the
 * "BRANCHPOINT <tag> tjlo=<k>" line, so a finding reproduces and shrinks like any other.
 * Power-cut schedules work too: every snapshot also carries the disk's volatile write
 * cache (HVX_GET_UNSYNC / HVX_SET_UNSYNC), i.e. the writes not flushed yet.
 * Output: BRANCHPOINT, then n+1 RESULT lines, then "BRANCHEND <tag>". A negative n runs |n|
 * variants and does not finish the base run (no "-base" RESULT): for a caller that already
 * ran the same schedule with rb=16, whose result the base run would only repeat.
 * (guest tokens: see hydo/explore/stack-explore.sh). The schedule generator, the
 * reproduce/shrink logic and the report live in hydo/explore/explore.py.
 *
 * Modes (as root, hvx loaded, through ~/bin/hvx-run):
 *   hvx_explore [--ram-mib N] [--vcpus N] [--cpu v2|v3] serve <bzImage> <initramfs> <explore-disk.img> <outdir>
 *       reads "RUN <tag> <schedule>" lines on stdin, answers "RESULT <tag> ..." lines
 *   hvx_explore [--ram-mib N] [--vcpus N] [--cpu v2|v3] one <bzImage> <initramfs> <explore-disk.img> <outdir> "<schedule>"
 *       one schedule from a fresh boot with no snapshot restore before the workload --
 *       its fingerprint must equal the serve path's (snapshot branching changes nothing)
 * Several vCPUs (SMP Stage 3, hydo/explore/TURNS.md): "--vcpus N" (or env HVX_EXPLORE_VCPUS),
 * default 1. With N > 1 every VM is created with N vCPUs and HVX_VM_SMP_SCHED (the vCPUs run
 * in exact turns of guest branches inside the kernel), every snapshot (S0, S1, branch
 * points) and restore covers every vCPU and the scheduler state, and every fingerprint
 * also hashes vCPUs 1..N-1 and the scheduler state. With N = 1 nothing changes. Linux starts
 * vCPUs 1..N-1 itself with INIT/SIPI (HVX_VM_SMP_BOOT, SMP Stage 2; the MP table lists N
 * CPUs; 4 consecutive PAUSEs end a turn, HVX_PAUSE_K overrides); HVX_VCPU_PARK=1 parks them
 * instead (halted, IF=0, never run). Host tokens for the turns, applied during the workload only:
 *   q=<branches>  every turn this many guest conditional branches (the module's default is 2^20)
 *   qs=<hex>      seeded turn lengths in [1, q] (hvx.c: 1 + splitmix64(seed + (t+1)*GOLDEN) % q,
 *                 t = the turn number), with q= or the VM's current turn length
 *   qb=<B>:<s>:<permille>  seeded bursts of short turns (hv/hvx/FASTTURN.md, HVX_SET_TURNCTL): the
 *                 branch clock is cut into windows of max(q, B) branches, a window holds a
 *                 burst of B branches with chance permille/1000, and inside a burst every
 *                 turn is s branches; seed qbs=<hex> (default 1)
 *   qm=<n>        landing margin of turn ends in branches (default 256; see FASTTURN.md)
 * RESULT then also reports qn=<turns taken during the workload> (and qbn=<turns that started
 * in a burst> with qb=). On a 1-vCPU VM these tokens
 * give an ERROR verdict. The module has no explicit per-turn list (HVX_SET_SCHED takes one
 * length and one seed), so there is no explicit form; see TURNS.md.
 * Guest RAM: 1024 MiB unless "--ram-mib N" (or env HVX_EXPLORE_RAM_MIB) says otherwise,
 * N in [256, 3072]; explore.py passes the manifest's guest.ram_mib. The size is not in the
 * schedule: a hand reproduction must pass the same --ram-mib (report.md's repro line does).
 *
 * Lock-step control (milestone 118; docs/design/guest-control.md "The channel", Phase 1; the
 * typed client is hydo-engine's `lockstep` module). Opt-in per request: RUN, BRANCH and `one`
 * without --control never execute any of it. Requests (serve mode, stdin):
 *   CTL <tag> <schedule>      like RUN (restore S1, arm the host part, type the guest part --
 *                             nothing when it is empty), but the engine stops and waits for the host
 *   RESUME <tag> <snapshot>   a fresh VM from a snapshot taken by SNAP in an earlier controlled run;
 *                             the run continues exactly where that run was (same schedule, console,
 *                             queued input, stop and event numbers)
 *   hvx_explore --control one ... "<schedule>"   the same as CTL from a fresh boot (stdin/stdout)
 * At every STOP the engine has already printed, in this order, one EVENT per complete console line
 * written since the last stop (the text escaped: \\ \n \r \t \xHH; the line's \n not included),
 * one SENT per queued input typed at this stop, and DROP for queued input a phase change discards:
 *   EVENT <tag> at=<icount> n=<event number> <escaped line>
 *   SENT <tag> id=<id> at=<icount>[ retype=1]
 *   DROP <tag> id=<id>
 *   STOP <tag> at=<icount> n=<stop number> phase=workload|check|recovery why=<why>
 * then it reads host lines until CONT or END; every other host line gets exactly one answer:
 *   SEND at=<icount> <escaped bytes>   type the bytes into the serial input at the first stop with
 *                                      icount >= at  ->  "OK send id=<id> sent=<icount>" (typed now,
 *                                      at <= this stop) or "OK send id=<id> queued=<at>"
 *   ACK <id>                           the guest has consumed send <id> (the agent's reply proves it)
 *                                      ->  "OK ack <id>" / "ERR ack <id> <why>"
 *   SNAP <name> [retype]               snapshot here (workload phase; not with tj/xt/pp/q/cf tokens),
 *                                      kept in this process under <name> (a second SNAP with the same
 *                                      name replaces it; at most 8 names)  ->  "OK snap <name> at=<icount>
 *                                      retype=<ids|->"; refused ("ERR snap <name> unacked=<ids>") while a
 *                                      typed send is not acknowledged, unless `retype` is given: input
 *                                      still in the UART receive queue is in no snapshot (M116), so
 *                                      RESUME types every such send again right after the restore.
 *                                      Queued sends are part of the snapshot and stay queued.
 *   CONT                               run on to the next stop
 *   END [<verdict>]                    end the run at this stop: fingerprint now, RESULT with <verdict>
 *                                      (default: the last HYDO-ORACLE line, as for a marker)
 * EOF on stdin ends the run with "ERROR control channel closed".
 * Stops (`why`): `start` before the first run call of each phase (the engine's own input -- the
 * guest line, "go", the oracle command -- is already typed), `resume` after a RESUME restore,
 * `cadence` at every return of a run call that was not cut short for a pending send (so the
 * schedule's rb=<n> is the cadence: a stop after at most n events; without rb= the module's own
 * returns), `send` when a pending send was typed at a capped return, `marker` where the shell
 * init's workload/check marker was seen (the engine then types its own next input as in RUN),
 * `exit` before a run ends on a guest stop. Exact delivery: while a send is pending each run call
 * gets at most max(1, (at - icount) / 2048) events (an event adds 1 or, a PMU tick, 2048 to
 * icount), so no call runs past `at`; capped returns that type nothing are not reported. With
 * the PMU clock these points are branch-exact; with several vCPUs they are the first clean turn
 * boundary at or after `at`. Each phase is its own VM clock: the recovery VM starts from S0's icount.
 * RESULT gets three more facts before verdict=: stops=<n> typed=<sends typed> inh=<hex16> (FNV-1a
 * over every typed send's icount, length and bytes, in typing order). Example (serve; `<` host,
 * `>` engine, abridged):
 *   < CTL a disk=none | run K=redis@5+2
 *   > STOP a at=949127 n=1 phase=workload why=start
 *   < SEND at=969127 hydo-agent-ping 1
 *   > OK send id=1 queued=969127
 *   < CONT
 *   > EVENT a at=967310 n=1 KILLED redis
 *   > STOP a at=967310 n=2 phase=workload why=cadence
 *   < CONT
 *   > SENT a id=1 at=969127
 *   > STOP a at=969127 n=3 phase=workload why=send
 *   < SNAP s retype
 *   > OK snap s at=969127 retype=1
 *   < CONT                                 ... stops until the workload marker, then the check
 *   > RESULT a fp=... wfp=... secs=2.6 stops=40 typed=1 inh=... verdict=HYDO-ORACLE: OK ...
 *   < RESUME b s
 *   > SENT b id=1 at=969127 retype=1
 *   > STOP b at=969127 n=3 phase=workload why=resume
 *   < CONT ...                             (the same stops and RESULT fingerprint as run a)
 * The real transcripts are in hydo/crates/hydo-engine/tests/lockstep/.
 */
#define HVX_STACK_NO_MAIN
#include "hvx_stack_verify.c"
#undef main
#include <time.h>
#include <stdarg.h>
#include <ctype.h>
#include "ptedges.h"

#define EXP_MEM_MIB	1024ULL	/* default guest RAM; "--ram-mib N" / HVX_EXPLORE_RAM_MIB change it */
#define EXP_MEM_MIN_MIB	256ULL	/* the smallest real-Linux guest the harnesses run (M27) */
#define EXP_MEM_MAX_MIB	3072ULL	/* hvx's MAX_GPAGES: RAM stays below the 4th-GiB MMIO hole (M28/M36) */
#define S0_MARKER	"hydo-explore-ready-S0"
#define S1_MARKER	"hydo-explore-ready-S1"
#define DONE_MARKER	"hydo-explore-workload-done"
#define ACKED_PREFIX	"hydo-explore-acked:"
#define FAILED_PREFIX	"hydo-explore-failed:"
/* Safety net only; a run that hits it is reported as TIMEOUT. HVX_EXPLORE_WALL_SECS overrides the
 * default 900 s (very short vCPU turns, q= of a few hundred branches, can need longer). */
static double run_wall_secs(void)
{
	const char *e = getenv("HVX_EXPLORE_WALL_SECS");
	return e && atof(e) > 0 ? atof(e) : 900;
}
#define RUN_WALL_SECS	run_wall_secs()

struct snap {
	struct hvx_snapshot *s;
	struct savebuf ram, rw;
	size_t ulen;		/* console watermark at the snapshot */
	uint8_t *con;		/* console text up to the watermark (part of every fingerprint) */
	uint8_t *unsync;	/* the disk's volatile write cache (HVX_GET_UNSYNC), unsync_n entries */
	uint32_t unsync_n;
	struct smpstate smp;	/* --vcpus N > 1: vCPUs 1..N-1 + the scheduler state */
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

/* ---- Cost measurement (PERF-RUN-COST.md, perf/): HVX_EXPLORE_PHASES=1 prints, on stderr only,
 * one "PHASES <tag> k=v ..." line per schedule with the host wall time of every phase and the
 * guest counters of the workload and of the check/recovery (exits = the module's retired-exit
 * clock, branches and ticks of the PMU clock, the virtual TSC, time inside the VMX section and
 * VM entries from HVX_GET_FTSTAT), and "PHBOOT" lines for the worker's boot to S0 and S1.
 * HVX_EXPLORE_DIRTY=1 also compares guest RAM and the data disk at the end of the workload (and
 * at the end of the check or recovery) with the snapshot the run started from, 4 KiB page by
 * page, and reports how many pages differ. Everything here only reads (GET ioctls, the RAM and
 * disk mmaps, the save files); the guest, the RESULT line and every fingerprint are unchanged.
 * Off (the default) nothing here runs. */
enum { PH_FRESH, PH_RSNAP, PH_RRAM, PH_RDISK, PH_RCON, PH_SETUP, PH_WORK, PH_WFP, PH_SAVECON,
       PH_CHECK, PH_FFP, PH_DURABLE, PH_RTEAR, PH_RFRESH, PH_RRESTORE, PH_RECOV, PH_RFP,
       PH_TEAR, PH_RUNIOCTL, PH_DRAIN, PH_DIRTY, PH_N };
static const char *const ph_name[PH_N] = {
	"fresh", "r_snap", "r_ram", "r_disk", "r_con", "setup", "workload", "wfp", "savecon",
	"check", "ffp", "durable", "rec_teardown", "rec_fresh", "rec_restore", "recovery", "rfp",
	"teardown", "run_ioctl", "drain", "dirtyscan" };
static int g_ph_on = -1, g_dirty_on;
static double g_ph[PH_N], g_ph_t0;
static uint64_t g_ph_calls;
static char g_ph_extra[4096];
static size_t g_ph_extra_n;
static struct snap *g_ph_s1;		/* the S1 snapshot (serve mode), for the dirty-page scan */
static int ph_on(void)
{
	if (g_ph_on < 0) {
		g_ph_on = getenv("HVX_EXPLORE_PHASES") && strcmp(getenv("HVX_EXPLORE_PHASES"), "0");
		g_dirty_on = getenv("HVX_EXPLORE_DIRTY") && strcmp(getenv("HVX_EXPLORE_DIRTY"), "0");
		if (g_dirty_on) g_ph_on = 1;
	}
	return g_ph_on;
}
#define PH_T()		(ph_on() ? now_s() : 0.0)
#define PH_ADD(i, t)	do { if (g_ph_on > 0) g_ph[i] += now_s() - (t); } while (0)
static void ph_reset(void)
{
	memset(g_ph, 0, sizeof(g_ph));
	g_ph_calls = 0;
	g_ph_extra_n = 0;
	g_ph_extra[0] = 0;
	g_ph_t0 = now_s();
}
static void ph_kv(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void ph_kv(const char *fmt, ...)
{
	va_list ap;
	if (g_ph_extra_n + 2 >= sizeof(g_ph_extra)) return;
	g_ph_extra[g_ph_extra_n++] = ' ';
	va_start(ap, fmt);
	g_ph_extra_n += (size_t)vsnprintf(g_ph_extra + g_ph_extra_n, sizeof(g_ph_extra) - g_ph_extra_n, fmt, ap);
	va_end(ap);
	if (g_ph_extra_n >= sizeof(g_ph_extra)) g_ph_extra_n = sizeof(g_ph_extra) - 1;
}
struct ph_ctr { uint64_t exits, branches, ticks, vtsc, cyc, entries, steps, calls, stepcyc, freecyc, stephw, rdtsc, dreq; };
static void ph_read(struct boot *b, struct ph_ctr *c)
{
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	struct hvx_pmu pm;
	struct hvx_ftstat *f = calloc(1, sizeof(*f));
	memset(c, 0, sizeof(*c));
	memset(&pm, 0, sizeof(pm));
	if (s && ioctl(b->fd, HVX_GET_SNAPSHOT, s) == 0) { c->exits = s->icount; c->vtsc = s->tsc; c->rdtsc = s->rdtsc_count; c->dreq = s->fault_req_seq; }
	if (ioctl(b->fd, HVX_GET_PMU, &pm) == 0) { c->branches = pm.total; c->ticks = pm.ticks; }
	if (f && ioctl(b->fd, HVX_GET_FTSTAT, f) == 0) {
		unsigned i;
		for (i = 0; i < f->n_vcpus && i < 8; i++) {
			c->cyc += f->v[i].total_cyc;
			c->entries += f->v[i].free_entries + f->v[i].step_entries;
			c->steps += f->v[i].step_entries;
			c->calls += f->v[i].calls;
			c->stepcyc += f->v[i].step_cyc;
			c->freecyc += f->v[i].free_cyc;
			c->stephw += f->v[i].step_hw_cyc;
		}
		if (f->tsc_khz) {	/* -> microseconds */
			c->cyc = c->cyc * 1000 / f->tsc_khz;
			c->stepcyc = c->stepcyc * 1000 / f->tsc_khz;
			c->freecyc = c->freecyc * 1000 / f->tsc_khz;
			c->stephw = c->stephw * 1000 / f->tsc_khz;
		}
	}
	free(s);
	free(f);
}
static void ph_delta(const char *pre, const struct ph_ctr *a, const struct ph_ctr *z)
{
	ph_kv("%s_exits=%llu %s_branches=%llu %s_ticks=%llu %s_vtsc=%llu %s_vmx_s=%.3f %s_entries=%llu %s_steps=%llu %s_runcalls=%llu",
	      pre, (unsigned long long)(z->exits - a->exits), pre, (unsigned long long)(z->branches - a->branches),
	      pre, (unsigned long long)(z->ticks - a->ticks), pre, (unsigned long long)(z->vtsc - a->vtsc),
	      pre, (z->cyc - a->cyc) / 1e6, pre, (unsigned long long)(z->entries - a->entries),
	      pre, (unsigned long long)(z->steps - a->steps), pre, (unsigned long long)(z->calls - a->calls));
	/* where the time inside the VMX section went: single-step tick/turn landings (exit handling
	 * + entry + one instruction), of that the hardware VMRESUME->exit part, and free-running entries */
	ph_kv("%s_step_s=%.3f %s_step_hw_s=%.3f %s_free_s=%.3f %s_rdtsc=%llu %s_diskreq=%llu", pre,
	      (z->stepcyc - a->stepcyc) / 1e6, pre, (z->stephw - a->stephw) / 1e6, pre, (z->freecyc - a->freecyc) / 1e6,
	      pre, (unsigned long long)(z->rdtsc - a->rdtsc), pre, (unsigned long long)(z->dreq - a->dreq));
}
static long ph_rss_kb(const char *key)
{
	char ln[256];
	long v = -1;
	size_t kl = strlen(key);
	FILE *f = fopen("/proc/self/status", "r");
	if (!f) return -1;
	while (fgets(ln, sizeof(ln), f))
		if (!strncmp(ln, key, kl)) { v = atol(ln + kl); break; }
	fclose(f);
	return v;
}
/* pages of `cur` that differ from the saved copy `sb` (4 KiB granularity); also counts how many
 * of the saved pages are all zero (nz = the non-zero ones) */
static void ph_diff(const char *pre, struct savebuf *sb, const uint8_t *cur, uint64_t size)
{
	static uint8_t chunk[1 << 20];
	static const uint8_t zero[4096];
	uint64_t off, diff = 0, nz = 0, curnz = 0;
	double t = now_s();
	for (off = 0; off < size; off += sizeof(chunk)) {
		uint64_t n = size - off < sizeof(chunk) ? size - off : sizeof(chunk), p;
		const uint8_t *src;
		if (sb->use_file) {
			if (pread(sb->fd, chunk, n, (off_t)off) != (ssize_t)n) { ph_kv("%s_dirty=ERR", pre); return; }
			src = chunk;
		} else {
			src = sb->mem + off;
		}
		for (p = 0; p < n; p += 4096) {
			size_t l = n - p < 4096 ? n - p : 4096;
			if (memcmp(src + p, cur + off + p, l)) diff++;
			if (memcmp(src + p, zero, l)) nz++;
			if (memcmp(cur + off + p, zero, l)) curnz++;
		}
	}
	ph_kv("%s_pages=%llu %s_dirty=%llu %s_nonzero_before=%llu %s_nonzero_after=%llu", pre,
	      (unsigned long long)((size + 4095) / 4096), pre, (unsigned long long)diff, pre,
	      (unsigned long long)nz, pre, (unsigned long long)curnz);
	g_ph[PH_DIRTY] += now_s() - t;
}
/* FNV-1a over a buffer that skips all-zero 4 KiB pages exactly: for a zero byte the step is
 * h = (h ^ 0) * P = h * P, so a zero page is h * P^4096 (mod 2^64). Same value as fnv(). */
static uint64_t fnv_zskip(uint64_t h, const uint8_t *p, size_t n)
{
	static const uint8_t zero[4096];
	static uint64_t p4096;
	size_t off;
	if (!p4096) {
		int i;
		p4096 = 1;
		for (i = 0; i < 4096; i++) p4096 *= 0x100000001b3ULL;
	}
	for (off = 0; off + 4096 <= n; off += 4096) {
		if (!memcmp(p + off, zero, 4096)) h *= p4096;
		else h = fnv(h, p + off, 4096);
	}
	return fnv(h, p + off, n - off);
}
/* HVX_EXPLORE_ZSKIP=1 (with HVX_EXPLORE_PHASES): at the end of the workload, hash guest RAM both
 * ways, check that the values agree and report both times. Reads only. */
static void ph_zskip(struct boot *b)
{
	double t1, t2, t3;
	uint64_t h1, h2;
	if (!getenv("HVX_EXPLORE_ZSKIP")) return;
	t1 = now_s();
	h1 = fnv(0xcbf29ce484222325ULL, b->mem, b->mem_size);
	t2 = now_s();
	h2 = fnv_zskip(0xcbf29ce484222325ULL, b->mem, b->mem_size);
	t3 = now_s();
	ph_kv("zskip_same=%d fnv_ram_s=%.3f fnv_zskip_ram_s=%.3f", h1 == h2, t2 - t1, t3 - t2);
	g_ph[PH_DIRTY] += t3 - t1;
}
static void ph_print(const char *tag)
{
	int i;
	if (g_ph_on <= 0) return;
	fprintf(stderr, "PHASES %s t0=%.3f total=%.3f", tag, g_ph_t0, now_s() - g_ph_t0);
	for (i = 0; i < PH_N; i++) fprintf(stderr, " %s=%.3f", ph_name[i], g_ph[i]);
	fprintf(stderr, " runcalls=%llu%s\n", (unsigned long long)g_ph_calls, g_ph_extra);
	fflush(stderr);
}

/* ---- Milestone 41: coverage feedback (Intel PT of the front door only) ----
 * Enabled by HVX_EXPLORE_PT_ELF=<the image's symbol-enabled hydostack, linked at a unique
 * address>. Each schedule's WORKLOAD VM traces guest user mode inside that program's text
 * range from the moment the schedule starts (S1) until the workload VM is torn down; the
 * trace is decoded to an edge file <outdir>/../cov/<tag>.edges. The recovery VM after a
 * power cut is not traced (it only runs the oracle). Tracing does not change the guest. */
static int g_pt;
static uint64_t g_pt_lo, g_pt_hi, g_pt_mib = 256;

/* ---- Milestone 44: whole-system coverage (hydo/richcov) ----
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
/* Diagnostic: the host's SMI count (MSR_SMI_COUNT 0x34, read through /dev/cpu/0/msr; SMIs
 * are broadcast, so CPU 0's count is everyone's). 0 when the msr driver is not there. */
static unsigned long long smi_count(void)
{
	static int fd = -2;
	unsigned long long v = 0;
	if (fd == -2) fd = open("/dev/cpu/0/msr", O_RDONLY);
	if (fd < 0 || pread(fd, &v, 8, 0x34) != 8) return 0;
	return v;
}

#define BRANCH_BUDGET 16

struct sched {
	char host[1024];
	char guest[512];
	uint32_t classes, prob;
	uint64_t dseed;
	int cut;
	int tj_on;
	struct hvx_tjitter tj;	/* tick-timing jitter for the workload (tj / tjx tokens) */
	uint64_t rb;		/* rb=<n>: run budget (see g_budget) */
	uint64_t pp, pptsc;	/* pp=<branches> pptsc=<tsc>: PMU tick period during the workload */
	int xt_on;
	struct hvx_xtick xt;	/* extra ticks during the workload (xt / xtx tokens) */
	int cf_on;		/* cf=<k> / cfn: power cut at the k-th disk FLUSH / count only */
	uint64_t cf;
	int q_on;		/* q=<branches> / qs=<hex>: vCPU turn lengths during the workload */
	uint64_t q, qs;		/* q 0 = keep the VM's turn length; qs 0 = fixed turns */
	int tc_on;		/* qb= / qbs= / qm=: HVX_SET_TURNCTL during the workload */
	struct hvx_turnctl tc;
};

/* ---- Lock-step control (M118, see the header). C.on is 0 unless a CTL/RESUME request or
 * `--control one` asked for it; with it 0 none of this runs and run_until_any is unchanged. */
#define CTL_MAX_SENDS	1024
#define CTL_MAX_SNAPS	8
#define CTL_SEND_MAX	4096	/* the module's serial receive ring (HVX_INPUT_SZ in hvx.c) */
enum { CS_PENDING, CS_TYPED, CS_ACKED, CS_RETYPE };
struct ctl_send { uint64_t id, at, sent; int state; uint32_t len; uint8_t *buf; };
struct ctl_book {		/* what a snapshot carries besides the VM */
	uint64_t nstop, nev, next_id, ntyped, inh;
	size_t evfrom;		/* console offset up to which lines were reported as EVENTs */
	int ns;
	struct ctl_send s[CTL_MAX_SENDS];
};
static struct {
	int on, start, capped;
	char tag[64];
	const char *phase;
	uint64_t at;		/* icount of the current stop */
	struct ctl_book k;
	char endv[256];
	const struct sched *sc;	/* the run's schedule, console start and timing info, for SNAP */
	size_t from;
	const char *tjinfo;
} C;
struct ctl_snap { char name[64]; struct snap sn; struct sched sc; size_t from; char tjinfo[160]; struct ctl_book k; };
static struct ctl_snap *g_ctl_snaps[CTL_MAX_SNAPS];
static int g_ctl_one;		/* --control: `one` runs under lock-step control */

static int put_bytes(struct boot *b, const uint8_t *p, size_t len);
static int snap_take(struct boot *b, struct snap *sn, const char *tag);
static void snap_free(struct snap *sn);

static void ctl_esc(FILE *f, const uint8_t *p, size_t n)
{
	size_t i;
	for (i = 0; i < n; i++) {
		uint8_t c = p[i];
		if (c == '\\') fputs("\\\\", f);
		else if (c == '\n') fputs("\\n", f);
		else if (c == '\r') fputs("\\r", f);
		else if (c == '\t') fputs("\\t", f);
		else if (c < 0x20 || c > 0x7e) fprintf(f, "\\x%02x", c);
		else fputc(c, f);
	}
}

/* the inverse of ctl_esc, in place; -1 on a malformed escape */
static long ctl_unesc(char *s)
{
	char *o = s;
	const char *p = s;
	while (*p) {
		if (*p != '\\') { *o++ = *p++; continue; }
		p++;
		if (*p == '\\') *o++ = '\\';
		else if (*p == 'n') *o++ = '\n';
		else if (*p == 'r') *o++ = '\r';
		else if (*p == 't') *o++ = '\t';
		else if (*p == 'x' && isxdigit((unsigned char)p[1]) && isxdigit((unsigned char)p[2])) {
			char h[3] = { p[1], p[2], 0 };
			*o++ = (char)strtoul(h, NULL, 16);
			p += 2;
		} else return -1;
		p++;
	}
	return o - s;
}

static uint64_t ctl_icount(struct boot *b)
{
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	uint64_t v = 0;
	if (s && ioctl(b->fd, HVX_GET_SNAPSHOT, s) == 0) v = s->icount;
	free(s);
	return v;
}

static void ctl_book_free(struct ctl_book *k)
{
	int i;
	for (i = 0; i < k->ns; i++) free(k->s[i].buf);
	k->ns = 0;
}

static int ctl_book_copy(struct ctl_book *d, const struct ctl_book *s)
{
	int i;
	*d = *s;
	for (i = 0; i < s->ns; i++) {
		d->s[i].buf = malloc(s->s[i].len ? s->s[i].len : 1);
		if (!d->s[i].buf) { d->ns = i; return -1; }
		memcpy(d->s[i].buf, s->s[i].buf, s->s[i].len);
	}
	return 0;
}

static void ctl_begin(const char *tag)
{
	ctl_book_free(&C.k);
	memset(&C, 0, sizeof(C));
	snprintf(C.tag, sizeof(C.tag), "%s", tag);
	C.on = C.start = 1;
	C.phase = "workload";
}

/* type one send now (at the current stop) */
static void ctl_type(struct boot *b, struct ctl_send *s, int print)
{
	int retype = s->state == CS_RETYPE;
	put_bytes(b, s->buf, s->len);
	s->sent = C.at;
	s->state = CS_TYPED;
	C.k.ntyped++;
	C.k.inh = fnv(C.k.inh ? C.k.inh : 0xcbf29ce484222325ULL, &C.at, sizeof(C.at));
	C.k.inh = fnv(C.k.inh, &s->len, sizeof(s->len));
	C.k.inh = fnv(C.k.inh, s->buf, s->len);
	if (print) printf("SENT %s id=%llu at=%llu%s\n", C.tag, (unsigned long long)s->id,
			  (unsigned long long)C.at, retype ? " retype=1" : "");
}

/* type every send due at this stop: retypes first (in id order), then pending ones with
 * at <= icount in (at, id) order; returns how many */
static int ctl_deliver(struct boot *b, int print)
{
	int i, n = 0;
	for (i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_RETYPE) { ctl_type(b, &C.k.s[i], print); n++; }
	for (;;) {
		struct ctl_send *best = NULL;
		for (i = 0; i < C.k.ns; i++) {
			struct ctl_send *s = &C.k.s[i];
			if (s->state != CS_PENDING || s->at > C.at) continue;
			if (!best || s->at < best->at || (s->at == best->at && s->id < best->id)) best = s;
		}
		if (!best) return n;
		ctl_type(b, best, print);
		n++;
	}
}

static int ctl_due(void)
{
	int i;
	for (i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_RETYPE || (C.k.s[i].state == CS_PENDING && C.k.s[i].at <= C.at)) return 1;
	return 0;
}

/* the event budget that cannot run past any pending send's target (0: no pending send) */
static uint64_t ctl_cap(void)
{
	uint64_t cap = 0;
	int i;
	for (i = 0; i < C.k.ns; i++) {
		uint64_t c;
		if (C.k.s[i].state != CS_PENDING || C.k.s[i].at <= C.at) continue;
		c = (C.k.s[i].at - C.at) / 2048;
		if (c < 1) c = 1;
		if (!cap || c < cap) cap = c;
	}
	return cap;
}

/* a phase change to a new VM (the recovery boot): queued input belongs to the old VM */
static void ctl_drop_pending(void)
{
	int i;
	for (i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_PENDING || C.k.s[i].state == CS_RETYPE) {
			C.k.s[i].state = CS_ACKED;
			printf("DROP %s id=%llu\n", C.tag, (unsigned long long)C.k.s[i].id);
		}
}

static void ctl_snap_cmd(struct boot *b, char *arg)
{
	char name[64] = "", mode[16] = "";
	struct ctl_snap *cs;
	char ids[512] = "";
	size_t il = 0;
	int i, slot = -1, n = sscanf(arg, "%63s %15s", name, mode);
	const struct sched *sc = C.sc;
	if (n < 1 || (n == 2 && strcmp(mode, "retype"))) { printf("ERR snap bad request\n"); return; }
	if (strcmp(C.phase, "workload")) { printf("ERR snap %s only in the workload phase\n", name); return; }
	if (sc->tj_on || sc->xt_on || sc->pp || sc->pptsc || sc->q_on || sc->cf_on) {
		printf("ERR snap %s not for tj/xt/pp/q/cf schedules\n", name); return;
	}
	for (i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_TYPED)
			il += (size_t)snprintf(ids + il, il < sizeof(ids) ? sizeof(ids) - il : 0, "%s%llu",
					       il ? "," : "", (unsigned long long)C.k.s[i].id);
	if (il >= sizeof(ids)) snprintf(ids + sizeof(ids) - 5, 5, ",...");
	if (il && n < 2) { printf("ERR snap %s unacked=%s\n", name, ids); return; }
	for (i = 0; i < CTL_MAX_SNAPS; i++)
		if (g_ctl_snaps[i] && !strcmp(g_ctl_snaps[i]->name, name)) slot = i;
	if (slot >= 0) {
		snap_free(&g_ctl_snaps[slot]->sn);
		ctl_book_free(&g_ctl_snaps[slot]->k);
		free(g_ctl_snaps[slot]);
		g_ctl_snaps[slot] = NULL;
	} else {
		for (i = 0; i < CTL_MAX_SNAPS && slot < 0; i++)
			if (!g_ctl_snaps[i]) slot = i;
		if (slot < 0) { printf("ERR snap %s no free slot (%d names)\n", name, CTL_MAX_SNAPS); return; }
	}
	cs = calloc(1, sizeof(*cs));
	if (!cs || snap_take(b, &cs->sn, "ctl") || ctl_book_copy(&cs->k, &C.k)) {
		if (cs) { snap_free(&cs->sn); ctl_book_free(&cs->k); free(cs); }
		printf("ERR snap %s snapshot failed\n", name);
		return;
	}
	for (i = 0; i < cs->k.ns; i++)
		if (cs->k.s[i].state == CS_TYPED) cs->k.s[i].state = CS_RETYPE;
	snprintf(cs->name, sizeof(cs->name), "%s", name);
	cs->sc = *sc;
	cs->from = C.from;
	snprintf(cs->tjinfo, sizeof(cs->tjinfo), "%s", C.tjinfo ? C.tjinfo : "");
	g_ctl_snaps[slot] = cs;
	printf("OK snap %s at=%llu retype=%s\n", name, (unsigned long long)C.at, il ? ids : "-");
}

/* One stop: report, then serve host lines until CONT (0) or END/EOF (1). */
static int ctl_stop(struct boot *b, const char *why)
{
	static char *ln;
	static size_t lcap;
	size_t p = C.k.evfrom, q;
	for (q = p; q < b->ulen; q++) {		/* complete console lines since the last stop */
		size_t e = q;
		if (b->ubuf[q] != '\n') continue;
		if (e > p && b->ubuf[e - 1] == '\r') e--;
		printf("EVENT %s at=%llu n=%llu ", C.tag, (unsigned long long)C.at, (unsigned long long)++C.k.nev);
		ctl_esc(stdout, b->ubuf + p, e - p);
		putchar('\n');
		p = q + 1;
	}
	C.k.evfrom = p;
	ctl_deliver(b, 1);
	printf("STOP %s at=%llu n=%llu phase=%s why=%s\n", C.tag, (unsigned long long)C.at,
	       (unsigned long long)++C.k.nstop, C.phase, why);
	fflush(stdout);
	for (;;) {
		ssize_t r = getline(&ln, &lcap, stdin);
		char *a;
		if (r < 0) { snprintf(C.endv, sizeof(C.endv), "ERROR control channel closed"); return 1; }
		while (r > 0 && (ln[r - 1] == '\n' || ln[r - 1] == '\r')) ln[--r] = 0;
		if (!strcmp(ln, "CONT")) return 0;
		if (!strncmp(ln, "END", 3) && (!ln[3] || ln[3] == ' ')) {
			snprintf(C.endv, sizeof(C.endv), "%s", ln[3] ? ln + 4 : "");
			return 1;
		}
		if (!strncmp(ln, "SEND at=", 8)) {
			char *end;
			unsigned long long at = strtoull(ln + 8, &end, 10);
			long len;
			struct ctl_send *s;
			if (end == ln + 8 || *end != ' ' || (len = ctl_unesc(end + 1)) < 0) { printf("ERR send bad request\n"); continue; }
			if (len == 0 || len > CTL_SEND_MAX) { printf("ERR send length %ld not in [1, %d]\n", len, CTL_SEND_MAX); continue; }
			if (C.k.ns >= CTL_MAX_SENDS) { printf("ERR send more than %d sends in one run\n", CTL_MAX_SENDS); continue; }
			s = &C.k.s[C.k.ns];
			memset(s, 0, sizeof(*s));
			if (!(s->buf = malloc((size_t)len))) { printf("ERR send out of memory\n"); continue; }
			memcpy(s->buf, end + 1, (size_t)len);
			s->len = (uint32_t)len;
			s->at = at;
			s->id = ++C.k.next_id;
			C.k.ns++;
			if (at <= C.at) {
				ctl_type(b, s, 0);
				printf("OK send id=%llu sent=%llu\n", (unsigned long long)s->id, (unsigned long long)C.at);
			} else {
				printf("OK send id=%llu queued=%llu\n", (unsigned long long)s->id, at);
			}
		} else if (!strncmp(ln, "ACK ", 4)) {
			unsigned long long id = strtoull(ln + 4, NULL, 10);
			int i, done = 0;
			for (i = 0; i < C.k.ns && !done; i++) {
				if (C.k.s[i].id != id) continue;
				done = 1;
				if (C.k.s[i].state == CS_PENDING) printf("ERR ack %llu not typed yet\n", id);
				else { C.k.s[i].state = CS_ACKED; printf("OK ack %llu\n", id); }
			}
			if (!done) printf("ERR ack %llu unknown\n", id);
		} else if (!strncmp(ln, "SNAP ", 5)) {
			ctl_snap_cmd(b, ln + 5);
		} else {
			a = ln;
			printf("ERR unknown command ");
			ctl_esc(stdout, (const uint8_t *)a, strlen(a));
			putchar('\n');
		}
		fflush(stdout);
	}
}

/* one run call under control: capped so that no call runs past a pending send */
static int ctl_run(struct boot *b, struct hvx_exit *ex)
{
	struct hvx_run_ctl c;
	uint64_t cap = ctl_cap();
	C.capped = cap && (!g_budget || cap < g_budget);
	memset(&c, 0, sizeof(c));
	c.max_insns = C.capped ? cap : g_budget;
	while (ioctl(b->fd, HVX_RUNC, &c) < 0) {
		if (errno == EINTR) continue;
		fprintf(stderr, "hvx_explore: HVX_RUNC: %s\n", strerror(errno));
		return -1;
	}
	C.at = c.icount;
	ex->reason = c.reason == HVX_EXIT_STEP ? HVX_EXIT_THROTTLE : c.reason;
	return 0;
}

static int run_until_any(struct boot *b, const char *const *mk, int n, size_t from)
{
	struct hvx_exit ex;
	double t0 = now_s();
	int i;
	if (C.on && C.start) {		/* lock-step: the phase's first stop, before any run call */
		const char *why = C.start == 2 ? "resume" : "start";
		C.start = 0;
		C.at = ctl_icount(b);
		if (ctl_stop(b, why)) return -3;
	}
	/* a marker may already be there (a branch request whose first stop already reached
	 * the end of the workload): stop where we are, as a single run would have */
	for (i = 0; i < n; i++)
		if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, mk[i]) >= 0)
			return i;
	for (;;) {
		double pht = PH_T();
		memset(&ex, 0, sizeof(ex));
		g_ph_calls++;
		if (C.on) {
			if (ctl_run(b, &ex)) return -1;
		} else if (g_budget) {
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
		PH_ADD(PH_RUNIOCTL, pht);
		pht = PH_T();
		console_drain(b);
		PH_ADD(PH_DRAIN, pht);
		if (getenv("HVX_EXPLORE_RETLOG")) {	/* diagnostic: where each run call returned */
			struct hvx_pmu pm;
			struct hvx_tsc ts;
			memset(&pm, 0, sizeof(pm));
			memset(&ts, 0, sizeof(ts));
			ioctl(b->fd, HVX_GET_PMU, &pm);
			ioctl(b->fd, HVX_GET_TSC, &ts);
			fprintf(stderr, "RET reason=%u ulen=%zu total=%llu ticks=%llu over=%llu land=%016llx steps=%llu hops=%llu tsc=%llu rdtsc=%llu smi=%llu\n",
				ex.reason, b->ulen, (unsigned long long)pm.total, (unsigned long long)pm.ticks,
				(unsigned long long)pm.overshoots, (unsigned long long)pm.land_hash,
				(unsigned long long)pm.steps, (unsigned long long)pm.hops,
				(unsigned long long)ts.tsc, (unsigned long long)ts.count,
				(unsigned long long)smi_count());
		}
		if (C.on) {			/* lock-step: report this stop unless it only landed short */
			int hit = -1;
			for (i = 0; i < n && hit < 0; i++)
				if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, mk[i]) >= 0)
					hit = i;
			if (hit >= 0 || !C.capped || ex.reason != HVX_EXIT_THROTTLE || ctl_due()) {
				const char *why = hit >= 0 ? "marker" : ex.reason != HVX_EXIT_THROTTLE ? "exit" :
						  ctl_due() ? "send" : "cadence";
				if (ctl_stop(b, why)) return -3;
			}
		}
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

static int put_bytes(struct boot *b, const uint8_t *p, size_t len)
{
	size_t off = 0;
	while (off < len) {
		struct hvx_input in;
		size_t c = len - off > HVX_INPUT_MAX ? HVX_INPUT_MAX : len - off;
		memset(&in, 0, sizeof(in));
		in.len = (uint32_t)c;
		memcpy(in.buf, p + off, c);
		if (ioctl(b->fd, HVX_PUT_INPUT, &in) < 0) { perror("HVX_PUT_INPUT"); return -1; }
		off += c;
	}
	return 0;
}

static int put_input(struct boot *b, const char *line)
{
	return put_bytes(b, (const uint8_t *)line, strlen(line));
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
	if (smp_get(b->fd, &sn->smp)) return -1;	/* nothing with one vCPU */
	if (savebuf_open(&sn->ram, E.mem_size, tag) || savebuf_open(&sn->rw, E.p2_len, tag)) return -1;
	if (savebuf_store(&sn->ram, b->mem, E.mem_size) || savebuf_store(&sn->rw, b->disk_mm + E.p2_off, E.p2_len)) return -1;
	sn->ulen = b->ulen;
	sn->con = malloc(b->ulen + 1);
	if (!sn->con) return -1;
	memcpy(sn->con, b->ubuf, b->ulen);
	{	/* writes the guest was told are done but no FLUSH made durable yet: not in the
		 * snapshot struct, and a mid-workload snapshot of a power-cut schedule has some */
		struct hvx_unsync_io io;
		memset(&io, 0, sizeof(io));
		io.len = HVX_UNSYNC_BYTES(HVX_UNSYNC_MAX);
		sn->unsync = malloc(io.len);
		if (!sn->unsync) return -1;
		io.addr = (uint64_t)(uintptr_t)sn->unsync;
		if (ioctl(b->fd, HVX_GET_UNSYNC, &io) < 0) { perror("HVX_GET_UNSYNC"); return -1; }
		sn->unsync_n = io.n;
	}
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
static int g_ph_rsplit;	/* cost measurement: split this restore's time into the r_* phases */
static int snap_restore(struct boot *b, struct snap *sn, struct savebuf *rw)
{
	double pht = PH_T();
	if (ioctl(b->fd, HVX_SET_SNAPSHOT, sn->s) < 0) { perror("HVX_SET_SNAPSHOT"); return -1; }
	if (smp_set(b->fd, &sn->smp)) return -1;	/* nothing with one vCPU */
	if (g_ph_rsplit) { PH_ADD(PH_RSNAP, pht); pht = PH_T(); }
	if (savebuf_load(&sn->ram, b->mem, E.mem_size)) return -1;
	if (g_ph_rsplit) { PH_ADD(PH_RRAM, pht); pht = PH_T(); }
	if (savebuf_load(rw ? rw : &sn->rw, b->disk_mm + E.p2_off, E.p2_len)) return -1;
	if (g_ph_rsplit) { PH_ADD(PH_RDISK, pht); pht = PH_T(); }
	memcpy(b->ubuf, sn->con, sn->ulen);	/* console as it was at the snapshot */
	b->ulen = sn->ulen;
	console_rebase(b);			/* a fresh VM: drained = its (empty) console */
	{	/* the held writes, after SET_SNAPSHOT (which restores the fault policy) */
		struct hvx_unsync_io io;
		memset(&io, 0, sizeof(io));
		io.addr = (uint64_t)(uintptr_t)sn->unsync;
		io.len = HVX_UNSYNC_BYTES(sn->unsync_n);
		io.n = sn->unsync_n;
		if (getenv("HVX_EXPLORE_NO_UNSYNC"))	/* diagnostic: drop the held writes (the old bug) */
			io.n = 0, io.len = 0;
		if (ioctl(b->fd, HVX_SET_UNSYNC, &io) < 0) { perror("HVX_SET_UNSYNC"); return -1; }
	}
	if (g_ph_rsplit) PH_ADD(PH_RCON, pht);
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

/* the turn policy to put back after a q= / qs= workload, and the turn count at its start */
static uint64_t g_q_old_quantum, g_q_old_seed, g_q_turns0;
/* Diagnostic, several vCPUs only: each vCPU's retired branches and turns during the workload,
 * printed on stderr as a SCHEDSTAT line (reads only; the RESULT line is unchanged). */
static uint64_t g_ss_br0[HVX_SCHED_MAX_VCPUS], g_ss_turns0[HVX_SCHED_MAX_VCPUS];

static void schedstat(struct boot *b, const char *tag, int start)
{
	struct hvx_sched c;
	if (g_vcpus < 2) return;
	memset(&c, 0, sizeof(c));
	if (ioctl(b->fd, HVX_GET_SCHED, &c) < 0) return;
	if (start) {
		for (unsigned i = 0; i < g_vcpus && i < HVX_SCHED_MAX_VCPUS; i++) {
			g_ss_br0[i] = c.v[i].branches;
			g_ss_turns0[i] = c.v[i].turns;
		}
		return;
	}
	fprintf(stderr, "SCHEDSTAT %s", tag);
	for (unsigned i = 0; i < g_vcpus && i < HVX_SCHED_MAX_VCPUS; i++)
		fprintf(stderr, " v%u_br=%llu v%u_turns=%llu", i, (unsigned long long)(c.v[i].branches - g_ss_br0[i]),
			i, (unsigned long long)(c.v[i].turns - g_ss_turns0[i]));
	fprintf(stderr, "\n");
}

/* Diagnostic (HVX_EXPLORE_VCPUSTAT=1, several vCPUs only): on stderr, the guest's "smp: Brought
 * up" line at S0, and per run the conditional branches and turns each vCPU took during the
 * workload ("VCPUSTAT <tag> v0 br=.. t=.. v1 br=.. t=.."). Reads HVX_GET_SCHED only, so the
 * guest, the RESULT line and every fingerprint are unchanged. */
static uint64_t g_vs_br0[HVX_SCHED_MAX_VCPUS], g_vs_t0[HVX_SCHED_MAX_VCPUS];
static int vcpustat_on(void) { return g_vcpus > 1 && getenv("HVX_EXPLORE_VCPUSTAT"); }
static void vcpustat(struct boot *b, const char *tag)
{
	struct hvx_sched c;
	unsigned i;
	char out[512];
	size_t l = 0;
	memset(&c, 0, sizeof(c));
	if (ioctl(b->fd, HVX_GET_SCHED, &c) < 0) { perror("HVX_GET_SCHED (vcpustat)"); return; }
	if (!tag) {
		for (i = 0; i < g_vcpus; i++) { g_vs_br0[i] = c.v[i].branches; g_vs_t0[i] = c.v[i].turns; }
		return;
	}
	for (i = 0; i < g_vcpus && l < sizeof(out) - 64; i++)
		l += snprintf(out + l, sizeof(out) - l, " v%u br=%llu t=%llu", i,
			      (unsigned long long)(c.v[i].branches - g_vs_br0[i]),
			      (unsigned long long)(c.v[i].turns - g_vs_t0[i]));
	fprintf(stderr, "VCPUSTAT %s%s\n", tag, out);
}

/* The value of host token <key> ("q=" ...) starting a word of `host`, or NULL. Exact word
 * match, so "q=" never matches inside another token. */
static const char *host_tok(const char *host, const char *key)
{
	const char *p = host;
	size_t n = strlen(key);
	while ((p = strstr(p, key))) {
		if (p == host || p[-1] == ' ') return p + n;
		p += n;
	}
	return NULL;
}

/* q=<branches> qs=<hex seed> (see the header). Returns -1 on a malformed token. */
static int parse_turns(const char *host, struct sched *sc)
{
	const char *p;
	char *end;
	if ((p = host_tok(host, "q="))) {
		sc->q = strtoull(p, &end, 10);
		if (end == p || !sc->q || (*end && *end != ' ')) return -1;
		sc->q_on = 1;
	}
	if ((p = host_tok(host, "qs="))) {
		sc->qs = strtoull(p, &end, 16);
		if (end == p || !sc->qs || (*end && *end != ' ')) return -1;
		sc->q_on = 1;
	}
	if ((p = host_tok(host, "qb="))) {	/* <B>:<s>:<permille> */
		unsigned long long bl, bq, pm;
		bl = strtoull(p, &end, 10);
		if (end == p || *end != ':' || !bl) return -1;
		p = end + 1;
		bq = strtoull(p, &end, 10);
		if (end == p || *end != ':' || !bq) return -1;
		p = end + 1;
		pm = strtoull(p, &end, 10);
		if (end == p || pm > 1000 || (*end && *end != ' ')) return -1;
		sc->tc.burst_len = bl;
		sc->tc.burst_q = bq;
		sc->tc.burst_permille = (uint32_t)pm;
		if (!sc->tc.burst_seed) sc->tc.burst_seed = 1;
		sc->tc_on = sc->q_on = 1;
	}
	if ((p = host_tok(host, "qbs="))) {
		sc->tc.burst_seed = strtoull(p, &end, 16);
		if (end == p || !sc->tc.burst_seed || (*end && *end != ' ')) return -1;
		if (!sc->tc.burst_len) return -1;	/* qbs= without qb= */
	}
	if ((p = host_tok(host, "qm="))) {
		sc->tc.margin = strtoull(p, &end, 10);
		if (end == p || sc->tc.margin < 8 || (*end && *end != ' ')) return -1;
		sc->tc_on = sc->q_on = 1;
	}
	return 0;
}

/* the PMU period and tick TSC to put back after a pp= workload */
static uint64_t g_pp_old_period, g_pp_old_tsc;

/* pp= pptsc= xt= xtr= xtpm= xtlo= xthi= xtsc= xtx= (see the header) */
static int parse_preempt(const char *host, struct sched *sc)
{
	const char *p;

	if ((p = strstr(host, "pp="))) sc->pp = strtoull(p + 3, NULL, 10);
	if ((p = strstr(host, "pptsc="))) sc->pptsc = strtoull(p + 6, NULL, 10);
	if ((p = strstr(host, "xtsc="))) sc->xt.tsc = strtoull(p + 5, NULL, 10);
	if ((p = strstr(host, "xtx="))) {
		char *end;
		p += 4;
		while (*p && *p != ' ' && sc->xt.n_explicit < HVX_XT_MAX_EXPLICIT) {
			sc->xt.explicit_[sc->xt.n_explicit++] = strtoull(p, &end, 10);
			if (end == p) return -1;
			p = (*end == ',') ? end + 1 : end;
		}
		if (*p && *p != ' ') return -1;		/* more than HVX_XT_MAX_EXPLICIT entries */
		sc->xt_on = sc->xt.n_explicit > 0;
		return 0;
	}
	if (!(p = strstr(host, "xt="))) return 0;
	sc->xt.seed = strtoull(p + 3, NULL, 16);
	sc->xt.rate = (p = strstr(host, "xtr=")) ? (uint32_t)strtoul(p + 4, NULL, 10) : 4;
	sc->xt.permille = (p = strstr(host, "xtpm=")) ? (uint32_t)strtoul(p + 5, NULL, 10) : 500;
	if ((p = strstr(host, "xtlo="))) sc->xt.lo = strtoull(p + 5, NULL, 10);
	if ((p = strstr(host, "xthi="))) sc->xt.hi = strtoull(p + 5, NULL, 10);
	sc->xt_on = sc->xt.rate && sc->xt.permille;
	return 0;
}

static int set_xt(struct boot *b, const struct hvx_xtick *xt)
{
	static struct hvx_xtick off;
	if (ioctl(b->fd, HVX_SET_XTICK, xt ? xt : &off) < 0) { perror("HVX_SET_XTICK"); return -1; }
	return 0;
}

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
	if (parse_preempt(sc->host, sc)) return -1;
	if (parse_turns(sc->host, sc)) return -1;
	if ((p = strstr(sc->host, "rb="))) sc->rb = strtoull(p + 3, NULL, 10);
	sc->cut = strstr(sc->guest, " C=") != NULL;
	/* cf=<k>: the disk loses power AT the k-th FLUSH of the workload (that flush is not
	 * executed; nothing is completed after it); the guest runs on to the end of its
	 * workload with a dead disk, then the recovery boots on the flushed bytes, as after
	 * C=. Needs disk=cut (the volatile write cache). cfn: count the FLUSHes only. */
	if ((p = strstr(sc->host, "cf="))) {
		sc->cf = strtoull(p + 3, NULL, 10);
		if (!sc->cf || sc->classes != HVX_FAULT_DROP_UNFLUSHED || sc->cut) return -1;
		sc->cf_on = 1;
		sc->cut = 1;
	} else if (strstr(sc->host, "cfn")) {
		sc->cf_on = 1;
	}
	return 0;
}

/* Diagnostic (HVX_EXPLORE_DUMP=<dir>): at each fingerprint point print the parts of the
 * fingerprint on stderr ("SUBFP <tag> <phase> con= snap= ram= disk=") and write the console,
 * the snapshot struct, all guest RAM and the data disk to <dir>/<tag>.<phase>.{con,snap,ram,disk}
 * (HVX_EXPLORE_DUMP_HASHONLY=1: the hashes only). Reads only; the guest does not run here. */
static void dump_write(const char *dir, const char *tag, const char *phase, const char *ext,
		       const void *p, size_t n)
{
	char path[600];
	FILE *f;
	snprintf(path, sizeof(path), "%s/%s.%s.%s", dir, tag, phase, ext);
	if (!(f = fopen(path, "w"))) { perror(path); return; }
	if (n && fwrite(p, 1, n, f) != n) perror(path);
	fclose(f);
}

static void dump_state(struct boot *b, const char *tag, const char *phase)
{
	const char *dir = getenv("HVX_EXPLORE_DUMP");
	struct hvx_snapshot *s;
	uint64_t h0 = 0xcbf29ce484222325ULL;
	if (!dir || b->fd < 0) return;
	s = calloc(1, sizeof(*s));
	if (!s) return;
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return; }
	fprintf(stderr, "SUBFP %s %s ulen=%zu con=%016llx snap=%016llx ram=%016llx disk=%016llx\n", tag, phase,
		b->ulen,
		(unsigned long long)fnv(h0, b->ubuf, b->ulen),
		(unsigned long long)fnv_snapshot(h0, s),
		(unsigned long long)fnv(h0, b->mem, b->mem_size),
		(unsigned long long)fnv(h0, b->disk_mm + b->p2_off, b->p2_len));
	fflush(stderr);
	if (!getenv("HVX_EXPLORE_DUMP_HASHONLY")) {
		dump_write(dir, tag, phase, "con", b->ubuf, b->ulen);
		dump_write(dir, tag, phase, "snap", s, sizeof(*s));
		dump_write(dir, tag, phase, "ram", b->mem, b->mem_size);
		dump_write(dir, tag, phase, "disk", b->disk_mm + b->p2_off, b->p2_len);
	}
	free(s);
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
	/* orc: the recovery boot's "oracle" command. 3584 bytes (was 512) so that the fake outside
	 * world of a manifest with fakes:/proxies: (hydo-init-fakes.sh appends its records to the
	 * acknowledged ids at a power cut, at most 3000 bytes) fits; the serial input ring is 4096.
	 * A command that fitted before is unchanged. */
	char verdict[256] = "", orc[3584], tjinfo[160];
	uint64_t wfp = 0, ffp = 0;
	int r;
	const char *phase = "workload";

	struct ph_ctr pc0, pc1, pc2;
	double pht;
	uint64_t phc0 = g_ph_calls;

	snprintf(tjinfo, sizeof(tjinfo), "%s", tjinfo_in);
	C.sc = sc;			/* lock-step: what a SNAP in this run carries */
	C.from = from;
	C.tjinfo = tjinfo_in;
	if (ph_on()) ph_read(b, &pc0);
	pht = PH_T();
	r = run_until_any(b, wk, 2, from);
	PH_ADD(PH_WORK, pht);
	if (r < 0 && r != -3) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT workload" : "ERROR vm workload"); goto out; }
	if (ph_on()) {
		ph_read(b, &pc1);
		ph_delta("w", &pc0, &pc1);
		ph_kv("w_runcalls_user=%llu rss_kb=%ld rss_shmem_kb=%ld con_bytes=%zu",
		      (unsigned long long)(g_ph_calls - phc0), ph_rss_kb("VmRSS:"), ph_rss_kb("RssShmem:"), b->ulen - from);
		if (g_dirty_on && g_ph_s1) {
			ph_diff("w_ram", &g_ph_s1->ram, b->mem, E.mem_size);
			ph_diff("w_disk", &g_ph_s1->rw, b->disk_mm + E.p2_off, E.p2_len);
		}
		ph_zskip(b);
	}
	pht = PH_T();
	wfp = fingerprint(b);
	PH_ADD(PH_WFP, pht);
	schedstat(b, tag, 0);
	dump_state(b, tag, "workload");
	if (sc->cf_on) {		/* on stderr, so the RESULT line keeps its format */
		struct hvx_flushcut fc;
		memset(&fc, 0, sizeof(fc));
		ioctl(b->fd, HVX_GET_FLUSHCUT, &fc);
		fprintf(stderr, "FLUSHCUT %s target=%llu flushes=%llu off=%u hung=%llu\n", tag,
			(unsigned long long)fc.target, (unsigned long long)fc.count, fc.off,
			(unsigned long long)fc.hung);
	}
	if (sc->tj_on) {		/* the jitter covers the workload only, like the disk fault */
		static struct hvx_tjitter got;
		size_t l = strlen(tjinfo);
		memset(&got, 0, sizeof(got));
		ioctl(b->fd, HVX_GET_TJITTER, &got);
		snprintf(tjinfo + l, sizeof(tjinfo) - l, "-%llu tjn=%llu",
			 (unsigned long long)got.last_tick, (unsigned long long)got.applied);
		set_tj(b, NULL);
	}
	if (sc->xt_on) {		/* extra ticks and the short period also cover the workload only */
		static struct hvx_xtick got;
		size_t l = strlen(tjinfo);
		memset(&got, 0, sizeof(got));
		ioctl(b->fd, HVX_GET_XTICK, &got);
		snprintf(tjinfo + l, sizeof(tjinfo) - l, " xtn=%llu", (unsigned long long)got.applied);
		set_xt(b, NULL);
	}
	if (vcpustat_on())
		vcpustat(b, tag);
	if (sc->q_on) {			/* the turn lengths too: report the turns, put the policy back */
		struct hvx_sched c;
		size_t l = strlen(tjinfo);
		memset(&c, 0, sizeof(c));
		if (ioctl(b->fd, HVX_GET_SCHED, &c) == 0) {
			snprintf(tjinfo + l, sizeof(tjinfo) - l, " qn=%llu", (unsigned long long)(c.turns - g_q_turns0));
			if (sc->tc_on) {	/* burst statistics, then bursts and margin back to the default */
				struct hvx_turnctl t;
				memset(&t, 0, sizeof(t));
				if (sc->tc.burst_len && ioctl(b->fd, HVX_GET_TURNCTL, &t) == 0) {
					l = strlen(tjinfo);
					snprintf(tjinfo + l, sizeof(tjinfo) - l, " qbn=%llu", (unsigned long long)t.burst_turns);
				}
				memset(&t, 0, sizeof(t));	/* back to the engine's margin (HVX_TURN_MARGIN) */
				if (getenv("HVX_TURN_MARGIN")) t.margin = strtoull(getenv("HVX_TURN_MARGIN"), NULL, 0);
				if (ioctl(b->fd, HVX_SET_TURNCTL, &t) < 0) perror("HVX_SET_TURNCTL (restore)");
			}
			c.quantum = g_q_old_quantum;
			c.seed = g_q_old_seed;
			if (ioctl(b->fd, HVX_SET_SCHED, &c) < 0) perror("HVX_SET_SCHED (restore turns)");
		} else {
			perror("HVX_GET_SCHED");
		}
	}
	if (sc->pp || sc->pptsc) {
		struct hvx_pmu pm;
		memset(&pm, 0, sizeof(pm));
		pm.period = g_pp_old_period;
		pm.tick_tsc = g_pp_old_tsc;
		ioctl(b->fd, HVX_SET_PMU, &pm);
	}
	if (r == 1 || r == -3) {	/* the guest gave a verdict during the workload (e.g. setup
					 * error), or the host ended the run here (lock-step END) */
		if (r == -3 && C.endv[0]) snprintf(verdict, sizeof(verdict), "%s", C.endv);
		else last_verdict(b, from, verdict, sizeof(verdict));
		ffp = wfp;
		goto out;
	}
	oracle_cmd(b, from, orc, sizeof(orc));
	pht = PH_T();
	save_console(b, from, tag, "");
	PH_ADD(PH_SAVECON, pht);
	if (!sc->cut) {			/* same boot: faults off, heal, recover, check */
		size_t f2 = b->ulen;
		phase = "check";
		if (set_fault(b, 0, 0, 0) || put_input(b, "go\n")) { snprintf(verdict, sizeof(verdict), "ERROR harness go"); goto out; }
		if (ph_on()) ph_read(b, &pc1);
		pht = PH_T();
		C.phase = "check";
		C.start = 1;
		r = run_until_any(b, fin, 1, f2);
		PH_ADD(PH_CHECK, pht);
		if (r < 0 && r != -3) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT check" : "ERROR vm check"); goto out_save; }
		if (ph_on()) {
			ph_read(b, &pc2);
			ph_delta("c", &pc1, &pc2);
			if (g_dirty_on && g_ph_s1) {
				ph_diff("c_ram", &g_ph_s1->ram, b->mem, E.mem_size);
				ph_diff("c_disk", &g_ph_s1->rw, b->disk_mm + E.p2_off, E.p2_len);
			}
		}
		pht = PH_T();
		ffp = fingerprint(b);
		PH_ADD(PH_FFP, pht);
		dump_state(b, tag, "check");
		if (r == -3 && C.endv[0]) snprintf(verdict, sizeof(verdict), "%s", C.endv);
		else last_verdict(b, f2, verdict, sizeof(verdict));
		save_console(b, f2, tag, "");
		pt_collect(b, tag);
		goto out_done;
	}
	{	/* power cut: only the flushed disk bytes survive; recover in a fresh VM from S0 */
		struct savebuf durable;
		struct boot ob;
		phase = "recovery";
		pht = PH_T();
		if (savebuf_open(&durable, E.p2_len, "durable") || savebuf_store(&durable, b->disk_mm + E.p2_off, E.p2_len)) {
			snprintf(verdict, sizeof(verdict), "ERROR harness durable"); goto out;
		}
		PH_ADD(PH_DURABLE, pht);
		if (getenv("HVX_EXPLORE_DURABLE_OUT")) {	/* diagnostic: keep the post-cut data disk */
			FILE *df = fopen(getenv("HVX_EXPLORE_DURABLE_OUT"), "w");
			if (df) { fwrite(b->disk_mm + E.p2_off, 1, E.p2_len, df); fclose(df); }
		}
		pt_collect(b, tag);
		pht = PH_T();
		boot_teardown(b);
		PH_ADD(PH_RTEAR, pht);
		pht = PH_T();
		if (vm_fresh(&ob)) {
			savebuf_close(&durable);
			snprintf(verdict, sizeof(verdict), "ERROR harness recovery vm"); goto out;
		}
		PH_ADD(PH_RFRESH, pht);
		pht = PH_T();
		if (snap_restore(&ob, s0, &durable)) {
			savebuf_close(&durable);
			snprintf(verdict, sizeof(verdict), "ERROR harness recovery vm"); goto out;
		}
		PH_ADD(PH_RRESTORE, pht);
		savebuf_close(&durable);
		*b = ob;
		from = b->ulen;
		if (set_fault(b, 0, 0, 0) || put_input(b, orc)) { snprintf(verdict, sizeof(verdict), "ERROR harness oracle cmd"); goto out; }
		if (ph_on()) ph_read(b, &pc1);
		pht = PH_T();
		if (C.on) {		/* a new VM with its own clock: queued input does not carry over */
			ctl_drop_pending();
			C.phase = "recovery";
			C.start = 1;
			C.k.evfrom = from;
		}
		r = run_until_any(b, fin, 1, from);
		PH_ADD(PH_RECOV, pht);
		if (r < 0 && r != -3) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT recovery" : "ERROR vm recovery"); goto out_save; }
		if (ph_on()) {
			ph_read(b, &pc2);
			ph_delta("rc", &pc1, &pc2);
			if (g_dirty_on)
				ph_diff("rc_ram", &s0->ram, b->mem, E.mem_size);
		}
		pht = PH_T();
		ffp = fingerprint(b);
		PH_ADD(PH_RFP, pht);
		dump_state(b, tag, "recovery");
		if (r == -3 && C.endv[0]) snprintf(verdict, sizeof(verdict), "%s", C.endv);
		else last_verdict(b, from, verdict, sizeof(verdict));
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
	if (C.on) {			/* lock-step facts, before verdict= like the other extras */
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof(tjinfo) - l, " stops=%llu typed=%llu inh=%016llx",
			 (unsigned long long)C.k.nstop, (unsigned long long)C.k.ntyped, (unsigned long long)C.k.inh);
	}
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f%s verdict=%s\n", tag,
	       (unsigned long long)fnv(wfp, &ffp, sizeof(ffp)), (unsigned long long)wfp, now_s() - t0,
	       tjinfo, verdict);
	fflush(stdout);
	if (C.on) { ctl_book_free(&C.k); C.on = 0; }
	pht = PH_T();
	boot_teardown(b);
	PH_ADD(PH_TEAR, pht);
	if (ph_on()) {
		ph_kv("cut=%d secs=%.3f", sc->cut, now_s() - t0);
		ph_print(tag);
	}
}

static void run_sched(struct boot *b, const char *tag, const struct sched *sc, struct snap *s0)
{
	char line[600], verdict[256] = "", tjinfo[160] = "";
	size_t from = b->ulen;
	double t0 = now_s();

	g_budget = sc->rb;
	if (C.on) C.k.evfrom = from;	/* lock-step: EVENTs start with the workload's console */
	if (sc->q_on && g_vcpus < 2) {	/* never silently ignored */
		snprintf(verdict, sizeof(verdict), "ERROR harness q=/qs=/qb=/qm= need several vCPUs (--vcpus N > 1)"); goto fail;
	}
	if (set_fault(b, sc->dseed, sc->classes, sc->prob)) { snprintf(verdict, sizeof(verdict), "ERROR harness set_fault"); goto fail; }
	if (sc->cf_on) {
		struct hvx_flushcut fc;
		memset(&fc, 0, sizeof(fc));
		fc.on = 1;
		fc.target = sc->cf;
		fc.lo = E.p2_off;		/* only the data partition goes dark; the */
		fc.hi = E.p2_off + E.p2_len;	/* read-only root on p1 keeps being served */
		if (ioctl(b->fd, HVX_SET_FLUSHCUT, &fc) < 0) { snprintf(verdict, sizeof(verdict), "ERROR harness cf= (HVX_SET_FLUSHCUT)"); goto fail; }
	}
	if (sc->pp || sc->pptsc) {	/* before the jitter: its delays are checked against the period */
		struct hvx_pmu pm;
		memset(&pm, 0, sizeof(pm));
		if (ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) {
			snprintf(verdict, sizeof(verdict), "ERROR harness pp= (needs the PMU clock)"); goto fail;
		}
		g_pp_old_period = pm.period;
		g_pp_old_tsc = pm.tick_tsc;
		pm.period = sc->pp;
		pm.tick_tsc = sc->pptsc;
		if (ioctl(b->fd, HVX_SET_PMU, &pm) < 0) { snprintf(verdict, sizeof(verdict), "ERROR harness pp= value"); goto fail; }
	}
	if (sc->xt_on && set_xt(b, &sc->xt)) {
		snprintf(verdict, sizeof(verdict), "ERROR harness extra ticks (needs the PMU clock)"); goto fail;
	}
	if (sc->tj_on) {
		struct hvx_pmu pm;
		memset(&pm, 0, sizeof(pm));
		if (set_tj(b, &sc->tj) || ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) {
			snprintf(verdict, sizeof(verdict), "ERROR harness tick jitter (needs the PMU clock)"); goto fail;
		}
		snprintf(tjinfo, sizeof(tjinfo), " tjk=%llu", (unsigned long long)(pm.next / pm.period));
	}
	if (sc->q_on) {			/* turn lengths for the workload only, like the jitter */
		struct hvx_sched c;
		memset(&c, 0, sizeof(c));
		if (ioctl(b->fd, HVX_GET_SCHED, &c) < 0) { snprintf(verdict, sizeof(verdict), "ERROR harness q= (HVX_GET_SCHED)"); goto fail; }
		g_q_old_quantum = c.quantum;
		g_q_old_seed = c.seed;
		g_q_turns0 = c.turns;
		if (sc->q) c.quantum = sc->q;
		c.seed = sc->qs;
		if (ioctl(b->fd, HVX_SET_SCHED, &c) < 0) { snprintf(verdict, sizeof(verdict), "ERROR harness q= value (HVX_SET_SCHED)"); goto fail; }
		struct hvx_turnctl tc = sc->tc;
		if (!tc.margin && getenv("HVX_TURN_MARGIN")) tc.margin = strtoull(getenv("HVX_TURN_MARGIN"), NULL, 0);
		if (sc->tc_on && ioctl(b->fd, HVX_SET_TURNCTL, &tc) < 0) {
			snprintf(verdict, sizeof(verdict), "ERROR harness qb=/qm= (HVX_SET_TURNCTL; needs a module with it)"); goto fail;
		}
	}
	schedstat(b, tag, 1);
	if (vcpustat_on())
		vcpustat(b, NULL);
	pt_arm(b);
	snprintf(line, sizeof(line), "%s\n", sc->guest);
	if (!(C.on && !sc->guest[0]) && put_input(b, line)) {	/* lock-step: an empty guest part types nothing */
		snprintf(verdict, sizeof(verdict), "ERROR harness put_input"); goto fail;
	}
	PH_ADD(PH_SETUP, t0);
	finish_sched(b, tag, sc, s0, from, t0, tjinfo);
	g_budget = 0;
	return;
fail:
	sched_fail(b, tag, from, t0, tjinfo, verdict);
	if (C.on) { ctl_book_free(&C.k); C.on = 0; }
	g_budget = 0;
}

static void snap_free(struct snap *sn)
{
	if (!sn->have) return;
	savebuf_close(&sn->ram);
	savebuf_close(&sn->rw);
	free(sn->s);
	free(sn->con);
	free(sn->unsync);
	smp_free(&sn->smp);
	sn->have = 0;
}

/* Run until the PMU clock reaches tick `target` -- or, with target 0, until the disk's
 * volatile write cache holds writes -- (returns 0), or the workload ends first (returns 1
 * for DONE, 2 for FINAL), or an error (< 0). Needs g_budget. */
static int run_until_tick(struct boot *b, uint64_t target, size_t from)
{
	static const char *const ends[] = { DONE_MARKER, FINAL_MARKER };
	double t0 = now_s();
	for (;;) {
		struct hvx_pmu pm;
		struct hvx_run_ctl c;
		int i;

		memset(&c, 0, sizeof(c));
		c.max_insns = g_budget;
		if (ioctl(b->fd, HVX_RUNC, &c) < 0) {
			if (errno == EINTR) continue;
			fprintf(stderr, "hvx_explore: HVX_RUNC: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);
		if (c.reason != HVX_EXIT_STEP && c.reason != HVX_EXIT_THROTTLE) {
			fprintf(stderr, "hvx_explore: guest stopped: reason=%u (%s)\n", c.reason, exit_reason_name(c.reason));
			return -1;
		}
		if (now_s() - t0 > RUN_WALL_SECS)
			return -2;
		for (i = 0; i < 2; i++)
			if (b->ulen > from && mem_find(b->ubuf + from, b->ulen - from, ends[i]) >= 0)
				return i + 1;
		if (!target) {
			struct hvx_unsync_io io;
			memset(&io, 0, sizeof(io));	/* len 0: only asks for the count (ENOSPC if any) */
			ioctl(b->fd, HVX_GET_UNSYNC, &io);
			if (io.n) return 0;
			continue;
		}
		memset(&pm, 0, sizeof(pm));
		if (ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) return -1;
		if (pm.next / pm.period >= target) return 0;
	}
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
	if (sc->tj_on || sc->rb || sc->pp || sc->pptsc || sc->xt_on || sc->q_on) {
		snprintf(verdict, sizeof(verdict), "ERROR branch: not for timed or rb= schedules");
		goto fail;
	}
	if (set_fault(b, sc->dseed, sc->classes, sc->prob)) { snprintf(verdict, sizeof(verdict), "ERROR harness set_fault"); goto fail; }
	snprintf(line, sizeof(line), "%s\n", sc->guest);
	if (put_input(b, line)) { snprintf(verdict, sizeof(verdict), "ERROR harness put_input"); goto fail; }
	if (mark[0] == '@') {		/* N ticks into the workload */
		memset(&pm, 0, sizeof(pm));
		if (ioctl(b->fd, HVX_GET_PMU, &pm) < 0 || !pm.period) { snprintf(verdict, sizeof(verdict), "ERROR branch needs the PMU clock"); goto fail; }
		r = run_until_tick(b, strcmp(mark, "@held") ?
				   pm.next / pm.period + strtoull(mark + 1, NULL, 10) : 0, from);
	} else {
		r = run_until_any(b, mk, 3, from);
	}
	if (r < 0) { snprintf(verdict, sizeof(verdict), "ERROR vm workload (before the branch point)"); goto fail; }
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
	printf("BRANCHPOINT %s tjlo=%llu held=%u\n", tag, (unsigned long long)lo, s2.unsync_n);
	fflush(stdout);
	if (nvar < 0) {			/* the caller already has the base run */
		boot_teardown(b);
		nvar = -nvar;
	} else {
		finish_sched(b, sub, sc, s0, from, t0, "");
	}
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

/* RESUME <tag> <snapshot>: continue a controlled run from a SNAP in a fresh VM (see the header). */
static void ctl_resume(const char *tag, const char *name, struct snap *s0)
{
	struct ctl_snap *cs = NULL;
	struct boot b;
	double t0 = now_s();
	int i;
	for (i = 0; i < CTL_MAX_SNAPS; i++)
		if (g_ctl_snaps[i] && !strcmp(g_ctl_snaps[i]->name, name)) cs = g_ctl_snaps[i];
	if (!cs) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR no snapshot %s\n", tag, name); return; }
	memset(&b, 0, sizeof(b));
	b.fd = -1;
	if (vm_fresh(&b) || snap_restore(&b, &cs->sn, NULL)) {
		printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR harness restore\n", tag);
		boot_teardown(&b);
		return;
	}
	ctl_begin(tag);
	if (ctl_book_copy(&C.k, &cs->k)) {
		C.on = 0;
		sched_fail(&b, tag, cs->from, t0, "", "ERROR harness resume (out of memory)");
		return;
	}
	C.start = 2;			/* the first stop says why=resume */
	g_budget = cs->sc.rb;
	finish_sched(&b, tag, &cs->sc, s0, cs->from, t0, cs->tjinfo);
	g_budget = 0;
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
	double pt0 = now_s(), pt1, pt2, pt3, pt4;
	struct ph_ctr bc0, bc1, bc2;
	if (boot_setup(b, E.mem_size, E.image, E.image_len, E.initrd, E.initrd_len,
		       E.base, E.disk_len, cmd, 0, HVX_FAULT_DROP_UNFLUSHED, 0) != 0) return -1;
	pt1 = now_s();
	if (ph_on()) ph_read(b, &bc0);
	if (run_until_any(b, m0, 2, 0) != 0) { fprintf(stderr, "hvx_explore: no S0 marker\n"); console_dump(b); return -1; }
	pt2 = now_s();
	if (ph_on()) ph_read(b, &bc1);
	if (snap_take(b, s0, "s0")) return -1;
	pt3 = now_s();
	fprintf(stderr, "hvx_explore: S0 at %zu console bytes, state %016llx\n", b->ulen, (unsigned long long)fingerprint(b));
	if (vcpustat_on()) {
		long at = mem_find(b->ubuf, b->ulen, "smp: Brought up");
		size_t i = 0;
		char ln[160];
		while (at >= 0 && (size_t)at + i < b->ulen && b->ubuf[at + i] != '\n' && i + 1 < sizeof(ln)) {
			ln[i] = (char)b->ubuf[at + i];
			i++;
		}
		ln[i] = 0;
		fprintf(stderr, "VCPUSTAT boot: %s\n", at >= 0 ? ln : "no \"smp: Brought up\" line in the console");
	}
	if (put_input(b, "provision\n")) return -1;
	double pt3b = now_s();	/* pt3b - pt3: the S0 fingerprint printed above */
	if (run_until_any(b, m1, 2, s0->ulen) != 0) { fprintf(stderr, "hvx_explore: no S1 marker\n"); console_dump(b); return -1; }
	pt4 = now_s();
	if (ph_on()) ph_read(b, &bc2);
	fprintf(stderr, "hvx_explore: S1 at %zu console bytes, state %016llx\n", b->ulen, (unsigned long long)fingerprint(b));
	if (s1) {
		double t5 = now_s();
		if (snap_take(b, s1, "s1")) return -1;
		if (ph_on()) {
			uint64_t off, nz1 = 0, dnz = 0;
			static const uint8_t zero[4096];
			double t6 = now_s();
			for (off = 0; off + 4096 <= E.mem_size; off += 4096)
				if (memcmp(b->mem + off, zero, 4096)) nz1++;
			for (off = 0; off + 4096 <= E.p2_len; off += 4096)
				if (memcmp(b->disk_mm + E.p2_off + off, zero, 4096)) dnz++;
			g_ph_extra_n = 0; g_ph_extra[0] = 0;
			ph_delta("s0", &bc0, &bc1);
			ph_delta("s1", &bc1, &bc2);
			fprintf(stderr, "PHBOOT setup=%.3f to_s0=%.3f snap_s0=%.3f fp_s0=%.3f to_s1=%.3f fp_s1=%.3f snap_s1=%.3f "
				"mem=%llu disk=%llu p1=%llu p2=%llu snapstruct=%zu s1_ram_nonzero_pages=%llu s1_p2_nonzero_pages=%llu "
				"con_s0=%zu con_s1=%zu rss_kb=%ld rss_shmem_kb=%ld%s\n",
				pt1 - pt0, pt2 - pt1, pt3 - pt2, pt3b - pt3, pt4 - pt3b, t5 - pt4, t6 - t5,
				(unsigned long long)E.mem_size, (unsigned long long)E.disk_len,
				(unsigned long long)E.p1_len, (unsigned long long)E.p2_len, sizeof(struct hvx_snapshot),
				(unsigned long long)nz1, (unsigned long long)dnz, s0->ulen, b->ulen,
				ph_rss_kb("VmRSS:"), ph_rss_kb("RssShmem:"), g_ph_extra);
			g_ph_extra_n = 0; g_ph_extra[0] = 0;
		}
	} else if (ph_on()) {
		fprintf(stderr, "PHBOOT setup=%.3f to_s0=%.3f snap_s0=%.3f fp_s0=%.3f to_s1=%.3f\n",
			pt1 - pt0, pt2 - pt1, pt3 - pt2, pt3b - pt3, pt4 - pt3b);
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

/* Guest RAM in MiB from "--ram-mib N" or HVX_EXPLORE_RAM_MIB; a whole number in the bounds. */
static int ram_mib_parse(const char *v, const char *what, unsigned long long *out)
{
	char *end;
	unsigned long long n = strtoull(v, &end, 10);
	if (!*v || *end || n < EXP_MEM_MIN_MIB || n > EXP_MEM_MAX_MIB) {
		fprintf(stderr, "hvx_explore: %s=%s: guest RAM must be a whole number of MiB in [%llu, %llu]\n",
			what, v, EXP_MEM_MIN_MIB, EXP_MEM_MAX_MIB);
		return -1;
	}
	*out = n;
	return 0;
}

/* "--vcpus N" / HVX_EXPLORE_VCPUS: N in [1, HVX_SCHED_MAX_VCPUS] */
static int vcpus_parse(const char *v, const char *what)
{
	char *end;
	unsigned long n = strtoul(v, &end, 10);
	if (!*v || *end || n < 1 || n > HVX_SCHED_MAX_VCPUS) {
		fprintf(stderr, "hvx_explore: %s=%s: the vCPU count must be a whole number in [1, %d]\n",
			what, v, HVX_SCHED_MAX_VCPUS);
		return -1;
	}
	g_vcpus = (unsigned int)n;
	return 0;
}

int main(int argc, char **argv)
{
	const char *mode;
	struct boot b, geom;
	struct snap s0, s1;
	size_t dlen = 0;
	double t0 = now_s();
	unsigned long long ram_mib = EXP_MEM_MIB;

	if (getenv("HVX_EXPLORE_RAM_MIB") && ram_mib_parse(getenv("HVX_EXPLORE_RAM_MIB"), "HVX_EXPLORE_RAM_MIB", &ram_mib))
		return 2;
	if (getenv("HVX_EXPLORE_VCPUS") && vcpus_parse(getenv("HVX_EXPLORE_VCPUS"), "HVX_EXPLORE_VCPUS"))
		return 2;
	while (argc > 2 && (!strcmp(argv[1], "--ram-mib") || !strcmp(argv[1], "--vcpus") || !strcmp(argv[1], "--cpu") ||
			    !strcmp(argv[1], "--control"))) {
		if (!strcmp(argv[1], "--control")) {	/* lock-step control of `one` (M118) */
			g_ctl_one = 1;
			argv[1] = argv[0];
			argv++; argc--;
			continue;
		}
		if (!strcmp(argv[1], "--ram-mib")) {	/* explore.py: the manifest's guest.ram_mib */
			if (ram_mib_parse(argv[2], "--ram-mib", &ram_mib)) return 2;
		} else if (!strcmp(argv[1], "--cpu")) {	/* explore.py: the manifest's guest.cpu */
			if (!strcmp(argv[2], "v3"))
				setenv("HVX_CPU_V3", "1", 1);	/* guest AVX/AVX2 (HVX_VM_X86_64_V3) */
			else if (strcmp(argv[2], "v2")) {
				fprintf(stderr, "hvx_explore: --cpu %s: expected v2 or v3\n", argv[2]);
				return 2;
			}
		} else if (vcpus_parse(argv[2], "--vcpus")) {	/* explore.py --vcpus N (N > 1 only) */
			return 2;
		}
		argv[2] = argv[0];
		argv += 2; argc -= 2;
	}
	if (getenv("HVX_VCPU_PARK")) g_vcpu_park = strcmp(getenv("HVX_VCPU_PARK"), "0") != 0;
	if (getenv("HVX_CPU_V3") && strcmp(getenv("HVX_CPU_V3"), "0"))
		fprintf(stderr, "hvx_explore: guest CPU x86-64-v3 (AVX/AVX2)\n");
	if (argc < 6) {
		fprintf(stderr, "usage: %s [--ram-mib N] [--vcpus N] [--cpu v2|v3] [--control] serve|one <bzImage> <initramfs> <explore-disk.img> <outdir> [\"<schedule>\"]\n", argv[0]);
		return 2;
	}
	mode = argv[1];
	g_outdir = argv[5];
	mkdir(g_outdir, 0755);
	memset(&E, 0, sizeof(E));
	E.mem_size = ram_mib << 20;
	if (ram_mib != EXP_MEM_MIB) fprintf(stderr, "hvx_explore: guest RAM %llu MiB\n", ram_mib);
	if (g_vcpus > 1)
		fprintf(stderr, "hvx_explore: %u vCPUs in in-kernel turns (HVX_VM_SMP_SCHED)%s\n", g_vcpus,
			g_vcpu_park ? "; vCPUs 1.. parked (halted, IF=0) until the guest can start them" : "");
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
		if (getenv("HVX_EXPLORE_BOOTCON")) save_console(&b, 0, "boot", "");	/* diagnostic: the boot console */
		fprintf(stderr, "hvx_explore: provisioned in %.1f s (fresh boot, no snapshot restore)\n", now_s() - t0);
		if (ph_on()) ph_reset();
		if (g_ctl_one) ctl_begin("one");
		run_sched(&b, "one", &sc, &s0);
		return 0;
	}
	if (g_ctl_one) { fprintf(stderr, "hvx_explore: --control is for `one`; serve takes CTL requests\n"); return 2; }
	if (strcmp(mode, "serve")) { fprintf(stderr, "hvx_explore: unknown mode %s\n", mode); return 2; }
	if (boot_to_s1(&b, &s0, &s1)) return 1;
	{
		double pht = PH_T();
		boot_teardown(&b);
		if (ph_on())
			fprintf(stderr, "PHBOOT teardown=%.3f total=%.3f\n", now_s() - pht, now_s() - t0);
	}
	g_ph_s1 = &s1;
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
			if (!strncmp(in, "RESUME ", 7)) {	/* lock-step (M118) */
				char name[64];
				if (sscanf(in, "RESUME %63s %63s", tag, name) != 2) {
					printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n");
					continue;
				}
				if (ph_on()) ph_reset();
				ctl_resume(tag, name, &s0);
				continue;
			}
			{
				int ctl = !strncmp(in, "CTL ", 4);
				const char *rq = in + (ctl ? 4 : 0);
				if (sscanf(rq, ctl ? "%63s %n" : "RUN %63s %n", tag, &off) != 1 || off == 0 ||
				    parse_sched(rq + off, &sc)) {
					printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n");
					continue;
				}
				if (ctl) ctl_begin(tag);
			}
			if (ph_on()) ph_reset();
			{
				double pht = PH_T();
				int bad = vm_fresh(&b);
				PH_ADD(PH_FRESH, pht);
				g_ph_rsplit = 1;
				bad = bad || snap_restore(&b, &s1, NULL);
				g_ph_rsplit = 0;
				if (bad) {
					printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR harness restore\n", tag);
					C.on = 0;
					boot_teardown(&b);
					continue;
				}
			}
			run_sched(&b, tag, &sc, &s0);
		}
	}
	return 0;
}

/*
 * hvmx -- the explorer's run engine on the Mac (Hypervisor.framework), the counterpart of
 * hv/hvx/hvx_explore.c for the deterministic arm64 VMM in ../hvm.c (Milestone 48).
 *
 * It speaks hvx_explore's protocol, so hydo/explore/explore.py drives it unchanged apart from
 * the command line (explore.py --backend hvm). The guest is a manifest-built arm64 initramfs
 * (hydo/onboard/hydo-build --arch arm64) whose /sbin/init is the generic hydo/onboard/hydo-init.sh:
 *
 *   S0  "hydo-explore-ready-S0"   nothing started yet           -> start of a recovery boot
 *   S1  "hydo-explore-ready-S1"   services up, provisioned      -> start of every schedule
 *
 * The engine boots once, snapshots S0 and S1 (vCPU registers, the machine/device/clock state,
 * and all guest RAM in a file-backed buffer), then for every schedule restores S1 into a FRESH
 * vCPU, types the schedule's guest part on the serial console, runs to
 * "hydo-explore-workload-done", types "go", and runs to the final verdict.
 *
 *   hvmx [--ram-mib N] serve <Image> <initramfs> <outdir>         "RUN <tag> <schedule>" on stdin -> RESULT lines
 *   hvmx [--ram-mib N] one <Image> <initramfs> <outdir> "<schedule>"
 *        one schedule from a fresh boot, no snapshot restore: its fingerprint must equal serve's
 *
 * With the data disk attached (HVM_DISK=<NAME>-arm64-disk.img, the virtio-blk device of
 * ../virtio.c) the host part works as on hvx_explore: disk=none|ioerr:P|cut|lying:P with dseed
 * arms the same seeded fault classes (same seed -> same per-request decisions), and a power
 * cut (C=) carries only the flushed disk bytes into a recovery boot restored from S0, which
 * gets "oracle <acked ids> [failed <ids>]". The disk boots with the flush-aware class armed at
 * probability 0, so the guest negotiates FLUSH (write-back caching) like hvx's guests.
 * Forced preemption points (pct=<hex seed> pctn=<points> [pctk=<exits>, default 20000]): pctn
 * seeded exits among the first pctk exits of the workload make the timer due at once (../hvm.c
 * PCT); the RESULT gains pctf=<points that fired> pctw=<exits the workload took>.
 * Tick jitter (tj=, tjx=) delays timer interrupts by a seeded number of EXITS (../hvm.c TJ);
 * the RESULT gains tjk=<first>-<last> tjn=<n> as on hvx. Not supported (the RESULT says UNSUPPORTED):
 * disk faults or cuts without a disk, network faults (N=, O=) on an image without tc
 * (initrd_has_tc: its compiled manifest, etc/hydo/manifest.sh, has an empty HYDO_TC), and the
 * host tokens only hvx_explore applies (rb=, pp=, xt*, q*, cf=, cfn; host_token_problem). A host
 * token no engine knows is "ERROR bad schedule". The explorer (hydo-explore, --backend hvm)
 * strips the unsupported ones from generated schedules up front (Ex::hvm_supported, Ex::render).
 *
 * Exit-free spin loops (../spin_observer.c, ../spin/RESEARCH.md): exact-repeat loops are
 * resolved in every run. Counter loops need the rewind mode (a checkpoint at every exit, several
 * times slower), chosen per schedule: HVM_SPIN_REWIND=auto (the default here) runs a schedule
 * plainly and, only if that ends as SPIN, runs it again from S1 with rewind on, and reports that
 * second run; =1 always rewinds, =0 never. A host token "spin=rewind" forces it for one schedule.
 * Reported results stay a pure function of the schedule: rewind mode only adds the checkpoints
 * (invisible to the guest) and the counter-loop resolution, so a schedule that completes plainly
 * gives the same fingerprint with rewind on, and one that does not complete plainly is never
 * reported from its plain run. The RESULT line keeps explore.py's format; stderr says when the
 * reported run used rewind mode.
 *
 * Guest RAM: "--ram-mib N" before the mode, else HVM_MEM_MB, else 1024 MiB; 256..4096.
 * Env: HVM_MEM_MB, HVM_SNAP_DIR (default /tmp/hvm-snap),
 *      HVM_RUN_EXITS (per-phase exit budget, default 50,000,000), HVM_DISK, plus ../hvm.c's knobs
 *      (HVM_SPIN_MS, HVM_SPIN_SECS, HVM_SPIN_OBSERVE, HVM_SPLOG).
 */
#define HVM_NO_MAIN
#include "../hvm.c"
#include "../virtio_int.h"
#include <errno.h>
#include <libkern/OSCacheControl.h>
#include <zlib.h>

#define S0_MARKER    "hydo-explore-ready-S0"
#define S1_MARKER    "hydo-explore-ready-S1"
#define DONE_MARKER  "hydo-explore-workload-done"
#define FINAL_MARKER "hydo-stack-marker: workload complete"
#define ACKED_PREFIX  "hydo-explore-acked:"
#define FAILED_PREFIX "hydo-explore-failed:"

static const char *outdir = ".";
static int spin_auto;          /* HVM_SPIN_REWIND=auto (default): escalate a SPIN run to rewind mode */
static uint64_t run_exits = 50000000ULL;

/* ---------------- stop at the first exit after any of several markers ---------------- */
static const char *hk_m[4];
static int hk_n, hk_hit;
static size_t hk_from, hk_scan;
static int marker_hook(void)
{
	size_t len = M.console_len;
	for (int i = 0; i < hk_n; i++) {
		size_t ml = strlen(hk_m[i]), st = hk_scan > ml ? hk_scan - ml : 0;
		if (st < hk_from) st = hk_from;
		if (len >= st + ml && memmem(console + st, len - st, hk_m[i], ml)) { hk_hit = i; return 1; }
	}
	hk_scan = len;
	return 0;
}
/* returns the index of the marker reached, or -1 (and the stop reason in *why) */
static int run_to(const char *const *m, int n, size_t from, int *why)
{
	for (int i = 0; i < n; i++) hk_m[i] = m[i];
	hk_n = n; hk_hit = -1; hk_from = hk_scan = from;
	hvm_stop_hook = marker_hook;
	max_exits = M.exits + run_exits;
	int s = run_until(NULL);
	hvm_stop_hook = NULL;
	if (why) *why = s;
	return s == STOP_MARKER ? hk_hit : -1;
}

/* ---------------- fingerprint: console + registers + machine + all RAM ---------------- */
static uint64_t mix64(const void *p, size_t n, uint64_t h)  /* word-wise FNV-style; RAM is 1 GiB */
{
	const uint64_t *w = p; size_t k = n / 8;
	for (size_t i = 0; i < k; i++) { h ^= w[i]; h *= 0x100000001b3ULL; h ^= h >> 29; }
	return fnv((const uint8_t *)p + 8 * k, n - 8 * k, h);
}
static uint64_t xfp(void)
{
	struct vsnap s; vtake(&s);
	struct mach m = M;
	m.n_wfi = m.n_wfi_jump = m.n_cntpct = m.n_mmio = m.n_hvc = m.n_sysreg_other = m.n_irq_line = m.n_steps = m.n_step_runs = 0;
	uint64_t h = fnv("hvm-explore-v1", 14, 0xcbf29ce484222325ULL);
	h = fnv(&M.console_len, 8, h); h = fnv(console, M.console_len, h);
	h = fnv(s.x, sizeof s.x, h); h = fnv(&s.pc, 8, h); h = fnv(&s.cpsr, 8, h); h = fnv(&s.fpcr, 8, h); h = fnv(&s.fpsr, 8, h);
	h = fnv(s.q, sizeof s.q, h); h = fnv(s.sr, sizeof s.sr, h); h = fnv(s.ok, sizeof s.ok, h);
	h = fnv(&m, sizeof m, h);
	h = fnv(&ram_size, 8, h);
	if (virtio_attached()) h = virtio_hash(h);   /* device state, disk image, volatile cache */
	return mix64(ram, ram_size, h);
}

/* ---------------- snapshots: RAM in an unlinked file, mapped ---------------- */
struct xsnap { struct vsnap v; struct mach m; uint8_t *ram; void *vio; char *con; int have; };
static void xsnap_take(struct xsnap *s, const char *name)
{
	if (!s->ram) {
		const char *d = getenv("HVM_SNAP_DIR") ? getenv("HVM_SNAP_DIR") : "/tmp/hvm-snap";
		char path[600];
		mkdir(d, 0700);
		snprintf(path, sizeof path, "%s/%s.%d", d, name, (int)getpid());
		int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
		if (fd < 0 || ftruncate(fd, ram_size)) { perror(path); exit(1); }
		s->ram = mmap(NULL, ram_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (s->ram == MAP_FAILED) { perror("mmap snapshot"); exit(1); }
		close(fd); unlink(path);   /* the mapping keeps it; nothing lingers after exit */
	}
	vtake(&s->v); s->m = M;
	memcpy(s->ram, ram, ram_size);
	if (virtio_attached()) s->vio = virtio_save();   /* taken once per snapshot point */
	s->con = malloc(M.console_len + 1);            /* the console text is part of every fingerprint, */
	memcpy(s->con, console, M.console_len);        /* and a recovery boot from S0 overwrites what S1 had */
	s->have = 1;
}
static void xsnap_restore(const struct xsnap *s)
{
	/* A fresh VM, not just a fresh vCPU, so no translation cached during the previous run
	 * (whose guest page tables differ from the restored ones) can be reused. Defensive: it did
	 * not remove the rare divergence described in README.md (Open). */
	CHK(hv_vcpu_destroy(vcpu));
	CHK(hv_vm_destroy());
	CHK(hv_vm_create(NULL));
	CHK(hv_vm_map(ram, RAM_BASE, ram_size, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	CHK(hv_vcpu_create(&vcpu, &ex, NULL)); trap_dbg_applied = -1;
	spin_reset();                  /* nothing armed at a snapshot point; the checkpoint is retaken */
	memcpy(ram, s->ram, ram_size);
	/* The host wrote guest code through the data cache; make sure no stale instruction of the
	 * previous run can be fetched. Defensive as well (README.md, Open). */
	sys_icache_invalidate(ram, ram_size);
	vput(&s->v); M = s->m;
	memcpy(console, s->con, M.console_len);
	if (s->vio) virtio_restore(s->vio);
	rx_head = rx_tail = 0;
	if (getenv("HVMX_CHECK_RESTORE")) {   /* diagnostic: read the registers back and compare */
		struct vsnap r; vtake(&r);
		for (int i = 0; i < 31; i++) if (r.x[i] != s->v.x[i]) fprintf(stderr, "hvmx: restore: x%d %llx != %llx\n", i, r.x[i], s->v.x[i]);
		if (r.pc != s->v.pc || r.cpsr != s->v.cpsr || r.fpcr != s->v.fpcr || r.fpsr != s->v.fpsr) fprintf(stderr, "hvmx: restore: pc/cpsr/fpcr/fpsr differ\n");
		if (memcmp(r.q, s->v.q, sizeof r.q)) fprintf(stderr, "hvmx: restore: SIMD registers differ\n");
		for (size_t i = 0; i < NSR; i++) if (r.ok[i] != s->v.ok[i] || r.sr[i] != s->v.sr[i])
			fprintf(stderr, "hvmx: restore: sysreg %zu (0x%x) ok %d/%d %llx != %llx\n", i, SR[i], r.ok[i], s->v.ok[i], r.sr[i], s->v.sr[i]);
	}
}
/* HVMX_DIAG=1: per-component hashes of the end state into <outdir>/<tag>.diag (to locate a divergence) */
static void diag(const char *tag, const char *phase)
{
	if (!getenv("HVMX_DIAG")) return;
	char path[700]; snprintf(path, sizeof path, "%s/%s.diag", outdir, tag);
	FILE *f = fopen(path, "a"); if (!f) return;
	struct vsnap s; vtake(&s);
	fprintf(f, "%s console %016llx len %llu\n", phase, fnv(console, M.console_len, 0xcbf29ce484222325ULL), M.console_len);
	for (int i = 0; i < 31; i++) fprintf(f, "%s x%d %016llx\n", phase, i, s.x[i]);
	fprintf(f, "%s pc %016llx cpsr %016llx fpcr %llx fpsr %llx\n", phase, s.pc, s.cpsr, s.fpcr, s.fpsr);
	for (int i = 0; i < 32; i++) fprintf(f, "%s q%d %016llx\n", phase, i, fnv(&s.q[i], 16, 0xcbf29ce484222325ULL));
	for (size_t i = 0; i < NSR; i++) fprintf(f, "%s sr%zu %d %016llx\n", phase, i, s.ok[i], s.sr[i]);
	struct mach m = M; m.n_wfi = m.n_wfi_jump = m.n_cntpct = m.n_mmio = m.n_hvc = m.n_sysreg_other = m.n_irq_line = m.n_steps = m.n_step_runs = 0;
	fprintf(f, "%s mach %016llx exits %llu vtime %llu\n", phase, fnv(&m, sizeof m, 0xcbf29ce484222325ULL), M.exits, M.vtime);
	for (uint64_t o = 0; o < ram_size; o += 1 << 20) fprintf(f, "%s ram@%08llx %016llx\n", phase, o, mix64(ram + o, 1 << 20, 0xcbf29ce484222325ULL));
	fclose(f);
	if (getenv("HVMX_DIAG_CHUNKS")) {   /* raw copies of chosen 1 MiB chunks of RAM */
		char *l = strdup(getenv("HVMX_DIAG_CHUNKS")), *t;
		for (t = strtok(l, ","); t; t = strtok(NULL, ",")) {
			uint64_t o = strtoull(t, 0, 0);
			snprintf(path, sizeof path, "%s/%s.%s.%llx.bin", outdir, tag, phase, o);
			FILE *g = fopen(path, "wb"); if (g) { fwrite(ram + o, 1, 1 << 20, g); fclose(g); }
		}
		free(l);
	}
}

/* ---------------- one schedule ---------------- */
static void save_log(const char *tag, size_t from)
{
	char path[700];
	snprintf(path, sizeof path, "%s/%s.log", outdir, tag);
	FILE *f = fopen(path, "a");
	if (!f) return;
	if (M.console_len > from) fwrite(console + from, 1, M.console_len - from, f);
	fclose(f);
}
static void last_verdict(size_t from, char *out, size_t n)
{
	long at = -1;
	for (long i = (long)from; i + 12 <= (long)M.console_len; i++)
		if (!memcmp(console + i, "HYDO-ORACLE:", 12)) at = i;
	if (at < 0) { snprintf(out, n, "ERROR no verdict"); return; }
	size_t i = 0;
	for (; at + i < M.console_len && console[at + i] != '\n' && console[at + i] != '\r' && i + 1 < n; i++) out[i] = console[at + i];
	out[i] = 0;
}
static const char *stop_name(int s)
{
	return s == STOP_HALT ? "guest halted" : s == STOP_IDLE ? "guest idle with no timer" :
	       s == STOP_LIMIT ? "exit budget exhausted" : s == STOP_SPIN ? "SPIN (guest loops without exits; run invalid)" : "?";
}
/* hvx's HVX_SET_FAULT on the virtio disk: new policy, counters from 0, volatile cache emptied */
static void set_fault(uint64_t seed, uint32_t classes, uint32_t prob)
{
	if (blk_idx < 0) return;
	V.blk.seed = seed; V.blk.classes = classes; V.blk.prob = prob; V.blk.max_delay = 0;
	V.blk.req_seq = V.blk.kick_seq = V.blk.fault_count = 0;
	blk_reset();
}
/* the host part's disk policy, as hvx_explore's parse_sched; -1 if it is not one */
static int parse_disk(const char *host, uint64_t *seed, uint32_t *classes, uint32_t *prob)
{
	char d[64] = "none"; const char *p;
	*seed = 0; *classes = 0; *prob = 0;
	if ((p = strstr(host, "disk="))) sscanf(p + 5, "%63s", d);
	if ((p = strstr(host, "dseed="))) *seed = strtoull(p + 6, NULL, 16);
	if (!strcmp(d, "none")) return 0;
	if (!strncmp(d, "ioerr:", 6)) { *classes = F_ERROR; *prob = atoi(d + 6); return 0; }
	if (!strcmp(d, "cut")) { *classes = F_UNFLUSHED; *prob = 100; return 0; }
	if (!strncmp(d, "lying:", 6)) { *classes = F_DROP; *prob = atoi(d + 6); return 0; }
	return -1;
}
/* the host part's tick jitter, as hvx_explore's parse_tj; delays are counted in exits here.
 * Sets TJ (../hvm.c) but leaves it off: run_sched turns it on for the workload. -1 if malformed. */
static int parse_tj(const char *host, struct tjit *t)
{
	const char *p;
	memset(t, 0, sizeof *t);
	if ((p = strstr(host, "tjx="))) {
		char *end;
		p += 4;
		while (*p && *p != ' ' && t->n_explicit < TJ_MAX_EXPLICIT) {
			t->ek[t->n_explicit] = strtoull(p, &end, 10);
			if (*end != ':') return -1;
			t->ed[t->n_explicit] = (uint32_t)strtoul(end + 1, &end, 10);
			t->n_explicit++;
			p = (*end == ',') ? end + 1 : end;
		}
		if (*p && *p != ' ') return -1;
		return 0;
	}
	if (!(p = strstr(host, "tj="))) return 0;
	t->seed = strtoull(p + 3, NULL, 16);
	t->permille = (p = strstr(host, "tjpm=")) ? (uint32_t)strtoul(p + 5, NULL, 10) : 10;
	t->max_delay = (p = strstr(host, "tjmax=")) ? (uint32_t)strtoul(p + 6, NULL, 10) : 1000;
	if ((p = strstr(host, "tjlo="))) t->lo = strtoull(p + 5, NULL, 10);
	if ((p = strstr(host, "tjhi="))) t->hi = strtoull(p + 5, NULL, 10);
	if (!t->permille || !t->max_delay) memset(t, 0, sizeof *t);
	return 0;
}
static int tj_wanted(const struct tjit *t) { return t->n_explicit || (t->permille && t->max_delay); }
/* Does the (gzip'd cpio) initramfs carry a tc for hydo-init.sh's network faults? Its compiled
 * manifest (etc/hydo/manifest.sh, hydo/onboard/hydo-build) says HYDO_TC='<path>' or HYDO_TC=''. */
static int initrd_has_tc;
static int scan_has_tc(const uint8_t *gz, size_t len)
{
	static const char key[] = "\nHYDO_TC='/", none[] = "\nHYDO_TC=''";
	z_stream z = { 0 };
	uint8_t buf[1 << 16], win[sizeof buf + 16];
	size_t keep = 0;
	int found = 0, r;
	if (inflateInit2(&z, 16 + MAX_WBITS) != Z_OK) return 0;
	z.next_in = (Bytef *)gz; z.avail_in = (uInt)len;
	do {
		z.next_out = buf; z.avail_out = sizeof buf;
		r = inflate(&z, Z_NO_FLUSH);
		size_t n = sizeof buf - z.avail_out;
		memcpy(win + keep, buf, n);                 /* the last 15 bytes carry over: a match may span chunks */
		if (memmem(win, keep + n, key, sizeof key - 1)) { found = 1; break; }
		if (memmem(win, keep + n, none, sizeof none - 1)) break;
		size_t total = keep + n;
		keep = total < 15 ? total : 15;
		memmove(win, win + total - keep, keep);
	} while (r == Z_OK);
	inflateEnd(&z);
	return found;
}
/* The host tokens this engine applies: disk=, dseed=, the tick jitter (tj= tjpm= tjmax= tjlo= tjhi=,
 * tjx=), the forced preemption points (pct= pctn= pctk=) and spin=rewind. Any other host token is
 * refused instead of ignored: hvx's PMU-clock tokens (rb= pp= pptsc= xt*, the vCPU turns q= qs= qb=
 * qbs= qm=) and the power cut at the k-th disk FLUSH (cf=, cfn) need hvx_explore; before M102 a
 * cf= was silently dropped here and the explorer's report claimed a power cut that never happened.
 * Returns why (in buf), or NULL; *unknown says whether the token is unknown to every engine. */
static const char *host_token_problem(const char *host, char *buf, size_t n, int *unknown)
{
	static const char *const ok[] = { "disk=", "dseed=", "tj=", "tjpm=", "tjmax=", "tjlo=", "tjhi=", "tjx=",
	                                   "pct=", "pctn=", "pctk=", NULL };
	static const char *const hvx_only[] = { "rb=", "pp=", "pptsc=", "xt=", "xtr=", "xtpm=", "xtlo=", "xthi=",
	                                        "xtsc=", "xtx=", "cf=", "q=", "qs=", "qb=", "qbs=", "qm=", NULL };
	char words[1024], *save = NULL;
	snprintf(words, sizeof words, "%s", host);
	for (char *w = strtok_r(words, " \t\r\n", &save); w; w = strtok_r(NULL, " \t\r\n", &save)) {
		int good = !strcmp(w, "spin=rewind"), hvx = !strcmp(w, "cfn");
		for (int i = 0; !good && ok[i]; i++) good = !strncmp(w, ok[i], strlen(ok[i]));
		if (good) continue;
		for (int i = 0; !hvx && hvx_only[i]; i++) hvx = !strncmp(w, hvx_only[i], strlen(hvx_only[i]));
		*unknown = !hvx;
		snprintf(buf, n, hvx ? "host token %.60s needs hvx (the PMU clock, vCPU turns or a power cut at a disk FLUSH)"
		                     : "unknown host token %.60s", w);
		return buf;
	}
	return NULL;
}
/* why a schedule cannot run here, or NULL */
static const char *unsupported(const char *host, const char *guest)
{
	const char *d = strstr(host, "disk=");
	if (!virtio_attached() && d && strncmp(d, "disk=none", 9)) return "disk faults and power cuts need the data disk (HVM_DISK)";
	if (!virtio_attached() && strstr(guest, " C=")) return "power cuts need the data disk (HVM_DISK)";
	if (!initrd_has_tc && (strstr(guest, " N=") || strstr(guest, " O="))) return "network faults need tc on the arm64 root";
	return NULL;
}
static size_t append_line(size_t from, const char *prefix, char *out, size_t o, size_t n)
{
	size_t pl = strlen(prefix);
	char *at = M.console_len > from ? memmem(console + from, M.console_len - from, prefix, pl) : NULL;
	if (at)
		for (const char *p = at + pl; p < console + M.console_len && *p != '\n' && *p != '\r' && o + 2 < n; p++) out[o++] = *p;
	return o;
}
static void oracle_cmd(size_t from, char *out, size_t n)   /* as hvx_explore's */
{
	size_t o = (size_t)snprintf(out, n, "oracle");
	o = append_line(from, ACKED_PREFIX, out, o, n);
	if (M.console_len > from && memmem(console + from, M.console_len - from, FAILED_PREFIX, strlen(FAILED_PREFIX)) && o + 8 < n) {
		memcpy(out + o, " failed", 7); o += 7;
		o = append_line(from, FAILED_PREFIX, out, o, n);
	}
	out[o++] = '\n'; out[o] = 0;
}
static struct xsnap *g_s0;
static int spin_hit;           /* a phase of this run ended as SPIN (the run is invalid) */
static void run_once(const char *tag, const char *sched, int rewound)
{
	static const char *const wk[] = { DONE_MARKER, FINAL_MARKER };
	static const char *const fin[] = { FINAL_MARKER };
	char host[1024], guest[1024], verdict[300] = "", line[1100];
	double t0 = now_s();
	uint64_t wfp = 0, ffp = 0;
	const char *bar = strchr(sched, '|');
	if (!bar) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR bad schedule\n", tag); fflush(stdout); return; }
	snprintf(host, sizeof host, "%.*s", (int)(bar - sched), sched);
	snprintf(guest, sizeof guest, " %s", bar + 1);
	while (strlen(guest) && (guest[strlen(guest) - 1] == '\n' || guest[strlen(guest) - 1] == ' ')) guest[strlen(guest) - 1] = 0;
	char hw[200]; int unknown = 0;
	if (host_token_problem(host, hw, sizeof hw, &unknown)) {
		printf("RESULT %s fp=0 wfp=0 secs=0 verdict=%s%s\n", tag, unknown ? "ERROR bad schedule: " : "UNSUPPORTED on hvm: ", hw);
		fflush(stdout);
		return;
	}
	const char *u = unsupported(host, guest);
	if (u) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=UNSUPPORTED on hvm: %s\n", tag, u); fflush(stdout); return; }
	uint64_t dseed; uint32_t dcl, dpr;
	if (parse_disk(host, &dseed, &dcl, &dpr)) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR bad disk token\n", tag); fflush(stdout); return; }
	struct tjit tj;
	if (parse_tj(host, &tj)) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR bad tick-jitter token\n", tag); fflush(stdout); return; }
	char tjinfo[96] = "";
	const char *pp = strstr(host, "pct=");
	uint64_t pct_seed = pp ? strtoull(pp + 4, NULL, 16) : 0;
	uint32_t pct_n = (pp = strstr(host, "pctn=")) ? (uint32_t)strtoul(pp + 5, NULL, 10) : 0;
	uint64_t pct_k = (pp = strstr(host, "pctk=")) ? strtoull(pp + 5, NULL, 10) : 20000;
	if (!strstr(host, "pct=")) pct_n = 0;
	int cut = strstr(guest, " C=") != NULL;
	size_t from = M.console_len;
	int why = 0, r;
	/* the recovery boot's "oracle" command: 3584 bytes, as hvx_explore's, so that the fake outside
	 * world of a manifest with fakes:/proxies: (hydo-init-fakes.sh appends its records to the
	 * acknowledged ids at a power cut, at most 3000 bytes) fits; the UART input ring is 4096
	 * (../hvm.c rxq). A command that fitted before is unchanged. */
	char orc[3584];
	if (trace_f) fprintf(trace_f, "RUN %s\n", tag);
	set_fault(dseed, dcl, dpr);
	if (tj_wanted(&tj)) {            /* the jitter covers the workload only, like the disk fault */
		TJ = tj; TJ.on = 1;
		snprintf(tjinfo, sizeof tjinfo, " tjk=%llu", (unsigned long long)(M.vtime / TJ_PERIOD));
	}
	uint64_t wx0 = M.exits;
	if (pct_n) pct_set(pct_seed, pct_n, M.exits, M.exits + pct_k);   /* the workload only */
	snprintf(line, sizeof line, "%s\n", guest + 1);
	hvm_put_input(line);
	r = run_to(wk, 2, from, &why);
	if (pct_n) {
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof tjinfo - l, " pctf=%llu pctw=%llu", (unsigned long long)PCT.fired, (unsigned long long)(M.exits - wx0));
		memset(&PCT, 0, sizeof PCT);
	}
	if (TJ.on) {
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof tjinfo - l, "-%llu tjn=%llu",
		         (unsigned long long)(M.vtime / TJ_PERIOD), (unsigned long long)TJ.applied);
		memset(&TJ, 0, sizeof TJ);
	}
	if (r < 0) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR workload: %s", stop_name(why)); goto out; }
	wfp = xfp(); diag(tag, "w");
	if (r == 1) { last_verdict(from, verdict, sizeof verdict); ffp = wfp; goto out; }
	if (!cut) {                    /* same boot: faults off, heal, check */
		set_fault(0, 0, 0);
		hvm_put_input("go\n");
		r = run_to(fin, 1, from, &why);
		if (r < 0) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR check: %s", stop_name(why)); goto out; }
		ffp = xfp(); diag(tag, "f");
		last_verdict(from, verdict, sizeof verdict);
		goto out;
	}
	{                              /* power cut: only the flushed disk bytes survive; recover from S0 */
		oracle_cmd(from, orc, sizeof orc);
		uint8_t *durable = malloc(V.blk.disk_size);
		memcpy(durable, disk, V.blk.disk_size);   /* stable media; the volatile cache is lost */
		save_log(tag, from);
		xsnap_restore(g_s0);
		memcpy(disk, durable, V.blk.disk_size);
		free(durable);
		set_fault(0, 0, 0);
		from = M.console_len;
		hvm_put_input(orc);
		r = run_to(fin, 1, from, &why);
		if (r < 0) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR recovery: %s", stop_name(why)); goto out_rec; }
		ffp = xfp(); diag(tag, "f");
		last_verdict(from, verdict, sizeof verdict);
	out_rec:;
		char rt[80]; snprintf(rt, sizeof rt, "%s.recovery", tag);
		save_log(rt, from);
		goto done;
	}
out:
	save_log(tag, from);
done:
	if (spin_hit && !rewound && spin_auto) return;   /* not reported: run_sched re-runs it with rewind */
	if (rewound) fprintf(stderr, "hvmx: %s: reported from a rewind-mode run\n", tag);
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f%s verdict=%s\n", tag,
	       (unsigned long long)fnv(&ffp, 8, wfp), (unsigned long long)wfp, now_s() - t0, tjinfo, verdict);
	fflush(stdout);
}
/* one schedule from the restored S1 (serve) or from the provisioned boot (one); see the header */
static void run_sched(const char *tag, const char *sched, struct xsnap *s1)
{
	const char *bar = strchr(sched, '|');
	int forced = bar && memmem(sched, (size_t)(bar - sched), "spin=rewind", 11) != NULL;
	int base = rw_on;
	spin_hit = 0;
	if (forced && !rw_on) rw_on = 1;
	run_once(tag, sched, rw_on);
	if (spin_hit && !rw_on && spin_auto) {
		fprintf(stderr, "hvmx: %s ended as SPIN; running it again with rewind (counter loops)\n", tag);
		if (s1) xsnap_restore(s1); else { fprintf(stderr, "hvmx: no S1 snapshot to re-run from\n"); exit(3); }
		rw_on = 1; spin_hit = 0;
		run_once(tag, sched, 1);
	}
	rw_on = base;
}

static int boot_to_s1(struct xsnap *s0, struct xsnap *s1)
{
	static const char *const m0[] = { S0_MARKER, FINAL_MARKER };
	static const char *const m1[] = { S1_MARKER, FINAL_MARKER };
	int why = 0;
	if (run_to(m0, 2, 0, &why) != 0) { fprintf(stderr, "hvmx: no S0 marker (%s)\n", stop_name(why)); fwrite(console, 1, M.console_len, stderr); return -1; }
	if (s0) xsnap_take(s0, "s0");
	fprintf(stderr, "hvmx: S0 at %llu console bytes, exit %llu, state %016llx\n", M.console_len, M.exits, (unsigned long long)xfp());
	size_t f = M.console_len;
	hvm_put_input("provision\n");
	if (run_to(m1, 2, f, &why) != 0) { fprintf(stderr, "hvmx: no S1 marker (%s)\n", stop_name(why)); fwrite(console + f, 1, M.console_len - f, stderr); return -1; }
	fprintf(stderr, "hvmx: S1 at %llu console bytes, exit %llu, state %016llx\n", M.console_len, M.exits, (unsigned long long)xfp());
	if (s1) xsnap_take(s1, "s1");
	return 0;
}

/* Guest RAM in MiB: "--ram-mib N" before the mode (explore.py passes it from the manifest's
 * guest.ram_mib), else HVM_MEM_MB, else 1024. Bounds: the initramfs is loaded at 130 MiB, so
 * below 256 MiB a real image does not fit; 4096 is the largest size run on this backend (M48). */
#define HVMX_RAM_MIN_MIB 256
#define HVMX_RAM_MAX_MIB 4096
static int ram_mib_parse(const char *v, const char *what, uint64_t *out)
{
	char *end;
	unsigned long long n = strtoull(v, &end, 10);
	if (!*v || *end || n < HVMX_RAM_MIN_MIB || n > HVMX_RAM_MAX_MIB) {
		fprintf(stderr, "hvmx: %s=%s: guest RAM must be a whole number of MiB in [%d, %d]\n",
			what, v, HVMX_RAM_MIN_MIB, HVMX_RAM_MAX_MIB);
		return -1;
	}
	*out = n;
	return 0;
}

int main(int argc, char **argv)
{
	uint64_t ram_mib = 1024;
	if (getenv("HVM_MEM_MB") && ram_mib_parse(getenv("HVM_MEM_MB"), "HVM_MEM_MB", &ram_mib)) return 2;
	while (argc > 2 && !strcmp(argv[1], "--ram-mib")) {
		if (ram_mib_parse(argv[2], "--ram-mib", &ram_mib)) return 2;
		argv[2] = argv[0];
		argv += 2; argc -= 2;
	}
	if (argc < 5 || (strcmp(argv[1], "serve") && strcmp(argv[1], "one")) || (!strcmp(argv[1], "one") && argc < 6)) {
		fprintf(stderr, "usage: hvmx [--ram-mib N] serve <Image> <initramfs> <outdir>\n       hvmx [--ram-mib N] one <Image> <initramfs> <outdir> \"<schedule>\"\n");
		return 2;
	}
	const char *mode = argv[1];
	kernel_img = load_file(argv[2], &kernel_len);
	initrd_img = load_file(argv[3], &initrd_len);
	initrd_has_tc = scan_has_tc(initrd_img, initrd_len);
	outdir = argv[4];
	mkdir(outdir, 0755);
	ram_size = ram_mib << 20;
	if (ram_mib != 1024) fprintf(stderr, "hvmx: guest RAM %llu MiB\n", (unsigned long long)ram_mib);
	if (getenv("HVM_STEP")) STEP = strtoull(getenv("HVM_STEP"), 0, 0);
	if (getenv("HVM_RUN_EXITS")) run_exits = strtoull(getenv("HVM_RUN_EXITS"), 0, 0);
	spin_env();
	spin_auto = !getenv("HVM_SPIN_REWIND") || !strcmp(getenv("HVM_SPIN_REWIND"), "auto");
	echo = getenv("HVM_ECHO") != NULL;
	soft_irq = getenv("HVM_SOFT_IRQ") ? atoi(getenv("HVM_SOFT_IRQ")) : 1;   /* exact IRQ entry (see ../hvm.c) */
	if (getenv("HVM_TRACE")) trace_f = fopen(getenv("HVM_TRACE"), "w");
	setvbuf(stdout, NULL, _IOLBF, 0);
	char bootargs[1024];
	snprintf(bootargs, sizeof bootargs, "%s hydo_phase=serve", getenv("HVM_BOOTARGS") ? getenv("HVM_BOOTARGS") : BOOTARGS);
	{ pthread_t t; pthread_create(&t, NULL, watchdog, NULL); }
	if (getenv("HVM_DISK") && !getenv("HVM_FAULT")) {   /* negotiate FLUSH: flush-aware class at probability 0 */
		setenv("HVM_FAULT", "0", 1); setenv("HVM_FAULT_CLASSES", "unflushed", 1); setenv("HVM_FAULT_PROB", "0", 1);
	}
	virtio_setup();
	vm_create(bootargs);
	virtio_bind(ram, RAM_BASE, ram_size, STEP);
	/* Keep guest RAM resident, so a host page-out never becomes a hidden VM exit while the
	 * guest runs. Defensive (README.md, Open). */
	if (!getenv("HVMX_NO_MLOCK") && mlock(ram, ram_size)) perror("hvmx: mlock guest RAM (continuing)");
	vcpu_boot();
	double t0 = now_s();
	static struct xsnap s0, s1;
	g_s0 = &s0;
	if (!strcmp(mode, "one")) {
		if (boot_to_s1(&s0, spin_auto ? &s1 : NULL)) return 1;   /* S0 is still needed for a power cut's recovery boot */
		fprintf(stderr, "hvmx: provisioned in %.1f s (fresh boot, no snapshot restore)\n", now_s() - t0);
		run_sched("one", argv[5], &s1);   /* S1 only if a SPIN run must be repeated with rewind */
		return 0;
	}
	if (boot_to_s1(&s0, &s1)) return 1;
	printf("READY boot_secs=%.1f\n", now_s() - t0);
	char in[4096];
	while (fgets(in, sizeof in, stdin)) {
		char tag[64]; int off = 0;
		if (!strncmp(in, "QUIT", 4)) break;
		if (!strncmp(in, "BRANCH ", 7)) { printf("BRANCHEND ? unsupported on hvm\n"); fflush(stdout); continue; }
		if (sscanf(in, "RUN %63s %n", tag, &off) != 1 || !off) { printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n"); fflush(stdout); continue; }
		xsnap_restore(&s1);
		run_sched(tag, in + off, &s1);
	}
	return 0;
}

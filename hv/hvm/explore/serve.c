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
 *   serve also takes "CTL <tag> <schedule>" and "RESUME <tag> <snapshot>", and
 *   hvmx [--ram-mib N] --control one ... runs `one` the same way: lock-step control (M119), the
 *   protocol of hv/hvx/hvx_explore.c (see "lock-step control" below). Without them nothing changes.
 *   hvmx [--ram-mib N] --control boot serve|one ... boots an image whose first process is the
 *   guest agent (milestone 121, "controlled boot" below): no S0/S1 markers; the host takes them.
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
#include <ctype.h>
#include <errno.h>
#include <libkern/OSCacheControl.h>
#include <zlib.h>

#define S0_MARKER    "hydo-explore-ready-S0"
#define S1_MARKER    "hydo-explore-ready-S1"
#define DONE_MARKER  "hydo-explore-workload-done"
#define FINAL_MARKER "hydo-stack-marker: workload complete"
#define ACKED_PREFIX  "hydo-explore-acked:"
#define FAILED_PREFIX "hydo-explore-failed:"
#define AGENT_PREFIX  "HYDO-AG "   /* hydo-agent's framed lines: an agent image booted without --control boot */

static const char *outdir = ".";
static int spin_auto;          /* HVM_SPIN_REWIND=auto (default): escalate a SPIN run to rewind mode */
static uint64_t run_exits = 50000000ULL;

/* ---- lock-step control (M119; the protocol is hvx_explore's, see "lock-step control" below).
 * C.on is 0 unless a CTL/RESUME request or `--control one` asked for it; with it 0 none of the
 * lock-step code runs and every RUN takes exactly the path it took before. */
struct runst {                 /* a run from the workload's start on: what run_rest needs, and what a SNAP keeps */
	char tag[64]; double t0; size_t from; int cut, rewound; uint32_t pct_n; uint64_t wx0; char tjinfo[160];
};
#define CTL_MAX_SENDS 1024
#define CTL_MAX_SNAPS 8
#define CTL_SEND_MAX  4096     /* the UART receive queue (../hvm.c rxq) */
#define CTL_RB_DEFAULT 65536   /* the cadence without rb=: hvx's run calls return every 2^16 retired exits */
enum { CS_PENDING, CS_TYPED, CS_ACKED, CS_RETYPE };
struct ctl_send { uint64_t id, at, sent; int state; uint32_t len; uint8_t *buf; };
struct ctl_book {              /* what a snapshot carries besides the VM */
	uint64_t nstop, nev, next_id, ntyped, inh, cad0;   /* cad0: the exit count the cadence counts from */
	size_t evfrom;             /* console offset up to which lines were reported as EVENTs */
	int ns;
	struct ctl_send s[CTL_MAX_SENDS];
};
static struct {
	int on, start, ended;      /* start: 1 = the phase's start stop is due, 2 = a RESUME's */
	int boot_s1;               /* --control boot one: SNAP S1 taken in the boot phase (M121) */
	char tag[64];
	const char *phase;
	uint64_t at, rb, last_x;   /* the current stop's exit count; the cadence in exits; the last exit seen */
	struct ctl_book k;
	char endv[256];
	struct runst *rs;
} C;
static int ctl_run_to(const char *const *m, int n, size_t from, int *why);
static int g_ctl_one;          /* --control: `one` runs under lock-step control */
static int g_agent;            /* --control boot: an agent image, under control from its boot (M121) */

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
	if (C.on) return ctl_run_to(m, n, from, why);
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
static void run_rest(struct runst *rs);
static void run_once(const char *tag, const char *sched, int rewound);

/* ---------------- lock-step control (M119) ----------------
 * The protocol of hv/hvx/hvx_explore.c (milestone 118; docs/design/guest-control.md "The channel",
 * Phase 1), so that one host client (hydo-engine's `lockstep` module) drives both engines. Only the
 * clock differs: here "at" is the exit count M.exits, there the instruction clock icount.
 *   CTL <tag> <schedule>      like RUN (restore S1, arm the host part, type the guest part --
 *                             nothing when it is empty), but the engine stops and waits for the host
 *   RESUME <tag> <snapshot>   a fresh VM from a snapshot taken by SNAP in an earlier controlled run;
 *                             the run continues exactly where that run was (same schedule, console,
 *                             queued input, stop and event numbers)
 *   hvmx --control one ... "<schedule>"   the same as CTL from a fresh boot (stdin/stdout)
 * At every STOP the engine has already printed, in this order, one EVENT per complete console line
 * written since the last stop (escaped: \\ \n \r \t \xHH; without the line's \n and a trailing \r),
 * one SENT per queued input typed at this stop, and DROP for queued input a phase change discards:
 *   EVENT <tag> at=<exits> n=<event number> <escaped line>
 *   SENT <tag> id=<id> at=<exits>[ retype=1]
 *   DROP <tag> id=<id>
 *   STOP <tag> at=<exits> n=<stop number> phase=workload|check|recovery why=<why>
 * then it reads host lines until CONT or END; every other host line gets exactly one answer:
 *   SEND at=<exits> <escaped bytes>    type the bytes into the serial input at exit <exits>  ->
 *                                      "OK send id=<id> sent=<exits>" (typed now: at <= this stop) or
 *                                      "OK send id=<id> queued=<at>"
 *   ACK <id>                           the guest has consumed send <id>  ->  "OK ack <id>" / "ERR ack ..."
 *   SNAP <name> [retype]               snapshot here (workload phase; not with tj=/pct= schedules), kept
 *                                      under <name> (a second SNAP with the name replaces it; at most 8)
 *                                      ->  "OK snap <name> at=<exits> retype=<ids|->"; refused ("ERR snap
 *                                      <name> unacked=<ids>") while a typed send is not acknowledged,
 *                                      unless `retype` is given: input still in the receive queue is in
 *                                      no snapshot (M116), so RESUME types every such send again right
 *                                      after the restore. Queued sends are part of the snapshot.
 *   CONT                               run on to the next stop
 *   END [<verdict>]                    end the run at this stop: fingerprint now, RESULT with <verdict>
 *                                      (default: the last HYDO-ORACLE line)
 * EOF on stdin ends the run with "ERROR control channel closed".
 * Stops (`why`): `start` before a phase runs (the engine's own input -- the guest line, "go", the
 * oracle command -- is already typed), `resume` after a RESUME restore, `send` at the exit where a
 * queued send is due (it is typed there, before the guest runs again: exact to the exit), `marker`
 * at the exit where the shell init's workload or check marker appeared (as RUN stops there; the
 * engine then types its own next input), `cadence` every rb=<n> exits from the phase's start (the
 * schedule's rb= is the cadence here, as hvx_explore's run-call budget is there; default 65,536),
 * and `exit` when the guest stops (halt, idle, exit budget, SPIN) before the run's error RESULT.
 * Kicks and the spin observer's own exits are not exits, so every stop is a pure function of the
 * run; the rewind mode is refused (it moves M.exits backwards) and a SPIN is not re-run. A CTL
 * with no SEND gives RUN's fingerprint. RESULT gets stops=<n> typed=<sends typed> inh=<hex16>
 * (FNV-1a over every typed send's exit count, length and bytes, in typing order) before verdict=.
 *
 * Controlled boot (milestone 121; hvx_explore.c's header has the same text for icount): an image
 * built with hydo-build --control agent has the guest agent hydo-agent as /init and no S0/S1
 * markers; the host reads its /etc/hydo/control and asks for this mode.
 *   hvmx ... --control boot serve <Image> <initramfs> <outdir> ["<boot schedule>"]
 *       the boot under control from the first instruction (stops phase=boot; the boot schedule's
 *       rb= is the cadence); the host takes `SNAP S0` and `SNAP S1` where it decides; END prints
 *       "RESULT boot ..." (the fingerprint at that stop), then READY. Then `CTL <tag> <schedule>`
 *       restores the host's S1, arms the host part and types nothing (guest part "run" only; the
 *       first stop is why=resume with S1's stop number), RESUME works from any snapshot, and RUN
 *       and BRANCH are refused.
 *   hvmx ... --control boot one <Image> <initramfs> <outdir> "<schedule>"
 *       the same from a fresh boot: SNAP S1 in the boot phase also arms the schedule's host part
 *       and the run goes on in phase=workload from that stop, as a CTL from S1 does.
 * The kernel command line gets " quiet loglevel=1" instead of " hydo_phase=serve". */
struct ctl_snap { char name[64]; struct xsnap x; struct runst rs; struct ctl_book k; uint64_t rb; int used; };
static struct ctl_snap g_ctl_snaps[CTL_MAX_SNAPS];

static void ctl_esc(FILE *f, const uint8_t *p, size_t n)
{
	for (size_t i = 0; i < n; i++) {
		uint8_t c = p[i];
		if (c == '\\') fputs("\\\\", f);
		else if (c == '\n') fputs("\\n", f);
		else if (c == '\r') fputs("\\r", f);
		else if (c == '\t') fputs("\\t", f);
		else if (c < 0x20 || c > 0x7e) fprintf(f, "\\x%02x", c);
		else fputc(c, f);
	}
}
static long ctl_unesc(char *s)   /* the inverse of ctl_esc, in place; -1 on a malformed escape */
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
static void ctl_book_free(struct ctl_book *k)
{
	for (int i = 0; i < k->ns; i++) free(k->s[i].buf);
	k->ns = 0;
}
static void ctl_book_copy(struct ctl_book *d, const struct ctl_book *s)
{
	memcpy(d, s, offsetof(struct ctl_book, s) + (size_t)s->ns * sizeof s->s[0]);
	for (int i = 0; i < s->ns; i++) {
		d->s[i].buf = malloc(s->s[i].len ? s->s[i].len : 1);
		memcpy(d->s[i].buf, s->s[i].buf, s->s[i].len);
	}
}
static void ctl_begin(const char *tag)
{
	ctl_book_free(&C.k);
	memset(&C, 0, sizeof C);
	snprintf(C.tag, sizeof C.tag, "%s", tag);
	C.on = C.start = 1;
	C.phase = "workload";
	C.rb = CTL_RB_DEFAULT;
}
static void ctl_finish(void) { ctl_book_free(&C.k); C.on = 0; }
static void ctl_verdict(int r, size_t from, char *verdict, size_t n)
{
	if (r == -3 && C.endv[0]) snprintf(verdict, n, "%s", C.endv);
	else last_verdict(from, verdict, n);
}
static void ctl_type(struct ctl_send *s, int print)   /* type one send now (at the current stop) */
{
	int retype = s->state == CS_RETYPE;
	uint32_t room = (uint32_t)sizeof rxq - (rx_tail - rx_head);
	if (s->len > room) fprintf(stderr, "hvmx: %s: send %llu: %u of %u bytes do not fit in the receive queue\n",
	                           C.tag, (unsigned long long)s->id, s->len - room, s->len);
	for (uint32_t i = 0; i < s->len && i < room; i++) rxq[rx_tail++ % sizeof rxq] = s->buf[i];
	s->state = CS_TYPED;
	if (!retype) {                 /* a retype restores the base run's input, which typed= and inh= already count */
		s->sent = C.at;
		C.k.ntyped++;
		C.k.inh = fnv(&C.at, sizeof C.at, C.k.inh ? C.k.inh : 0xcbf29ce484222325ULL);
		C.k.inh = fnv(&s->len, sizeof s->len, C.k.inh);
		C.k.inh = fnv(s->buf, s->len, C.k.inh);
	}
	if (print) printf("SENT %s id=%llu at=%llu%s\n", C.tag, (unsigned long long)s->id, (unsigned long long)C.at, retype ? " retype=1" : "");
}
static void ctl_deliver(void)    /* every send due at this stop: retypes first (id order), then (at, id) order */
{
	for (int i = 0; i < C.k.ns; i++) if (C.k.s[i].state == CS_RETYPE) ctl_type(&C.k.s[i], 1);
	for (;;) {
		struct ctl_send *best = NULL;
		for (int i = 0; i < C.k.ns; i++) {
			struct ctl_send *s = &C.k.s[i];
			if (s->state != CS_PENDING || s->at > C.at) continue;
			if (!best || s->at < best->at || (s->at == best->at && s->id < best->id)) best = s;
		}
		if (!best) return;
		ctl_type(best, 1);
	}
}
static int ctl_due(void)
{
	for (int i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_RETYPE || (C.k.s[i].state == CS_PENDING && C.k.s[i].at <= C.at)) return 1;
	return 0;
}
static void ctl_drop_pending(void)   /* a phase change to a new VM: queued input belongs to the old one */
{
	for (int i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_PENDING || C.k.s[i].state == CS_RETYPE) {
			C.k.s[i].state = CS_ACKED;
			printf("DROP %s id=%llu\n", C.tag, (unsigned long long)C.k.s[i].id);
		}
}
static void ctl_snap_cmd(char *arg)
{
	char name[64] = "", mode[16] = "", ids[512] = "";
	size_t il = 0;
	int slot = -1, n = sscanf(arg, "%63s %15s", name, mode);
	if (n < 1 || (n == 2 && strcmp(mode, "retype"))) { printf("ERR snap bad request\n"); return; }
	if (strcmp(C.phase, "workload") && strcmp(C.phase, "boot")) { printf("ERR snap %s only in the workload or boot phase\n", name); return; }
	if (TJ.on || C.rs->pct_n) { printf("ERR snap %s not for tj/pct schedules\n", name); return; }
	for (int i = 0; i < C.k.ns; i++)
		if (C.k.s[i].state == CS_TYPED)
			il += (size_t)snprintf(ids + il, il < sizeof ids ? sizeof ids - il : 0, "%s%llu", il ? "," : "", (unsigned long long)C.k.s[i].id);
	if (il >= sizeof ids) snprintf(ids + sizeof ids - 5, 5, ",...");
	if (il && n < 2) { printf("ERR snap %s unacked=%s\n", name, ids); return; }
	for (int i = 0; i < CTL_MAX_SNAPS; i++) if (g_ctl_snaps[i].used && !strcmp(g_ctl_snaps[i].name, name)) slot = i;
	for (int i = 0; i < CTL_MAX_SNAPS && slot < 0; i++) if (!g_ctl_snaps[i].used) slot = i;
	if (slot < 0) { printf("ERR snap %s no free slot (%d names)\n", name, CTL_MAX_SNAPS); return; }
	struct ctl_snap *cs = &g_ctl_snaps[slot];
	if (cs->used) {                /* replaced: keep the RAM mapping, drop the rest */
		free(cs->x.con); cs->x.con = NULL;
		if (cs->x.vio) { virtio_save_free(cs->x.vio); cs->x.vio = NULL; }
		ctl_book_free(&cs->k);
	}
	xsnap_take(&cs->x, "ctl");
	ctl_book_copy(&cs->k, &C.k);
	for (int i = 0; i < cs->k.ns; i++) if (cs->k.s[i].state == CS_TYPED) cs->k.s[i].state = CS_RETYPE;
	cs->rs = *C.rs; cs->rb = C.rb; cs->used = 1;
	snprintf(cs->name, sizeof cs->name, "%s", name);
	printf("OK snap %s at=%llu retype=%s\n", name, (unsigned long long)C.at, il ? ids : "-");
	if (g_agent && g_ctl_one && !strcmp(C.phase, "boot") && !strcmp(name, "S1"))
		C.boot_s1 = 1;             /* M121: the workload starts here once the host says CONT */
}
/* one stop: report, then serve host lines until CONT (0) or END/EOF (1) */
static int ctl_stop(const char *why)
{
	static char *ln;
	static size_t lcap;
	size_t p = C.k.evfrom;
	for (size_t q = p; q < M.console_len; q++) {   /* complete console lines since the last stop */
		size_t e = q;
		if (console[q] != '\n') continue;
		if (e > p && console[e - 1] == '\r') e--;
		printf("EVENT %s at=%llu n=%llu ", C.tag, (unsigned long long)C.at, (unsigned long long)++C.k.nev);
		ctl_esc(stdout, (const uint8_t *)console + p, e - p);
		putchar('\n');
		p = q + 1;
	}
	C.k.evfrom = p;
	ctl_deliver();
	printf("STOP %s at=%llu n=%llu phase=%s why=%s\n", C.tag, (unsigned long long)C.at, (unsigned long long)++C.k.nstop, C.phase, why);
	fflush(stdout);
	for (;;) {
		ssize_t r = getline(&ln, &lcap, stdin);
		if (r < 0) { snprintf(C.endv, sizeof C.endv, "ERROR control channel closed"); return 1; }
		while (r > 0 && (ln[r - 1] == '\n' || ln[r - 1] == '\r')) ln[--r] = 0;
		if (!strcmp(ln, "CONT")) return 0;
		if (!strncmp(ln, "END", 3) && (!ln[3] || ln[3] == ' ')) { snprintf(C.endv, sizeof C.endv, "%s", ln[3] ? ln + 4 : ""); return 1; }
		if (!strncmp(ln, "SEND at=", 8)) {
			char *end;
			unsigned long long at = strtoull(ln + 8, &end, 10);
			long len;
			if (end == ln + 8 || *end != ' ' || (len = ctl_unesc(end + 1)) < 0) { printf("ERR send bad request\n"); fflush(stdout); continue; }
			if (len == 0 || len > CTL_SEND_MAX) { printf("ERR send length %ld not in [1, %d]\n", len, CTL_SEND_MAX); fflush(stdout); continue; }
			if (C.k.ns >= CTL_MAX_SENDS) { printf("ERR send more than %d sends in one run\n", CTL_MAX_SENDS); fflush(stdout); continue; }
			struct ctl_send *s = &C.k.s[C.k.ns++];
			memset(s, 0, sizeof *s);
			s->buf = malloc((size_t)len);
			memcpy(s->buf, end + 1, (size_t)len);
			s->len = (uint32_t)len; s->at = at; s->id = ++C.k.next_id;
			if (at <= C.at) { ctl_type(s, 0); printf("OK send id=%llu sent=%llu\n", (unsigned long long)s->id, (unsigned long long)C.at); }
			else printf("OK send id=%llu queued=%llu\n", (unsigned long long)s->id, at);
		} else if (!strncmp(ln, "ACK ", 4)) {
			unsigned long long id = strtoull(ln + 4, NULL, 10);
			int done = 0;
			for (int i = 0; i < C.k.ns && !done; i++) {
				if (C.k.s[i].id != id) continue;
				done = 1;
				if (C.k.s[i].state == CS_PENDING) printf("ERR ack %llu not typed yet\n", id);
				else { C.k.s[i].state = CS_ACKED; printf("OK ack %llu\n", id); }
			}
			if (!done) printf("ERR ack %llu unknown\n", id);
		} else if (!strncmp(ln, "SNAP ", 5)) {
			ctl_snap_cmd(ln + 5);
		} else {
			printf("ERR unknown command ");
			ctl_esc(stdout, (const uint8_t *)ln, strlen(ln));
			putchar('\n');
		}
		fflush(stdout);
	}
}
static int ctl_hook(void)         /* hvm_stop_hook at every loop top of a lock-step phase */
{
	uint64_t x = M.exits;
	if (x == C.last_x) return 0;   /* a kick or an observer exit: not a new exit */
	C.last_x = x;
	int hit = marker_hook();       /* RUN's marker stop, at the same exit */
	C.at = x;
	const char *why = hit ? "marker" : ctl_due() ? "send" : (x - C.k.cad0) % C.rb == 0 ? "cadence" : NULL;
	if (why && ctl_stop(why)) { C.ended = 1; return 1; }
	if (C.boot_s1) { C.ended = 2; return 1; }   /* M121: --control boot one, S1 taken here */
	return hit;
}
/* run_to under lock-step: the phase's start (or resume) stop, then stops at markers, due sends and
 * the cadence; -3 when the host ended the run (END) */
static int ctl_run_to(const char *const *m, int n, size_t from, int *why)
{
	for (int i = 0; i < n; i++) hk_m[i] = m[i];
	hk_n = n; hk_hit = -1; hk_from = hk_scan = from;
	if (C.start) {
		const char *w = C.start == 2 ? "resume" : "start";
		if (C.start == 2) C.k.nstop--;   /* the snapshot's own stop, again: the same numbers as the base */
		else C.k.cad0 = M.exits;
		C.start = 0;
		C.at = M.exits;
		if (ctl_stop(w)) return -3;
		if (C.boot_s1) return -4;
	}
	for (int i = 0; i < n; i++)      /* a marker may already be there: stop where we are, as RUN would */
		if (M.console_len > from && memmem(console + from, M.console_len - from, m[i], strlen(m[i]))) return i;
	C.last_x = M.exits; C.ended = 0;
	hvm_stop_hook = ctl_hook;
	max_exits = M.exits + run_exits;
	int s = run_until(NULL);
	hvm_stop_hook = NULL;
	if (C.ended == 2) return -4;
	if (C.ended) return -3;
	if (s == STOP_MARKER) return hk_hit;
	C.at = M.exits;
	if (ctl_stop("exit")) return -3;
	if (why) *why = s;
	return -1;
}
/* The lock-step part of a schedule: s (a copy) loses its rb=<n> (the cadence, *rb; 0 = none).
 * Prints the error RESULT and returns -1 for a rewind-mode schedule or a bad rb=. */
static int ctl_sched(const char *tag, char *s, uint64_t *rb)
{
	char *bar = strchr(s, '|');
	*rb = 0;
	if (rw_on || (bar && memmem(s, (size_t)(bar - s), "spin=rewind", 11))) {
		printf("RESULT %s fp=0 wfp=0 secs=0 verdict=UNSUPPORTED on hvm: lock-step control does not run in rewind mode\n", tag);
		fflush(stdout);
		return -1;
	}
	for (char *w = s; bar && (w = strstr(w, "rb=")) && w < bar; ) {   /* take the cadence out of the host part */
		if (w != s && w[-1] != ' ') { w += 3; continue; }
		char *e;
		*rb = strtoull(w + 3, &e, 10);
		if (e == w + 3 || (*e != ' ' && *e != '|')) {
			printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR bad schedule: rb= takes a number of exits\n", tag);
			fflush(stdout);
			return -1;
		}
		memmove(w, e, strlen(e) + 1);
		bar = strchr(s, '|');
	}
	return 0;
}
/* An agent image's schedule has no guest tokens: its guest part is "run" (or empty). */
static int agent_guest_ok(const char *s)
{
	const char *bar = strchr(s, '|');
	if (!bar) return 1;            /* run_once refuses it */
	for (bar++; *bar == ' '; bar++) ;
	size_t n = strlen(bar);
	while (n && (bar[n - 1] == ' ' || bar[n - 1] == '\n' || bar[n - 1] == '\r')) n--;
	return n == 0 || (n == 3 && !strncmp(bar, "run", 3));
}
static struct ctl_snap *ctl_find(const char *name)
{
	for (int i = 0; i < CTL_MAX_SNAPS; i++) if (g_ctl_snaps[i].used && !strcmp(g_ctl_snaps[i].name, name)) return &g_ctl_snaps[i];
	return NULL;
}
/* CTL <tag> <schedule> (after the S1 restore) and `--control one`: the schedule's rb=<n> is the
 * cadence. For an agent image (M121) a CTL restores the host's S1 (cs) here and continues its book. */
static void ctl_request(const char *tag, const char *sched, struct ctl_snap *cs)
{
	char s[4096];
	uint64_t rb = 0;
	snprintf(s, sizeof s, "%s", sched);
	if (ctl_sched(tag, s, &rb)) return;
	if (cs) xsnap_restore(&cs->x);
	ctl_begin(tag);
	if (cs) { ctl_book_copy(&C.k, &cs->k); C.start = 2; }   /* the first stop is S1's own again */
	if (rb) C.rb = rb;
	spin_hit = 0;
	run_once(tag, s, 0);
	if (C.on) ctl_finish();         /* a refused schedule: no run, no RESULT facts */
}
/* ---- the controlled boot of an agent image (M121, see above) ---- */
static struct runst g_boot_rs;
/* the boot under control: -3 at the host's END, -4 (--control boot one) at the stop where the host
 * took S1 and said CONT, -1 when the guest stopped (*why) */
static int agent_boot(const char *tag, uint64_t rb, int *why)
{
	ctl_begin(tag);
	C.phase = "boot";
	if (rb) C.rb = rb;
	memset(&g_boot_rs, 0, sizeof g_boot_rs);
	snprintf(g_boot_rs.tag, sizeof g_boot_rs.tag, "%s", tag);
	g_boot_rs.t0 = now_s();
	C.rs = &g_boot_rs;             /* what a SNAP in the boot carries */
	return run_to(NULL, 0, 0, why);
}
static void agent_boot_result(const char *tag, int r, int why, double t0)
{
	char verdict[300];
	uint64_t wfp = 0;
	if (r == -3) { wfp = xfp(); ctl_verdict(r, 0, verdict, sizeof verdict); }
	else snprintf(verdict, sizeof verdict, "ERROR boot: %s", stop_name(why));
	save_log(tag, 0);
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f stops=%llu typed=%llu inh=%016llx verdict=%s\n", tag,
	       (unsigned long long)fnv(&wfp, 8, wfp), (unsigned long long)wfp, now_s() - t0,
	       (unsigned long long)C.k.nstop, (unsigned long long)C.k.ntyped, (unsigned long long)C.k.inh, verdict);
	fflush(stdout);
	ctl_finish();
}
/* RESUME <tag> <snapshot>: continue a controlled run from a SNAP in a fresh VM (see above) */
static void ctl_resume(const char *tag, const char *name)
{
	struct ctl_snap *cs = NULL;
	for (int i = 0; i < CTL_MAX_SNAPS; i++) if (g_ctl_snaps[i].used && !strcmp(g_ctl_snaps[i].name, name)) cs = &g_ctl_snaps[i];
	if (!cs) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR no snapshot %s\n", tag, name); fflush(stdout); return; }
	xsnap_restore(&cs->x);
	ctl_begin(tag);
	ctl_book_copy(&C.k, &cs->k);
	C.start = 2;                   /* the first stop says why=resume */
	C.rb = cs->rb;
	struct runst rs = cs->rs;
	snprintf(rs.tag, sizeof rs.tag, "%s", tag);
	rs.t0 = now_s();
	spin_hit = 0;
	run_rest(&rs);
	if (C.on) ctl_finish();
}

static void run_once(const char *tag, const char *sched, int rewound)
{
	char host[1024], guest[1024], line[1100];
	double t0 = now_s();
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
	const char *pp = strstr(host, "pct=");
	uint64_t pct_seed = pp ? strtoull(pp + 4, NULL, 16) : 0;
	uint32_t pct_n = (pp = strstr(host, "pctn=")) ? (uint32_t)strtoul(pp + 5, NULL, 10) : 0;
	uint64_t pct_k = (pp = strstr(host, "pctk=")) ? strtoull(pp + 5, NULL, 10) : 20000;
	if (!strstr(host, "pct=")) pct_n = 0;
	int cut = strstr(guest, " C=") != NULL;
	size_t from = M.console_len;
	if (C.on && !g_agent) C.k.evfrom = from;   /* lock-step: EVENTs start with the workload's console */
	if (trace_f) fprintf(trace_f, "RUN %s\n", tag);
	set_fault(dseed, dcl, dpr);
	struct runst rs = { .t0 = t0, .from = from, .cut = cut, .rewound = rewound, .pct_n = pct_n };
	snprintf(rs.tag, sizeof rs.tag, "%s", tag);
	if (tj_wanted(&tj)) {            /* the jitter covers the workload only, like the disk fault */
		TJ = tj; TJ.on = 1;
		snprintf(rs.tjinfo, sizeof rs.tjinfo, " tjk=%llu", (unsigned long long)(M.vtime / TJ_PERIOD));
	}
	rs.wx0 = M.exits;
	if (pct_n) pct_set(pct_seed, pct_n, M.exits, M.exits + pct_k);   /* the workload only */
	snprintf(line, sizeof line, "%s\n", guest + 1);
	if (!(C.on && (!guest[0] || g_agent))) hvm_put_input(line);   /* lock-step: an empty guest part (an agent image: always) types nothing */
	run_rest(&rs);
}
/* the rest of a run, from the workload's start: a RUN goes on here, a lock-step RESUME starts here */
static void run_rest(struct runst *rs)
{
	static const char *const wk[] = { DONE_MARKER, FINAL_MARKER };
	static const char *const fin[] = { FINAL_MARKER };
	const char *tag = rs->tag;
	char verdict[300] = "", *tjinfo = rs->tjinfo;
	double t0 = rs->t0;
	uint64_t wfp = 0, ffp = 0, wx0 = rs->wx0;
	uint32_t pct_n = rs->pct_n;
	int cut = rs->cut, rewound = rs->rewound, why = 0, r;
	size_t from = rs->from;
	/* the recovery boot's "oracle" command: 3584 bytes, as hvx_explore's, so that the fake outside
	 * world of a manifest with fakes:/proxies: (hydo-init-fakes.sh appends its records to the
	 * acknowledged ids at a power cut, at most 3000 bytes) fits; the UART input ring is 4096
	 * (../hvm.c rxq). A command that fitted before is unchanged. */
	char orc[3584];
	C.rs = rs;                     /* lock-step: what a SNAP in this run carries */
	r = run_to(wk, g_agent ? 0 : 2, from, &why);   /* an agent image prints no marker */
	if (pct_n) {
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof rs->tjinfo - l, " pctf=%llu pctw=%llu", (unsigned long long)PCT.fired, (unsigned long long)(M.exits - wx0));
		memset(&PCT, 0, sizeof PCT);
	}
	if (TJ.on) {
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof rs->tjinfo - l, "-%llu tjn=%llu",
		         (unsigned long long)(M.vtime / TJ_PERIOD), (unsigned long long)TJ.applied);
		memset(&TJ, 0, sizeof TJ);
	}
	/* r == -3: the host ended a lock-step run at a stop (END); never without lock-step */
	if (r < 0 && r != -3) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR workload: %s", stop_name(why)); goto out; }
	wfp = xfp(); diag(tag, "w");
	if (r == 1 || r == -3) { ctl_verdict(r, from, verdict, sizeof verdict); ffp = wfp; goto out; }
	if (!cut) {                    /* same boot: faults off, heal, check */
		set_fault(0, 0, 0);
		hvm_put_input("go\n");
		if (C.on) { C.phase = "check"; C.start = 1; }
		r = run_to(fin, 1, from, &why);
		if (r < 0 && r != -3) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR check: %s", stop_name(why)); goto out; }
		ffp = xfp(); diag(tag, "f");
		ctl_verdict(r, from, verdict, sizeof verdict);
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
		if (C.on) {                /* a new VM with its own clock: queued input does not carry over */
			ctl_drop_pending();
			C.phase = "recovery"; C.start = 1; C.k.evfrom = from;
		}
		r = run_to(fin, 1, from, &why);
		if (r < 0 && r != -3) { spin_hit |= why == STOP_SPIN; snprintf(verdict, sizeof verdict, "ERROR recovery: %s", stop_name(why)); goto out_rec; }
		ffp = xfp(); diag(tag, "f");
		ctl_verdict(r, from, verdict, sizeof verdict);
	out_rec:;
		char rt[80]; snprintf(rt, sizeof rt, "%s.recovery", tag);
		save_log(rt, from);
		goto done;
	}
out:
	save_log(tag, from);
done:
	if (spin_hit && !rewound && spin_auto && !C.on) return;   /* not reported: run_sched re-runs it with rewind */
	if (rewound) fprintf(stderr, "hvmx: %s: reported from a rewind-mode run\n", tag);
	if (C.on) {                    /* lock-step facts, before verdict= like the other extras */
		size_t l = strlen(tjinfo);
		snprintf(tjinfo + l, sizeof rs->tjinfo - l, " stops=%llu typed=%llu inh=%016llx",
		         (unsigned long long)C.k.nstop, (unsigned long long)C.k.ntyped, (unsigned long long)C.k.inh);
	}
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f%s verdict=%s\n", tag,
	       (unsigned long long)fnv(&ffp, 8, wfp), (unsigned long long)wfp, now_s() - t0, tjinfo, verdict);
	fflush(stdout);
	if (C.on) ctl_finish();
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
	static const char *const m0[] = { S0_MARKER, FINAL_MARKER, AGENT_PREFIX };
	static const char *const m1[] = { S1_MARKER, FINAL_MARKER };
	int why = 0, r0 = run_to(m0, 3, 0, &why);   /* a shell image never prints the agent's prefix (M121) */
	if (r0 == 2) { fprintf(stderr, "hvmx: the guest's first process is hydo-agent (an agent image, /etc/hydo/control): boot it with --control boot\n"); return -1; }
	if (r0 != 0) { fprintf(stderr, "hvmx: no S0 marker (%s)\n", stop_name(why)); fwrite(console, 1, M.console_len, stderr); return -1; }
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
	int ctl_one = 0;
	if (argc > 2 && !strcmp(argv[1], "--control") && !strcmp(argv[2], "boot")) {   /* an agent image (M121) */
		g_agent = 1;
		argv[2] = argv[0];
		argv += 2; argc -= 2;
	} else if (argc > 1 && !strcmp(argv[1], "--control")) {   /* lock-step control of `one` (M119) */
		ctl_one = 1;
		argv[1] = argv[0];
		argv++; argc--;
	}
	if (argc < 5 || (strcmp(argv[1], "serve") && strcmp(argv[1], "one")) || (!strcmp(argv[1], "one") && argc < 6) ||
	    (ctl_one && strcmp(argv[1], "one"))) {
		fprintf(stderr, "usage: hvmx [--ram-mib N] serve <Image> <initramfs> <outdir>\n       hvmx [--ram-mib N] [--control] one <Image> <initramfs> <outdir> \"<schedule>\"\n"
		                "       hvmx [--ram-mib N] --control boot serve|one <Image> <initramfs> <outdir> [\"<schedule>\"]\n");
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
	snprintf(bootargs, sizeof bootargs, "%s %s", getenv("HVM_BOOTARGS") ? getenv("HVM_BOOTARGS") : BOOTARGS,
	         g_agent ? "quiet loglevel=1" : "hydo_phase=serve");   /* an agent's framed lines share the console */
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
	if (g_agent) {                 /* M121: the boot under control; S0 and S1 are the host's SNAPs */
		char s[4096];
		uint64_t rb = 0;
		int why = 0, r;
		const char *tag = strcmp(mode, "one") ? "boot" : "one";
		snprintf(s, sizeof s, "%s", argc > 5 ? argv[5] : "|");
		if (ctl_sched(tag, s, &rb)) return 2;
		if (!agent_guest_ok(s)) { fprintf(stderr, "hvmx: an agent image's schedule has the guest part \"run\" only\n"); return 2; }
		g_ctl_one = !strcmp(mode, "one");
		r = agent_boot(tag, rb, &why);
		if (r != -4) agent_boot_result(tag, r, why, t0);
		if (g_ctl_one) {
			if (r == -4) {         /* the workload from the stop where S1 was taken, as a CTL from S1 */
				struct ctl_snap *cs = ctl_find("S0");
				if (cs) g_s0 = &cs->x;
				C.boot_s1 = 0;
				C.phase = "workload";
				spin_hit = 0;
				run_once(tag, s, 0);
				if (C.on) ctl_finish();
			}
			return 0;
		}
		{
			struct ctl_snap *cs = ctl_find("S0");
			if (cs) g_s0 = &cs->x;
		}
		printf("READY boot_secs=%.1f\n", now_s() - t0);
		char in[4096];
		while (fgets(in, sizeof in, stdin)) {
			char tag2[64]; int off = 0;
			if (!strncmp(in, "QUIT", 4)) break;
			if (!strncmp(in, "BRANCH ", 7)) { printf("BRANCHEND ? unsupported on hvm\n"); fflush(stdout); continue; }
			if (!strncmp(in, "RESUME ", 7)) {
				char name[64];
				if (sscanf(in, "RESUME %63s %63s", tag2, name) != 2) { printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n"); fflush(stdout); continue; }
				ctl_resume(tag2, name);
				continue;
			}
			int ctl = !strncmp(in, "CTL ", 4);
			const char *rq = in + (ctl ? 4 : 0);
			if (sscanf(rq, ctl ? "%63s %n" : "RUN %63s %n", tag2, &off) != 1 || !off) { printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n"); fflush(stdout); continue; }
			struct ctl_snap *cs = ctl_find("S1");
			const char *no = !ctl ? "an agent image runs under control only (CTL, RESUME)" : !cs ? "no snapshot S1 (the boot took none)" :
			                 !agent_guest_ok(rq + off) ? "an agent image's schedule has the guest part \"run\" only" : NULL;
			if (no) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=ERROR %s\n", tag2, no); fflush(stdout); continue; }
			ctl_request(tag2, rq + off, cs);
		}
		return 0;
	}
	if (!strcmp(mode, "one")) {
		if (boot_to_s1(&s0, spin_auto ? &s1 : NULL)) return 1;   /* S0 is still needed for a power cut's recovery boot */
		fprintf(stderr, "hvmx: provisioned in %.1f s (fresh boot, no snapshot restore)\n", now_s() - t0);
		if (ctl_one) ctl_request("one", argv[5], NULL);
		else run_sched("one", argv[5], &s1);   /* S1 only if a SPIN run must be repeated with rewind */
		return 0;
	}
	if (boot_to_s1(&s0, &s1)) return 1;
	printf("READY boot_secs=%.1f\n", now_s() - t0);
	char in[4096];
	while (fgets(in, sizeof in, stdin)) {
		char tag[64]; int off = 0;
		if (!strncmp(in, "QUIT", 4)) break;
		if (!strncmp(in, "BRANCH ", 7)) { printf("BRANCHEND ? unsupported on hvm\n"); fflush(stdout); continue; }
		if (!strncmp(in, "RESUME ", 7)) {   /* lock-step (M119) */
			char name[64];
			if (sscanf(in, "RESUME %63s %63s", tag, name) != 2) { printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n"); fflush(stdout); continue; }
			ctl_resume(tag, name);
			continue;
		}
		int ctl = !strncmp(in, "CTL ", 4);   /* lock-step (M119) */
		const char *rq = in + (ctl ? 4 : 0);
		if (sscanf(rq, ctl ? "%63s %n" : "RUN %63s %n", tag, &off) != 1 || !off) { printf("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request\n"); fflush(stdout); continue; }
		xsnap_restore(&s1);
		if (ctl) ctl_request(tag, rq + off, NULL);
		else run_sched(tag, in + off, &s1);
	}
	return 0;
}

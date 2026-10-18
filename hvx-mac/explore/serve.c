/*
 * hvmx -- the explorer's run engine on the Mac (Hypervisor.framework), the counterpart of
 * hvx/hvx_explore.c for the deterministic arm64 VMM in ../hvm.c (Milestone 48).
 *
 * It speaks hvx_explore's protocol, so hvx/explore/explore.py drives it unchanged apart from
 * the command line (explore.py --backend hvm). The guest is a manifest-built arm64 initramfs
 * (hvx/onboard/hydo-build --arch arm64) whose /sbin/init is the generic hvx/onboard/hydo-init.sh:
 *
 *   S0  "hydo-explore-ready-S0"   nothing started yet           -> start of a recovery boot
 *   S1  "hydo-explore-ready-S1"   services up, provisioned      -> start of every schedule
 *
 * The engine boots once, snapshots S0 and S1 (vCPU registers, the machine/device/clock state,
 * and all guest RAM in a file-backed buffer), then for every schedule restores S1 into a FRESH
 * vCPU, types the schedule's guest part on the serial console, runs to
 * "hydo-explore-workload-done", types "go", and runs to the final verdict.
 *
 *   hvmx serve <Image> <initramfs> <outdir>         "RUN <tag> <schedule>" on stdin -> RESULT lines
 *   hvmx one   <Image> <initramfs> <outdir> "<schedule>"
 *        one schedule from a fresh boot, no snapshot restore: its fingerprint must equal serve's
 *
 * Supported now: the guest part's K/P/T/<knob> tokens. Not yet (no virtio-blk, no tc on the
 * arm64 root): power cuts (C=), disk faults (disk= other than none), network faults (N=, O=),
 * tick-timing jitter (tj*: there is no PMU clock on this hardware). A schedule asking for one
 * of those gets "verdict=UNSUPPORTED ..." rather than running something it did not ask for.
 * explore.py --backend hvm strips them from generated schedules up front (hvm_supported()).
 *
 * Env: HVM_MEM_MB (default 1024, the manifest v1 size), HVM_SNAP_DIR (default /tmp/hvm-snap),
 *      HVM_RUN_EXITS (per-phase exit budget, default 50,000,000), plus ../hvm.c's knobs.
 */
#define HVM_NO_MAIN
#include "../hvm.c"
#include <errno.h>

#define S0_MARKER    "hydo-explore-ready-S0"
#define S1_MARKER    "hydo-explore-ready-S1"
#define DONE_MARKER  "hydo-explore-workload-done"
#define FINAL_MARKER "hydo-stack-marker: workload complete"

static const char *outdir = ".";
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
	return mix64(ram, ram_size, h);
}

/* ---------------- snapshots: RAM in an unlinked file, mapped ---------------- */
struct xsnap { struct vsnap v; struct mach m; uint8_t *ram; int have; };
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
	s->have = 1;
}
static void xsnap_restore(const struct xsnap *s)
{
	CHK(hv_vcpu_destroy(vcpu));
	CHK(hv_vcpu_create(&vcpu, &ex, NULL)); trap_dbg_applied = -1;
	memcpy(ram, s->ram, ram_size);
	vput(&s->v); M = s->m;
	rx_head = rx_tail = 0;
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
/* why a schedule cannot run here, or NULL */
static const char *unsupported(const char *host, const char *guest)
{
	const char *d = strstr(host, "disk=");
	if (d && strncmp(d, "disk=none", 9)) return "disk faults and power cuts need virtio-blk";
	if (strstr(host, "tj=") || strstr(host, "tjx=")) return "tick-timing jitter needs a PMU clock";
	if (strstr(guest, " C=")) return "power cuts need virtio-blk";
	if (strstr(guest, " N=") || strstr(guest, " O=")) return "network faults need tc on the arm64 root";
	return NULL;
}
static void run_sched(const char *tag, const char *sched)
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
	const char *u = unsupported(host, guest);
	if (u) { printf("RESULT %s fp=0 wfp=0 secs=0 verdict=UNSUPPORTED on hvm: %s\n", tag, u); fflush(stdout); return; }
	size_t from = M.console_len;
	int why = 0, r;
	if (trace_f) fprintf(trace_f, "RUN %s\n", tag);
	snprintf(line, sizeof line, "%s\n", guest + 1);
	hvm_put_input(line);
	r = run_to(wk, 2, from, &why);
	if (r < 0) { snprintf(verdict, sizeof verdict, "ERROR workload: %s", stop_name(why)); goto out; }
	wfp = xfp();
	if (r == 1) { last_verdict(from, verdict, sizeof verdict); ffp = wfp; goto out; }
	hvm_put_input("go\n");
	r = run_to(fin, 1, from, &why);
	if (r < 0) { snprintf(verdict, sizeof verdict, "ERROR check: %s", stop_name(why)); goto out; }
	ffp = xfp();
	last_verdict(from, verdict, sizeof verdict);
out:
	save_log(tag, from);
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f verdict=%s\n", tag,
	       (unsigned long long)fnv(&ffp, 8, wfp), (unsigned long long)wfp, now_s() - t0, verdict);
	fflush(stdout);
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

int main(int argc, char **argv)
{
	if (argc < 5 || (strcmp(argv[1], "serve") && strcmp(argv[1], "one")) || (!strcmp(argv[1], "one") && argc < 6)) {
		fprintf(stderr, "usage: hvmx serve <Image> <initramfs> <outdir>\n       hvmx one <Image> <initramfs> <outdir> \"<schedule>\"\n");
		return 2;
	}
	const char *mode = argv[1];
	kernel_img = load_file(argv[2], &kernel_len);
	initrd_img = load_file(argv[3], &initrd_len);
	outdir = argv[4];
	mkdir(outdir, 0755);
	ram_size = (uint64_t)(getenv("HVM_MEM_MB") ? atoi(getenv("HVM_MEM_MB")) : 1024) << 20;
	if (getenv("HVM_STEP")) STEP = strtoull(getenv("HVM_STEP"), 0, 0);
	if (getenv("HVM_RUN_EXITS")) run_exits = strtoull(getenv("HVM_RUN_EXITS"), 0, 0);
	if (getenv("HVM_SPIN_SECS")) spin_secs = atof(getenv("HVM_SPIN_SECS"));
	echo = getenv("HVM_ECHO") != NULL;
	if (getenv("HVM_TRACE")) trace_f = fopen(getenv("HVM_TRACE"), "w");
	setvbuf(stdout, NULL, _IOLBF, 0);
	char bootargs[1024];
	snprintf(bootargs, sizeof bootargs, "%s hydo_phase=serve", getenv("HVM_BOOTARGS") ? getenv("HVM_BOOTARGS") : BOOTARGS);
	{ pthread_t t; pthread_create(&t, NULL, watchdog, NULL); }
	vm_create(bootargs);
	vcpu_boot();
	double t0 = now_s();
	static struct xsnap s0, s1;
	if (!strcmp(mode, "one")) {
		if (boot_to_s1(NULL, NULL)) return 1;
		fprintf(stderr, "hvmx: provisioned in %.1f s (fresh boot, no snapshot restore)\n", now_s() - t0);
		run_sched("one", argv[5]);
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
		run_sched(tag, in + off);
	}
	return 0;
}

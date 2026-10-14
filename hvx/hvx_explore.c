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
 * A schedule has a host part (disk fault) and a guest part (service kill, network fault,
 * power-cut point, durability knobs), separated by '|':
 *   disk=<none|ioerr:P|cut|lying:P> dseed=<hex> | run [K=..] [N=..] [C=..] [M=..] [R=..]
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

#define EXP_MEM_MIB	1024ULL
#define S0_MARKER	"hydo-explore-ready-S0"
#define S1_MARKER	"hydo-explore-ready-S1"
#define DONE_MARKER	"hydo-explore-workload-done"
#define ACKED_PREFIX	"hydo-explore-acked:"
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

/* Run until any of the markers appears in the console AFTER offset `from`. Returns the
 * marker index, -1 on a VM error, -2 when the run exceeds RUN_WALL_SECS. */
static int run_until_any(struct boot *b, const char *const *mk, int n, size_t from)
{
	struct hvx_exit ex;
	double t0 = now_s();
	for (;;) {
		int i;
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
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

/* "hydo-explore-acked: 0 1 2 ..." after `from` -> "oracle 0 1 2 ...\n" */
static void oracle_cmd(const struct boot *b, size_t from, char *out, size_t n)
{
	long at = b->ulen > from ? mem_find(b->ubuf + from, b->ulen - from, ACKED_PREFIX) : -1;
	size_t o = (size_t)snprintf(out, n, "oracle");
	if (at >= 0) {
		const uint8_t *p = b->ubuf + from + at + strlen(ACKED_PREFIX);
		while (p < b->ubuf + b->ulen && *p != '\n' && *p != '\r' && o + 2 < n)
			out[o++] = (char)*p++;
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
	char host[256];
	char guest[512];
	uint32_t classes, prob;
	uint64_t dseed;
	int cut;
};

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
	sc->cut = strstr(sc->guest, " C=") != NULL;
	return 0;
}

/* Run one schedule in VM b, which is positioned at S1 (restored or reached by booting).
 * Prints the RESULT line. Consumes b (tears it down). */
static void run_sched(struct boot *b, const char *tag, const struct sched *sc, struct snap *s0)
{
	static const char *const wk[] = { DONE_MARKER, FINAL_MARKER };
	static const char *const fin[] = { FINAL_MARKER };
	char line[600], verdict[256] = "", orc[512];
	uint64_t wfp = 0, ffp = 0;
	size_t from = b->ulen;
	double t0 = now_s();
	int r;
	const char *phase = "workload";

	if (set_fault(b, sc->dseed, sc->classes, sc->prob)) { snprintf(verdict, sizeof(verdict), "ERROR harness set_fault"); goto out; }
	snprintf(line, sizeof(line), "%s\n", sc->guest);
	if (put_input(b, line)) { snprintf(verdict, sizeof(verdict), "ERROR harness put_input"); goto out; }
	r = run_until_any(b, wk, 2, from);
	if (r < 0) { snprintf(verdict, sizeof(verdict), r == -2 ? "TIMEOUT workload" : "ERROR vm workload"); goto out; }
	wfp = fingerprint(b);
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
	printf("RESULT %s fp=%016llx wfp=%016llx secs=%.1f verdict=%s\n", tag,
	       (unsigned long long)fnv(wfp, &ffp, sizeof(ffp)), (unsigned long long)wfp, now_s() - t0, verdict);
	fflush(stdout);
	boot_teardown(b);
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
		char in[1024];
		while (fgets(in, sizeof(in), stdin)) {
			char tag[64];
			int off = 0;
			struct sched sc;
			if (!strncmp(in, "QUIT", 4)) break;
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

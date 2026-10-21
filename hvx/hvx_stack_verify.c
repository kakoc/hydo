// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_stack_verify - milestone 35 (Rung 6): the FOUR-SERVICE STACK flagship.
 * UNMODIFIED MySQL 8.4 + Kafka 3.5 (KRaft) + Redis 7.2 behind a small gRPC front door
 * (hvx/stack/, Go), all co-located in one guest over the in-kernel 127.0.0.1 loopback,
 * a client placing orders through gRPC, and a CROSS-SERVICE oracle. Derived from
 * hvx_mysql_verify.c (same harness shape); the guest is built by build-stack.sh.
 *
 * Each order is ACKed by the front door only after MySQL (autocommit INSERT), Kafka
 * (acks=all) and Redis (SET) all succeeded. After a power cut + recovery the oracle
 * checks, for every acked order: MySQL has it with the right amount, the Kafka topic has
 * it, and Redis (a cache) either misses it or holds the right value.
 *   ==> HYDO-ORACLE: OK orders=<N> consistent | VIOLATION <store> order=<id> ...
 *
 * Power cut = the backing disk's VOLATILE WRITE CACHE (HVX_FAULT_DROP_UNFLUSHED at prob
 * 100: durable only after a virtio-blk FLUSH, i.e. an fsync) + a two-boot crash-recover.
 *     * hydo_flush=on  -> every store durable before the ACK (MySQL trx_commit=1, Kafka
 *                         flush.messages=1, Redis appendfsync=always)       -> SURVIVES.
 *     * hydo_flush=off -> ONE relaxed knob, MySQL trx_commit=2 (Kafka and Redis stay
 *                         durable) -> Kafka holds acked orders MySQL lost: a cross-service
 *                         disagreement.
 *
 * Checks (mode = sanity | replay | f1 | f2 | f3 | all):
 *   SANITY  solo boot, no fault, no crash                       -> OK
 *   REPLAY  two solo boots                                      -> bit-identical
 *   F1      flush=on  + power cut, twice                        -> OK, bit-identical
 *   F2      flush=off + power cut, twice + branch-from-snapshot -> VIOLATION, bit-identical
 *   F3      flush=on  + seeded lying-disk DROP, two seeds       -> two different violations
 *
 * Usage (as root, hvx loaded):
 *   hvx_stack_verify <bzImage> <initramfs-kafka.cpio.gz> <stack-disk.img> ["cmdline"] [memMiB] [mode]
 */
#include "hvx_boot.h"
#include <stdlib.h>
#include <errno.h>
#include <sys/stat.h>

/* Printed by the real root's /sbin/init right after the oracle -- the stop point. */
#define FINAL_MARKER    "hydo-stack-marker: workload complete"
/* Printed by the workload phase after the acked produce + SIGSTOP -- boot1's stop. */
#define WORKLOAD_MARKER "hydo-stack-workload-done"
/* Printed in the ORACLE phase after mount but BEFORE the broker recovers the log -- the
 * branch mid-point (so a branch reproduces the whole log recovery + oracle). */
#define MID_MARKER      "hydo-stack: recovering from log"
/* M36 fault scenarios: branch point in a SOLO boot -- every service up and provisioned,
 * no order placed yet, no fault injected yet. */
#define SOLO_MID_MARKER "hydo-stack: provisioning synced"

#define ROOTFS_CMDLINE \
	"root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

/* 1024 MiB = exactly 512 EPT PT pages = a SINGLE EPT PD (no M28 PD fan-out). The >1 GiB
 * fan-out corrupts guest memory under real Linux (a genuine hvx-core bug, handed off
 * separately), so the flagship runs at <=1 GiB. The stack fits: Kafka broker -Xmx256m + MySQL (32 MiB buffer pool) + Redis + the Go front door
 * -Xmx160m + format -Xmx128m (sequential) + Scala + OS + page cache. */
#define ROOTFS_MEM_MIB 1024UL
#define BOOT_TSC_STEP  0x10000ULL
#define UBUF_CAP       (1UL << 20)
#define MAX_RUN_ITERS  8000000000ULL    /* MySQL free-run boots take MANY throttle windows */

/* Lying-disk DROP probability for F3 (seed-sensitive). */
#define DROP_PROB    8u
#define SEED_A       0x5A17A0ULL
#define SEED_B       0x5A17A1ULL

/* JIT config for the guest JVM (M32: proven bit-deterministic; c1sync is the fastest
 * deterministic). Overridable via env HVX_JIT. */
static const char *g_jit = "c1sync";
/* M36: extra guest cmdline tokens from $HVX_STACK_FAULT, e.g. "hydo_kill=kafka@10" or
 * "hydo_net=mysql:loss,30%" -- appended to every boot of the run. */
static const char *g_fault = "";

/* The DURABLE-outcome fingerprint (the oracle-verdict console + the persisted rw log), set
 * as a side effect of every fingerprint() call. Unlike the full state it excludes guest RAM,
 * so it is invariant to the post-marker idle bookkeeping (jiffies/scheduler) that the
 * synthetic idle-heartbeat advances between the marker and the harness's throttle-boundary
 * stop -- used to prove a branch reproduces the DURABILITY OUTCOME even though the full-RAM
 * endpoint of a branch vs a separate straight run differs by that jitter. */
static uint64_t g_durable_fp;

/* ---- canonical fingerprint (FNV-1a) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}

/* ---- file-backed snapshot save buffer (M32 Part 2): keep the double-buffer off L1 RAM ---- */
struct savebuf {
	int fd;
	int use_file;
	uint8_t *mem;
	uint64_t size;
	char path[288];
};
static const char *snap_dir(void)
{
	const char *d = getenv("HVX_SNAP_DIR");
	return (d && d[0]) ? d : "/var/tmp/hvx-snap";
}
static int savebuf_open(struct savebuf *sb, uint64_t size, const char *tag)
{
	static unsigned seq;
	memset(sb, 0, sizeof(*sb));
	sb->fd = -1;
	sb->size = size;
	if (getenv("HVX_SNAP_MALLOC")) { sb->use_file = 0; sb->mem = malloc(size); return sb->mem ? 0 : -1; }
	sb->use_file = 1;
	mkdir(snap_dir(), 0700);
	snprintf(sb->path, sizeof(sb->path), "%.240s/save-%d-%.16s-%u.bin", snap_dir(), (int)getpid(), tag, seq++);
	sb->fd = open(sb->path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (sb->fd < 0) { perror("savebuf open (HVX_SNAP_DIR on a real disk?)"); return -1; }
	if (ftruncate(sb->fd, (off_t)size) < 0) { perror("ftruncate"); return -1; }
	unlink(sb->path);
	return 0;
}
static int savebuf_store(struct savebuf *sb, const void *src, uint64_t size)
{
	uint64_t off = 0;
	const uint8_t *p = src;
	if (!sb->use_file) { memcpy(sb->mem, src, size); return 0; }
	while (off < size) {
		ssize_t w = pwrite(sb->fd, p + off, size - off, (off_t)off);
		if (w < 0) { if (errno == EINTR) continue; perror("pwrite save"); return -1; }
		off += (uint64_t)w;
	}
	fdatasync(sb->fd);
	posix_fadvise(sb->fd, 0, (off_t)size, POSIX_FADV_DONTNEED);
	return 0;
}
static int savebuf_load(struct savebuf *sb, void *dst, uint64_t size)
{
	uint64_t off = 0;
	uint8_t *p = dst;
	if (!sb->use_file) { memcpy(dst, sb->mem, size); return 0; }
	while (off < size) {
		ssize_t r = pread(sb->fd, p + off, size - off, (off_t)off);
		if (r < 0) { if (errno == EINTR) continue; perror("pread restore"); return -1; }
		if (r == 0) break;
		off += (uint64_t)r;
	}
	return 0;
}
static void savebuf_close(struct savebuf *sb)
{
	if (sb->use_file) { if (sb->fd >= 0) close(sb->fd); }
	else free(sb->mem);
	sb->fd = -1; sb->mem = NULL;
}

struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *disk_mm;	/* zero-copy disk window (shared with the kernel pages) */
	uint64_t disk_len;
	uint8_t *ubuf;		/* reconstructed console */
	size_t ulen;
	size_t drained;
	const uint8_t *base;	/* base disk image to (re)load (p1 ro-root + pristine p2) */
	uint64_t p1_off, p1_len, p2_off, p2_len;
	uint64_t fault_seed;
	uint32_t fault_classes;
	uint32_t fault_prob;
};

static long mem_find(const uint8_t *hay, size_t hlen, const char *needle)
{
	size_t nlen = strlen(needle), i;
	if (nlen == 0 || hlen < nlen)
		return -1;
	for (i = 0; i + nlen <= hlen; i++)
		if (memcmp(hay + i, needle, nlen) == 0)
			return (long)i;
	return -1;
}
static int oracle_verdict(const struct boot *b, char *out, size_t outsz)
{
	long at = mem_find(b->ubuf, b->ulen, "HYDO-ORACLE:");
	size_t i = 0;
	if (at < 0)
		return 0;
	while ((size_t)at + i < b->ulen && b->ubuf[at + i] != '\n' && i + 1 < outsz) { out[i] = (char)b->ubuf[at + i]; i++; }
	out[i] = '\0';
	return 1;
}
static int verdict_is_ok(const char *v)        { return strstr(v, "OK") != NULL; }
static int verdict_is_violation(const char *v) { return strstr(v, "VIOLATION") != NULL; }

/* Capture the client's "produced acked=.../consumed keys=..." line for diagnostics. */
static void mysql_line(const struct boot *b, char *out, size_t outsz)
{
	long at = mem_find(b->ubuf, b->ulen, "HYDO-STACK:");
	size_t i = 0;
	out[0] = '\0';
	if (at < 0) return;
	while ((size_t)at + i < b->ulen && b->ubuf[at + i] != '\n' && i + 1 < outsz) { out[i] = (char)b->ubuf[at + i]; i++; }
	out[i] = '\0';
}

/* Parse the MBR (build-linux.sh §2l layout) to recover p1/p2 byte ranges. */
static int mbr_partitions(const uint8_t *img, size_t img_len, struct boot *b)
{
	uint32_t p1_lba, p1_n, p2_lba, p2_n;
	if (img_len < 512 || img[0x1fe] != 0x55 || img[0x1ff] != 0xaa) {
		fprintf(stderr, "hvx_stack_verify: image has no MBR boot signature\n");
		return -1;
	}
	p1_lba = (uint32_t)img[0x1be + 8] | (uint32_t)img[0x1be + 9] << 8 | (uint32_t)img[0x1be + 10] << 16 | (uint32_t)img[0x1be + 11] << 24;
	p1_n   = (uint32_t)img[0x1be + 12] | (uint32_t)img[0x1be + 13] << 8 | (uint32_t)img[0x1be + 14] << 16 | (uint32_t)img[0x1be + 15] << 24;
	p2_lba = (uint32_t)img[0x1ce + 8] | (uint32_t)img[0x1ce + 9] << 8 | (uint32_t)img[0x1ce + 10] << 16 | (uint32_t)img[0x1ce + 11] << 24;
	p2_n   = (uint32_t)img[0x1ce + 12] | (uint32_t)img[0x1ce + 13] << 8 | (uint32_t)img[0x1ce + 14] << 16 | (uint32_t)img[0x1ce + 15] << 24;
	if (p1_n == 0 || p2_n == 0) { fprintf(stderr, "hvx_stack_verify: MBR partition table empty\n"); return -1; }
	b->p1_off = (uint64_t)p1_lba * 512; b->p1_len = (uint64_t)p1_n * 512;
	b->p2_off = (uint64_t)p2_lba * 512; b->p2_len = (uint64_t)p2_n * 512;
	if (b->p2_off + b->p2_len > b->disk_len || b->p1_off + b->p1_len > b->disk_len) {
		fprintf(stderr, "hvx_stack_verify: partition ranges exceed disk size\n"); return -1;
	}
	return 0;
}

static struct hvx_console con_scratch;

/*
 * Read the module's console buffer, retrying until it succeeds. HVX_GET_CONSOLE kzalloc's a
 * 64 KiB + 4 struct in the kernel, an order-5 contiguous allocation, which fails with ENOMEM
 * when host memory is fragmented (two 1 GiB VMs plus a full page cache, as in a 2-worker
 * explore run: "hvx_explore: page allocation failure: order:5" in the kernel log). A drain
 * that silently gave up on that return did not see a marker that had just appeared, so the
 * harness stopped one THROTTLE later than usual: same console and verdict, a different end
 * state and fingerprint (milestone 57: the two M56 depth-1 points that did not reproduce).
 * Returns 0, or -1 after ~10 s of failures (the caller cannot know where the guest is).
 *
 * Diagnostics: HVX_DRAIN_NO_RETRY=1 restores the old give-up behaviour; HVX_SIM_DRAIN_DROP=
 * <marker> treats the FIRST successful read whose text contains <marker> as failed (once per
 * process), which is what an allocation failure on that return looked like.
 */
static int console_fetch(struct boot *b)
{
	static int sim_dropped;
	const char *sim = getenv("HVX_SIM_DRAIN_DROP");
	int tries;
	for (tries = 0; ; tries++) {
		int bad = ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0;
		if (!bad && sim && *sim && !sim_dropped &&
		    mem_find(con_scratch.buf, con_scratch.len < HVX_CONSOLE_SZ ? con_scratch.len : HVX_CONSOLE_SZ, sim) >= 0) {
			sim_dropped = 1;
			bad = 1;
			errno = ENOMEM;
			fprintf(stderr, "hvx_stack_verify: HVX_SIM_DRAIN_DROP: dropped the read that first shows \"%s\"\n", sim);
		}
		if (!bad) return 0;
		if (getenv("HVX_DRAIN_NO_RETRY")) return -1;
		if (tries == 0 || tries % 1000 == 0)
			fprintf(stderr, "hvx_stack_verify: HVX_GET_CONSOLE failed (%s), retrying\n", strerror(errno));
		if (tries >= 10000) {
			fprintf(stderr, "hvx_stack_verify: HVX_GET_CONSOLE still failing after %d tries\n", tries);
			return -1;
		}
		usleep(1000);
	}
}

static void console_drain(struct boot *b)
{
	size_t modlen, add;
	if (console_fetch(b) < 0) {
		if (getenv("HVX_DRAIN_NO_RETRY")) return;	/* diagnostic: the old behaviour */
		/* stopping anywhere else would give a wrong, unreproducible end state */
		fprintf(stderr, "hvx_stack_verify: cannot read the guest console; giving up\n");
		exit(3);
	}
	modlen = con_scratch.len;
	if (modlen > HVX_CONSOLE_SZ) modlen = HVX_CONSOLE_SZ;
	if (modlen <= b->drained) return;
	add = modlen - b->drained;
	if (b->ulen + add > UBUF_CAP) add = UBUF_CAP - b->ulen;
	memcpy(b->ubuf + b->ulen, con_scratch.buf + b->drained, add);
	b->ulen += add;
	b->drained = modlen;
}
static void console_rebase(struct boot *b)
{
	if (console_fetch(b) < 0) {
		if (getenv("HVX_DRAIN_NO_RETRY")) return;
		fprintf(stderr, "hvx_stack_verify: cannot read the guest console; giving up\n");
		exit(3);
	}
	b->drained =con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

/* Write the whole reconstructed guest console to $HVX_CONSOLE_OUT (if set), so a
 * failed run leaves the full log on disk rather than a 280-byte tail. */
static void console_dump(struct boot *b)
{
	const char *path = getenv("HVX_CONSOLE_OUT");
	FILE *f;
	if (!path || !(f = fopen(path, "a"))) return;	/* append: every boot of a run is kept */
	fprintf(f, "\n===== hvx_stack_verify: console of one boot =====\n");
	fwrite(b->ubuf, 1, b->ulen, f);
	fclose(f);
}

static int run_until_marker_inner(struct boot *b, const char *marker);
static int run_until_marker(struct boot *b, const char *marker)
{
	int r = run_until_marker_inner(b, marker);
	console_dump(b);
	return r;
}
static int run_until_marker_inner(struct boot *b, const char *marker)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;
	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR) continue;
			fprintf(stderr, "hvx_stack_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);
		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;
		if (getenv("HVX_PROGRESS") && (iters % 4) == 0) {	/* diagnostic: live console + clock stats */
			struct hvx_pmu pm; memset(&pm, 0, sizeof(pm));
			ioctl(b->fd, HVX_GET_PMU, &pm);
			fprintf(stderr, "[progress: %llu returns, %zu console bytes, pmu branches=%llu ticks=%llu steps=%llu hops=%llu overshoots=%llu]\n",
				iters, b->ulen, (unsigned long long)pm.total, (unsigned long long)pm.ticks,
				(unsigned long long)pm.steps, (unsigned long long)pm.hops, (unsigned long long)pm.overshoots);
			console_dump(b);
		}
		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 2048) == 0)
				fprintf(stderr, "[hvx_stack_verify: %llu throttles, %zu console bytes, still running]\n",
					throttles, b->ulen);
			continue;
		}
		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 280 ? b->ulen - 280 : 0;
			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0) rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_stack_verify: stopped before marker \"%s\": reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason), (unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_stack_verify: console tail: |%.*s|\n", (int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_stack_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

static uint64_t fnv_snapshot(uint64_t h, const struct hvx_snapshot *s)
{
	h = fnv(h, &s->regs, sizeof(s->regs));
	h = fnv(h, &s->tsc, sizeof(s->tsc));
	h = fnv(h, &s->tsc_step, sizeof(s->tsc_step));
	h = fnv(h, &s->tsc_aux, sizeof(s->tsc_aux));
	h = fnv(h, &s->tsc_mode, sizeof(s->tsc_mode));
	h = fnv(h, &s->icount, sizeof(s->icount));
	h = fnv(h, &s->rdtsc_count, sizeof(s->rdtsc_count));
	h = fnv(h, &s->inject_at_icount, sizeof(s->inject_at_icount));
	h = fnv(h, &s->inject_vector, sizeof(s->inject_vector));
	h = fnv(h, &s->inject_valid, sizeof(s->inject_valid));
	h = fnv(h, s->sw_msr, sizeof(s->sw_msr));
	h = fnv(h, &s->sregs, sizeof(s->sregs));
	h = fnv(h, &s->lapic_apic_base, sizeof(s->lapic_apic_base));
	h = fnv(h, &s->lapic_tsc_deadline, sizeof(s->lapic_tsc_deadline));
	h = fnv(h, s->lapic_reg, sizeof(s->lapic_reg));
	h = fnv(h, &s->lapic_in_service, sizeof(s->lapic_in_service));
	h = fnv(h, &s->ioapic_ioregsel, sizeof(s->ioapic_ioregsel));
	h = fnv(h, &s->ioapic_id, sizeof(s->ioapic_id));
	h = fnv(h, s->ioapic_redir, sizeof(s->ioapic_redir));
	h = fnv(h, s->pic_mask, sizeof(s->pic_mask));
	h = fnv(h, &s->uart_ier, 1); h = fnv(h, &s->uart_lcr, 1);
	h = fnv(h, &s->uart_mcr, 1); h = fnv(h, &s->uart_fcr, 1);
	h = fnv(h, &s->uart_scr, 1); h = fnv(h, &s->uart_dll, 1); h = fnv(h, &s->uart_dlm, 1);
	h = fnv(h, &s->vblk_desc, sizeof(s->vblk_desc));
	h = fnv(h, &s->vblk_avail, sizeof(s->vblk_avail));
	h = fnv(h, &s->vblk_used, sizeof(s->vblk_used));
	h = fnv(h, &s->vblk_qsize, sizeof(s->vblk_qsize));
	h = fnv(h, &s->vblk_qready, sizeof(s->vblk_qready));
	h = fnv(h, &s->vblk_last_avail, sizeof(s->vblk_last_avail));
	h = fnv(h, &s->vblk_isr, sizeof(s->vblk_isr));
	h = fnv(h, &s->vblk_irq_pending, sizeof(s->vblk_irq_pending));
	h = fnv(h, &s->vblk_completion_armed, sizeof(s->vblk_completion_armed));
	h = fnv(h, &s->vblk_completion_due, sizeof(s->vblk_completion_due));
	h = fnv(h, &s->vblk_status, sizeof(s->vblk_status));
	h = fnv(h, &s->vblk_pci_command, sizeof(s->vblk_pci_command));
	h = fnv(h, &s->vblk_pci_bar0, sizeof(s->vblk_pci_bar0));
	h = fnv(h, &s->vblk_msix_control, sizeof(s->vblk_msix_control));
	h = fnv(h, &s->vblk_config_msix, sizeof(s->vblk_config_msix));
	h = fnv(h, &s->vblk_queue_msix, sizeof(s->vblk_queue_msix));
	h = fnv(h, s->vblk_msix_tbl, sizeof(s->vblk_msix_tbl));
	h = fnv(h, &s->fault_seed, sizeof(s->fault_seed));
	h = fnv(h, &s->fault_classes, sizeof(s->fault_classes));
	h = fnv(h, &s->fault_probability, sizeof(s->fault_probability));
	h = fnv(h, &s->fault_req_seq, sizeof(s->fault_req_seq));
	h = fnv(h, &s->fault_count, sizeof(s->fault_count));
	h = fnv(h, s->irr, sizeof(s->irr));	/* pending one-shot vectors */
	if (s->pmu_next) {	/* PMU branch clock state (zero when the clock is off) */
		h = fnv(h, &s->pmu_total, sizeof(s->pmu_total));
		h = fnv(h, &s->pmu_next, sizeof(s->pmu_next));
	}
	return h;
}

/* Full-state fingerprint: console + snapshot + all guest RAM + ONLY the rw log partition
 * (p2). The big ro-root (p1) is EXCLUDED (static; asserted separately). Sets g_durable_fp
 * (= console + p2, no RAM) as a side effect. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size, dlen = b->p2_len;
	uint64_t dfp;
	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }
	h = fnv(h, "hvx-stack-state-v2", 18);
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
	h = fnv_snapshot(h, s);
	dfp = fnv(0xcbf29ce484222325ULL, b->ubuf, b->ulen);	/* durable = console ... */
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);			/* all guest RAM (zero-copy mmap) */
	h = fnv(h, &dlen, sizeof(dlen));
	h = fnv(h, b->disk_mm + b->p2_off, b->p2_len);		/* rw log disk ONLY (zero-copy) */
	dfp = fnv(dfp, b->disk_mm + b->p2_off, b->p2_len);	/* ... + the persisted log */
	g_durable_fp = dfp;
	free(s);
	return h;
}
static uint64_t ro_root_fp(struct boot *b) { return fnv(0xcbf29ce484222325ULL, b->disk_mm + b->p1_off, b->p1_len); }

/* Boot the guest: attach a zeroed page-array disk, load the base image zero-copy through the
 * disk mmap, optionally arm a seeded fault, then set up long mode + regs. The caller may
 * overwrite disk_mm+p2 (e.g. with a durable log) BEFORE run_until_marker. */
static int boot_setup(struct boot *b, uint64_t mem_size,
		      const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len,
		      const uint8_t *base, uint64_t disk_len, const char *cmdline,
		      uint64_t fault_seed, uint32_t fault_classes, uint32_t fault_prob)
{
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_disk dk;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1; b->mem = MAP_FAILED; b->disk_mm = MAP_FAILED;
	b->mem_size = mem_size; b->base = base; b->disk_len = disk_len;
	b->fault_seed = fault_seed; b->fault_classes = fault_classes; b->fault_prob = fault_prob;
	b->ubuf = malloc(UBUF_CAP);
	if (!b->ubuf) { perror("malloc"); return -1; }
	if (mbr_partitions(base, disk_len, b) < 0) return -1;

	b->fd = open(getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx", O_RDWR);	/* HVX_DEV: a side-by-side module */
	if (b->fd < 0) { perror("open /dev/hvx"); return -1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	cfg.flags = HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT;	/* real glibc userland + tickless-idle progress */
	if (!(getenv("HVX_PAUSE_EXIT") && !strcmp(getenv("HVX_PAUSE_EXIT"), "0")))
		cfg.flags |= HVX_VM_PAUSE_EXIT;		/* InnoDB spin-waits must move the clock */
	/* PMU branch clock: virtual time advances while the guest computes without VM
	 * exits. On by default (HVX_PMU_CLOCK=0 turns it off); a host without a usable
	 * PMU (nested / hybrid) refuses it with ENODEV and we fall back to the exit clock. */
	if (!(getenv("HVX_PMU_CLOCK") && !strcmp(getenv("HVX_PMU_CLOCK"), "0")))
		cfg.flags |= HVX_VM_PMU_CLOCK;
	/* Milestone 41: allow Intel PT tracing of this VM (the explorer's coverage feedback).
	 * The flag alone traces nothing; HVX_SET_PT switches it on. Off unless asked for. */
	if (getenv("HVX_PT_TRACE_VM"))
		cfg.flags |= HVX_VM_PT_TRACE;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) {
		static int noted;
		if (errno != ENODEV || !(cfg.flags & HVX_VM_PMU_CLOCK)) { perror("HVX_CREATE_VM"); return -1; }
		if (!noted++) fprintf(stderr, "hvx_stack_verify: no usable PMU on this host; using the exit-count clock\n");
		cfg.flags &= ~HVX_VM_PMU_CLOCK;
		if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }
	}

	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap RAM"); return -1; }
	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }

	memset(&dk, 0, sizeof(dk));
	dk.len = disk_len; dk.data = 0; dk.flags = HVX_DISK_NO_IDMAP;	/* zeroed page array; fill via mmap */
	if (ioctl(b->fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return -1; }

	b->disk_mm = mmap(NULL, disk_len, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, (off_t)HVX_DISK_MMAP_PGOFF * 4096);
	if (b->disk_mm == MAP_FAILED) { perror("mmap disk window"); return -1; }
	memcpy(b->disk_mm, base, disk_len);		/* load the partitioned image zero-copy */

	if (fault_classes) {
		struct hvx_fault f;
		memset(&f, 0, sizeof(f));
		f.seed = fault_seed; f.classes = fault_classes; f.probability = fault_prob;
		if (ioctl(b->fd, HVX_SET_FAULT, &f) < 0) { perror("HVX_SET_FAULT"); return -1; }
	}

	build_page_tables_and_gdt(b->mem, mem_size);
	if (load_bzimage(b->mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0) return -1;
	fill_sregs(&sregs);
	if (ioctl(b->fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return -1; }
	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(b->fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	return 0;
}
static void boot_teardown(struct boot *b)
{
	if (b->disk_mm != MAP_FAILED && b->disk_mm) munmap(b->disk_mm, b->disk_len);
	if (b->mem != MAP_FAILED && b->mem) munmap(b->mem, b->mem_size);
	if (b->fd >= 0) close(b->fd);
	free(b->ubuf);
	b->ubuf = NULL; b->fd = -1; b->mem = MAP_FAILED; b->disk_mm = MAP_FAILED;
}

static const char *hx(uint64_t v)
{
	static char bufs[8][17];
	static int i;
	char *b = bufs[i++ & 7];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

struct result { uint64_t fp; uint64_t dfp; uint64_t ro_fp; int ok; char verdict[192]; char kline[192]; };

/* Shared inputs threaded through every scenario. */
struct env {
	uint64_t mem_size;
	const uint8_t *image; size_t image_len;
	const uint8_t *initrd; size_t initrd_len;
	const uint8_t *base; uint64_t disk_len;	/* the pristine MBR-partitioned image */
	uint64_t p1_off, p1_len, p2_off, p2_len;
	uint64_t offline_p1_fp;
	const char *cmdbase;
};

static void mkcmd(char *out, size_t n, const struct env *e, const char *phase, const char *flush)
{
	snprintf(out, n, "%s hydo_jit=%s hydo_phase=%s hydo_flush=%s%s%s", e->cmdbase, g_jit, phase, flush,
		 g_fault[0] ? " " : "", g_fault);
}

/* ---- SOLO: one boot, produce+consume+oracle in ONE timeline (no crash). ---- */
static int run_solo(const struct env *e, const char *flush, struct result *r)
{
	struct boot b;
	char cmd[512];
	int rc = -1;
	memset(r, 0, sizeof(*r));
	mkcmd(cmd, sizeof(cmd), e, "solo", flush);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->base, e->disk_len, cmd, 0, 0, 0) == 0 &&
	    run_until_marker(&b, FINAL_MARKER) == 0) {
		r->fp = fingerprint(&b); r->dfp = g_durable_fp; r->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, r->verdict, sizeof(r->verdict));
		mysql_line(&b, r->kline, sizeof(r->kline));
		r->ok = (r->fp != 0);
		rc = 0;
	}
	boot_teardown(&b);
	return rc;
}

/* boot1 WORKLOAD: run MySQL under the fault, produce acks=all, SIGSTOP the broker, and save
 * the DURABLE rw log (disk_mm+p2, the committed pages, EXCLUDING the volatile cache) into sb. */
static int produce_durable(const struct env *e, const char *flush,
			   uint64_t seed, uint32_t classes, uint32_t prob, struct savebuf *sb)
{
	struct boot b;
	char cmd[512];
	int rc = -1;
	mkcmd(cmd, sizeof(cmd), e, "workload", flush);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->base, e->disk_len, cmd, seed, classes, prob) != 0)
		goto out;
	if (run_until_marker(&b, WORKLOAD_MARKER) != 0)
		goto out;
	if (savebuf_store(sb, b.disk_mm + e->p2_off, e->p2_len) != 0)
		goto out;
	{
		uint64_t dh = fnv(0xcbf29ce484222325ULL, b.disk_mm + e->p2_off, e->p2_len);
		fprintf(stderr, "produce_durable[%s cls=%u prob=%u seed=%#llx]: durable-log-hash=%016llx\n",
			flush, classes, prob, (unsigned long long)seed, (unsigned long long)dh);
	}
	rc = 0;
out:
	boot_teardown(&b);
	return rc;
}

/* boot2 ORACLE (fresh VM, NO fault): load base, overlay the durable rw log into p2, boot the
 * broker (recovers its log), consumer re-reads -> {fp, dfp, verdict}. */
static int run_oracle(const struct env *e, const char *flush, struct savebuf *sb, struct result *r)
{
	struct boot b;
	char cmd[512];
	int rc = -1;
	memset(r, 0, sizeof(*r));
	mkcmd(cmd, sizeof(cmd), e, "oracle", flush);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->base, e->disk_len, cmd, 0, 0, 0) != 0)
		goto out;
	if (savebuf_load(sb, b.disk_mm + e->p2_off, e->p2_len) != 0)		/* overlay durable log */
		goto out;
	if (run_until_marker(&b, FINAL_MARKER) != 0)
		goto out;
	r->fp = fingerprint(&b); r->dfp = g_durable_fp; r->ro_fp = ro_root_fp(&b);
	oracle_verdict(&b, r->verdict, sizeof(r->verdict));
	mysql_line(&b, r->kline, sizeof(r->kline));
	r->ok = (r->fp != 0);
	rc = 0;
out:
	boot_teardown(&b);
	return rc;
}

/* Full crash-recover: workload boot -> durable log -> oracle boot. */
static int run_scenario(const struct env *e, const char *flush,
			uint64_t seed, uint32_t classes, uint32_t prob, struct result *r)
{
	struct savebuf sb;
	int rc = -1;
	memset(r, 0, sizeof(*r));
	if (savebuf_open(&sb, e->p2_len, "durable") != 0) return -1;
	if (produce_durable(e, flush, seed, classes, prob, &sb) == 0)
		rc = run_oracle(e, flush, &sb, r);
	savebuf_close(&sb);
	return rc;
}

/* Branch-from-snapshot on the ORACLE boot: boot2 to MID_MARKER (mounted, before the broker
 * recovers), snapshot RAM + rw log + state, finish -> ra; restore, finish -> rb.
 * ra.fp == rb.fp proves the recovery replay + rw log + state are captured. */
static int run_oracle_branch(const struct env *e, const char *flush, struct savebuf *durable,
			     struct result *ra, struct result *rb)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
	struct savebuf sb_ram, sb_rw;
	int have_ram = 0, have_rw = 0;
	size_t branch_ulen = 0;
	char cmd[512];
	int rc = -1;
	memset(ra, 0, sizeof(*ra)); memset(rb, 0, sizeof(*rb));
	if (!mid) { perror("alloc"); goto done; }
	if (savebuf_open(&sb_ram, e->mem_size, "br-ram") != 0) goto done;
	have_ram = 1;

	mkcmd(cmd, sizeof(cmd), e, "oracle", flush);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->base, e->disk_len, cmd, 0, 0, 0) != 0)
		goto teardown;
	if (savebuf_load(durable, b.disk_mm + e->p2_off, e->p2_len) != 0) goto teardown;
	if (run_until_marker(&b, MID_MARKER) != 0) goto teardown;

	if (savebuf_open(&sb_rw, e->p2_len, "br-rw") != 0) goto teardown;
	have_rw = 1;
	fprintf(stderr, "hvx_stack_verify: reached mid marker at %zu bytes; snapshotting (RAM + rw log)\n", b.ulen);
	branch_ulen = b.ulen;
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	savebuf_store(&sb_ram, b.mem, e->mem_size);
	savebuf_store(&sb_rw, b.disk_mm + e->p2_off, e->p2_len);

	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		ra->fp = fingerprint(&b); ra->dfp = g_durable_fp; ra->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
		ra->ok = (ra->fp != 0);
	}
	if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	savebuf_load(&sb_ram, b.mem, e->mem_size);
	savebuf_load(&sb_rw, b.disk_mm + e->p2_off, e->p2_len);
	b.ulen = branch_ulen;
	console_rebase(&b);
	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		rb->fp = fingerprint(&b); rb->dfp = g_durable_fp; rb->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, rb->verdict, sizeof(rb->verdict));
		rb->ok = (rb->fp != 0);
	}
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(mid);
	if (have_ram) savebuf_close(&sb_ram);
	if (have_rw) savebuf_close(&sb_rw);
	return rc;
}

/* M36: branch-from-snapshot on a SOLO boot: run to SOLO_MID_MARKER, snapshot RAM + rw disk
 * + state, finish -> ra; restore, finish -> rb. With a fault scenario on the cmdline this
 * proves the injected crash / network fault and everything after it replay exactly. */
static int run_solo_branch(const struct env *e, const char *flush, struct result *ra, struct result *rb)
{
	struct boot b;
	struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
	struct savebuf sb_ram, sb_rw;
	int have_ram = 0, have_rw = 0;
	size_t branch_ulen = 0;
	char cmd[512];
	int rc = -1;
	memset(ra, 0, sizeof(*ra)); memset(rb, 0, sizeof(*rb));
	if (!mid) { perror("alloc"); goto done; }
	if (savebuf_open(&sb_ram, e->mem_size, "sb-ram") != 0) goto done;
	have_ram = 1;
	mkcmd(cmd, sizeof(cmd), e, "solo", flush);
	if (boot_setup(&b, e->mem_size, e->image, e->image_len, e->initrd, e->initrd_len,
		       e->base, e->disk_len, cmd, 0, 0, 0) != 0)
		goto teardown;
	if (run_until_marker(&b, SOLO_MID_MARKER) != 0) goto teardown;
	if (savebuf_open(&sb_rw, e->p2_len, "sb-rw") != 0) goto teardown;
	have_rw = 1;
	fprintf(stderr, "hvx_stack_verify: reached solo mid marker at %zu bytes; snapshotting (RAM + rw disk)\n", b.ulen);
	branch_ulen = b.ulen;
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) { perror("HVX_GET_SNAPSHOT"); goto teardown; }
	savebuf_store(&sb_ram, b.mem, e->mem_size);
	savebuf_store(&sb_rw, b.disk_mm + e->p2_off, e->p2_len);
	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		ra->fp = fingerprint(&b); ra->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, ra->verdict, sizeof(ra->verdict));
		mysql_line(&b, ra->kline, sizeof(ra->kline));
		ra->ok = (ra->fp != 0);
	}
	if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) { perror("HVX_SET_SNAPSHOT"); goto teardown; }
	savebuf_load(&sb_ram, b.mem, e->mem_size);
	savebuf_load(&sb_rw, b.disk_mm + e->p2_off, e->p2_len);
	b.ulen = branch_ulen;
	console_rebase(&b);
	if (run_until_marker(&b, FINAL_MARKER) == 0) {
		rb->fp = fingerprint(&b); rb->ro_fp = ro_root_fp(&b);
		oracle_verdict(&b, rb->verdict, sizeof(rb->verdict));
		mysql_line(&b, rb->kline, sizeof(rb->kline));
		rb->ok = (rb->fp != 0);
	}
	rc = 0;
teardown:
	boot_teardown(&b);
done:
	free(mid);
	if (have_ram) savebuf_close(&sb_ram);
	if (have_rw) savebuf_close(&sb_rw);
	return rc;
}

/* hvx_explore.c includes this file for its boot/snapshot/fingerprint machinery and
 * supplies its own main. */
#ifdef HVX_STACK_NO_MAIN
#define main hvx_stack_verify_main
#endif
int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *disk_path;
	struct env e;
	uint8_t *image = NULL, *initrd = NULL;
	const uint8_t *base = NULL;
	size_t image_len = 0, initrd_len = 0;
	uint64_t disk_len = 0;
	struct boot geom;
	int rc = 1;
	const char *mode = "all", *ev;
	int do_sanity = 0, do_replay = 0, do_f1 = 0, do_f2 = 0, do_f3 = 0;
	struct result san, rep;
	struct result f1a, f1b;
	struct result f2a, f2b, f2ba, f2bb;
	struct result f3a, f3b;
	struct savebuf f2_durable;
	int have_f2dur = 0;
	int ok_sanity = 1, ok_replay = 1, ok_f1 = 1, ok_f2v = 1, ok_f2r = 1, ok_f2b = 1, ok_seed = 1, ok_roroot = 1;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-kafka> <stack-disk.img> [\"cmdline\"] [memMiB] "
			"[all|sanity|replay|f1|f2|f3|fault]  (fault: set HVX_STACK_FAULT, e.g. \"hydo_kill=kafka@10\" or \"hydo_net=mysql:loss,30%%\")\n", argv[0]);
		return 2;
	}
	kernel_path = argv[1]; initrd_path = argv[2]; disk_path = argv[3];
	e.cmdbase  = (argc >= 5 && argv[4][0]) ? argv[4] : ROOTFS_CMDLINE;
	e.mem_size = ((argc >= 6) ? (uint64_t)strtoull(argv[5], NULL, 0) : ROOTFS_MEM_MIB) * 1024ULL * 1024ULL;
	if (argc >= 7 && argv[6][0]) mode = argv[6];
	if ((ev = getenv("HVX_JIT")) && ev[0]) g_jit = ev;
	if ((ev = getenv("HVX_STACK_FAULT")) && ev[0]) g_fault = ev;

	if      (strcmp(mode, "all") == 0)    { do_sanity = do_f1 = do_f2 = do_f3 = 1; }
	else if (strcmp(mode, "sanity") == 0) do_sanity = 1;
	else if (strcmp(mode, "replay") == 0) do_replay = 1;
	else if (strcmp(mode, "f1") == 0)     do_f1 = 1;
	else if (strcmp(mode, "f2") == 0)     do_f2 = 1;
	else if (strcmp(mode, "f3") == 0)     do_f3 = 1;
	else if (strcmp(mode, "fault") != 0) { fprintf(stderr, "unknown mode %s\n", mode); return 2; }

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	/* mmap the base disk read-only (page-cache backed, evictable) instead of a 1.2 GiB malloc */
	{
		int dfd = open(disk_path, O_RDONLY);
		struct stat st;
		if (dfd < 0 || fstat(dfd, &st) < 0) { perror("open/stat disk"); free(image); free(initrd); return 1; }
		disk_len = (uint64_t)st.st_size;
		base = mmap(NULL, disk_len, PROT_READ, MAP_PRIVATE, dfd, 0);
		close(dfd);
		if (base == MAP_FAILED) { perror("mmap disk file"); free(image); free(initrd); return 1; }
	}

	memset(&geom, 0, sizeof(geom));
	geom.disk_len = disk_len;
	if (mbr_partitions(base, disk_len, &geom) < 0) { free(image); free(initrd); return 1; }
	e.image = image; e.image_len = image_len;
	e.initrd = initrd; e.initrd_len = initrd_len;
	e.base = base; e.disk_len = disk_len;
	e.p1_off = geom.p1_off; e.p1_len = geom.p1_len; e.p2_off = geom.p2_off; e.p2_len = geom.p2_len;
	e.offline_p1_fp = fnv(0xcbf29ce484222325ULL, base + geom.p1_off, geom.p1_len);

	fprintf(stderr, "hvx_stack_verify: mode=%s jit=%s mem=%llu MiB\n", mode, g_jit,
		(unsigned long long)(e.mem_size / (1024 * 1024)));
	fprintf(stderr, "hvx_stack_verify: bzImage=%zuB initramfs=%zuB stack-disk=%lluB ; p1(ro root) %llu MiB @ %#llx ; "
		"p2(rw log) %llu MiB @ %#llx ; ro-root hash=%s\n",
		image_len, initrd_len, (unsigned long long)disk_len,
		(unsigned long long)(e.p1_len >> 20), (unsigned long long)e.p1_off,
		(unsigned long long)(e.p2_len >> 20), (unsigned long long)e.p2_off, hx(e.offline_p1_fp));

	memset(&san,0,sizeof(san)); memset(&rep,0,sizeof(rep));
	memset(&f1a,0,sizeof(f1a)); memset(&f1b,0,sizeof(f1b));
	memset(&f2a,0,sizeof(f2a)); memset(&f2b,0,sizeof(f2b)); memset(&f2ba,0,sizeof(f2ba)); memset(&f2bb,0,sizeof(f2bb));
	memset(&f3a,0,sizeof(f3a)); memset(&f3b,0,sizeof(f3b));

	/* ---------------- SANITY: flush=off, no fault, no crash -> OK ---------------- */
	if (strcmp(mode, "fault") == 0) {
		/* M36: one fault scenario (from $HVX_STACK_FAULT) on a SOLO boot: two fresh runs +
		 * a branch from the pre-fault snapshot must all be bit-identical. */
		struct result r1, r2, ba, bb;
		int ok;
		fprintf(stderr, "\n=== FAULT scenario [%s]: run #1 ===\n", g_fault);
		run_solo(&e, "on", &r1);
		fprintf(stderr, "\n=== FAULT scenario [%s]: run #2 ===\n", g_fault);
		run_solo(&e, "on", &r2);
		fprintf(stderr, "\n=== FAULT scenario [%s]: branch from the pre-fault snapshot ===\n", g_fault);
		run_solo_branch(&e, "on", &ba, &bb);
		ok = r1.ok && r2.ok && ba.ok && bb.ok && r1.fp == r2.fp && ba.fp == bb.fp && ba.fp == r1.fp &&
		     r1.ro_fp == e.offline_p1_fp && r2.ro_fp == e.offline_p1_fp;
		printf("FAULT [%s] verdict=|%s|\n", g_fault, r1.verdict);
		printf("FAULT [%s] client=|%s|\n", g_fault, r1.kline);
		printf("FAULT [%s] run fp1=%s fp2=%s branch fp_a=%s fp_b=%s -> %s\n", g_fault,
		       hx(r1.fp), hx(r2.fp), hx(ba.fp), hx(bb.fp),
		       ok ? "REPRODUCED (two runs + branch identical)" : "MISMATCH");
		if (!ok) fprintf(stderr, "hvx_stack_verify: FAIL: fault scenario not reproduced\n");
		else printf("hvx_stack_verify: mode=fault PASS\n");
		return ok ? 0 : 1;
	}
	if (do_sanity) {
		fprintf(stderr, "\n=== SANITY: stack solo, NO fault, NO crash -- expect HYDO-ORACLE: OK ===\n");
		run_solo(&e, "off", &san);
		ok_sanity = san.ok && verdict_is_ok(san.verdict);
		printf("SANITY  (solo, no-crash)   verdict=|%s|\n", san.verdict);
		printf("SANITY  client=|%s| fp=%s -> %s\n", san.kline, hx(san.fp),
		       ok_sanity ? "OK (gRPC + MySQL + Kafka + Redis consistent)" : "UNEXPECTED");
	}

	/* ---------------- REPLAY: two fresh solo boots bit-identical (cheap determinism) -------- */
	if (do_replay) {
		fprintf(stderr, "\n=== REPLAY: stack solo boot #1 ===\n");
		run_solo(&e, "off", &san);
		fprintf(stderr, "\n=== REPLAY: stack solo boot #2 ===\n");
		run_solo(&e, "off", &rep);
		ok_replay = san.ok && rep.ok && san.fp == rep.fp;
		ok_sanity = san.ok && verdict_is_ok(san.verdict);
		printf("REPLAY  verdict=|%s|\n", san.verdict);
		printf("REPLAY  fp1=%s fp2=%s -> %s\n", hx(san.fp), hx(rep.fp), ok_replay ? "IDENTICAL (deterministic)" : "MISMATCH");
		printf("    boot1 |%s|\n    boot2 |%s|\n", san.kline, rep.kline);
	}

	/* ---------------- F1: DURABLE CONFIG (flush=on) survives the power loss ------- */
	if (do_f1) {
		fprintf(stderr, "\n=== F1: flush=on + volatile-cache power loss -- crash-recover #1 (OK baseline) ===\n");
		run_scenario(&e, "on", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f1a);
		fprintf(stderr, "\n=== F1: flush=on -- crash-recover #2 (replay bit-for-bit) ===\n");
		run_scenario(&e, "on", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f1b);
		ok_f1 = f1a.ok && f1b.ok && verdict_is_ok(f1a.verdict) && f1a.fp == f1b.fp;
		printf("F1 FLUSH=ON verdict=|%s|\n", f1a.verdict);
		printf("F1 FLUSH=ON fp1=%s fp2=%s -> replay %s (durable config SURVIVES)\n",
		       hx(f1a.fp), hx(f1b.fp), (f1a.ok && f1b.ok && f1a.fp == f1b.fp) ? "IDENTICAL" : "MISMATCH");
	}

	/* ---------------- F2: DOCUMENTED WINDOW (flush=off) loses ---------------------- */
	if (do_f2) {
		fprintf(stderr, "\n=== F2: flush=off + SAME power loss -- crash-recover #1 (the failing run) ===\n");
		run_scenario(&e, "off", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f2a);
		fprintf(stderr, "\n=== F2: flush=off -- crash-recover #2 (replay bit-for-bit) ===\n");
		run_scenario(&e, "off", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f2b);
		fprintf(stderr, "\n=== F2: flush=off -- branch-from-snapshot across the log recovery ===\n");
		if (savebuf_open(&f2_durable, e.p2_len, "f2dur") == 0) {
			have_f2dur = 1;
			if (produce_durable(&e, "off", 0, HVX_FAULT_DROP_UNFLUSHED, 100, &f2_durable) == 0)
				run_oracle_branch(&e, "off", &f2_durable, &f2ba, &f2bb);
		}
		ok_f2v = f2a.ok && verdict_is_violation(f2a.verdict);
		ok_f2r = f2a.ok && f2b.ok && f2a.fp == f2b.fp;
		ok_f2b = f2ba.ok && f2bb.ok && f2ba.fp == f2bb.fp && f2ba.dfp == f2bb.dfp &&
			 f2ba.dfp == f2a.dfp && verdict_is_violation(f2ba.verdict);
		printf("F2 FLUSH=OFF verdict=|%s|\n", f2a.verdict);
		printf("F2 FLUSH=OFF fp1=%s fp2=%s -> replay %s\n", hx(f2a.fp), hx(f2b.fp), ok_f2r ? "IDENTICAL" : "MISMATCH");
		printf("F2 FLUSH=OFF branch fp_a=%s fp_b=%s (round-trip %s) ; durable dfp_a=%s straight-dfp=%s -> %s\n",
		       hx(f2ba.fp), hx(f2bb.fp), (f2ba.fp == f2bb.fp) ? "IDENTICAL" : "MISMATCH",
		       hx(f2ba.dfp), hx(f2a.dfp), ok_f2b ? "OUTCOME REPRODUCED" : "MISMATCH");
	}

	/* ---------------- F3: SEED-SENSITIVITY (lying-disk DROP) ---------------------- */
	if (do_f3) {
		fprintf(stderr, "\n=== F3: seed-sensitive LYING-DISK DROP (flush=on) seed=0x%llx ===\n", (unsigned long long)SEED_A);
		run_scenario(&e, "on", SEED_A, HVX_FAULT_DROP, DROP_PROB, &f3a);
		fprintf(stderr, "\n=== F3: DIFFERENT SEED seed=0x%llx ===\n", (unsigned long long)SEED_B);
		run_scenario(&e, "on", SEED_B, HVX_FAULT_DROP, DROP_PROB, &f3b);
		ok_seed = f3a.ok && f3b.ok && verdict_is_violation(f3a.verdict) &&
			  verdict_is_violation(f3b.verdict) && f3a.fp != f3b.fp;
		printf("F3 SEED-A verdict=|%s| fp=%s\n", f3a.verdict, hx(f3a.fp));
		printf("F3 SEED-B verdict=|%s| fp=%s -> %s\n", f3b.verdict, hx(f3b.fp),
		       ok_seed ? "DIFFERENT+REPRODUCIBLE" : "UNEXPECTED");
	}

	/* ro-root static across every boot that ran */
	ok_roroot = (!do_sanity || san.ro_fp == e.offline_p1_fp) &&
		    (!do_replay || (san.ro_fp == e.offline_p1_fp && rep.ro_fp == e.offline_p1_fp)) &&
		    (!do_f1     || (f1a.ro_fp == e.offline_p1_fp && f1b.ro_fp == e.offline_p1_fp)) &&
		    (!do_f2     || (f2a.ro_fp == e.offline_p1_fp)) &&
		    (!do_f3     || (f3a.ro_fp == e.offline_p1_fp && f3b.ro_fp == e.offline_p1_fp));
	printf("ro-root static offline=%s -> %s\n", hx(e.offline_p1_fp),
	       ok_roroot ? "STABLE (excluded from snapshot, re-attached identically)" : "DRIFTED");

	if (ok_sanity && ok_replay && ok_f1 && ok_f2v && ok_f2r && ok_f2b && ok_seed && ok_roroot) {
		if (strcmp(mode, "all") == 0)
			printf("hvx_stack_verify: MILESTONE 35 PASS: UNMODIFIED MySQL 8.4 + Kafka 3.5 + Redis 7.2 behind a "
			       "gRPC front door, co-located over 127.0.0.1 loopback, data on a journaled ext4 fs. Under a faithful "
			       "volatile-cache power cut the all-durable configuration SURVIVES (F1 OK, baseline %s), while relaxing "
			       "ONE store (MySQL trx_commit=2) leaves the stores DISAGREEING about acked orders (F2 VIOLATION, "
			       "baseline %s) and the failure REPLAYS + BRANCHES bit-for-bit; a seeded lying-disk DROP breaks the "
			       "system differently per seed (F3 %s vs %s).\n",
			       hx(f1a.fp), hx(f2a.fp), hx(f3a.fp), hx(f3b.fp));
		else
			printf("hvx_stack_verify: mode=%s PASS\n", mode);
		rc = 0;
	} else {
		fprintf(stderr, "hvx_stack_verify: FAIL: sanity=%d replay=%d f1=%d f2[verdict=%d repro=%d branch=%d] seed=%d ro-root=%d\n",
			ok_sanity, ok_replay, ok_f1, ok_f2v, ok_f2r, ok_f2b, ok_seed, ok_roroot);
	}

	if (have_f2dur) savebuf_close(&f2_durable);
	free(image); free(initrd);
	return rc;
}

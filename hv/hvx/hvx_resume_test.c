// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_resume_test - DEEP-SNAPSHOT-RESUME reproducer.
 *
 * The existing verify harnesses prove either (a) a SHALLOW branch (snapshot in
 * early kernel, resume), or (b) a DEEP round-trip that only RE-FINGERPRINTS and
 * never continues execution. Nothing has ever RESTORED a deep multi-process
 * userspace snapshot AND CONTINUED it. The search loop needs exactly that, and
 * it hangs: the restored guest idles and never re-runs its driver.
 *
 * The purest failure mode (from the search harness's own comments): a guest
 * IDLE-BLOCKED on a serial read, fed input AFTER restore. M18 proves feeding a
 * LIVE idle guest works; the untested path is feed-AFTER-restore.
 *
 *   1. boot to a SNAP marker (guest up and idle-blocked)
 *   2. GET_SNAPSHOT + save RAM [+ disk]     (capture the "ready" snapshot)
 *   3. SET_SNAPSHOT + restore RAM [+ disk]  (restore from it, in place)
 *   4. [optional] HVX_PUT_INPUT             (feed the event AFTER restore)
 *   5. RESUME (HVX_RUN) and try to reach FINAL within a cap
 *      -> "RESUME OK"   if it makes progress to FINAL
 *      -> "RESUME HANG" if it cycles without progress (the bug)
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_resume_test hvx_resume_test.c
 * Run:  sudo ./hvx_resume_test bzImage initramfs {disk|-} "snap" "final" ["input\n"]
 */
#include "hvx_boot.h"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)

struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *ubuf;
	size_t ulen;
	size_t drained;
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

static struct hvx_console con_scratch;
static void console_drain(struct boot *b)
{
	size_t modlen, add;

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0) {
		/* never ignore a failed read: it would move the stop point (M58) */
		perror("HVX_GET_CONSOLE");
		exit(1);
	}
	modlen = con_scratch.len;
	if (modlen > HVX_CONSOLE_SZ)
		modlen = HVX_CONSOLE_SZ;
	if (modlen <= b->drained)
		return;
	add = modlen - b->drained;
	if (b->ulen + add > UBUF_CAP)
		add = UBUF_CAP - b->ulen;
	memcpy(b->ubuf + b->ulen, con_scratch.buf + b->drained, add);
	b->ulen += add;
	b->drained = modlen;
}

static int run_until_marker(struct boot *b, const char *marker, unsigned long long cap)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;

	for (iters = 0; iters < cap; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_resume_test: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);
		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;
		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 8192) == 0)
				fprintf(stderr, "[%llu throttles, %zu console bytes]\n",
					throttles, b->ulen);
		}
	}
	return -1;
}

static void console_tail(struct boot *b, int n)
{
	size_t from = b->ulen > (size_t)n ? b->ulen - n : 0;
	fprintf(stderr, "console tail: |%.*s|\n", (int)(b->ulen - from), b->ubuf + from);
}

static uint8_t *slurp(const char *path, size_t *len)
{
	FILE *f = fopen(path, "rb");
	uint8_t *buf; long sz;
	if (!f) { perror(path); return NULL; }
	fseek(f, 0, SEEK_END); sz = ftell(f); fseek(f, 0, SEEK_SET);
	buf = malloc(sz);
	if (!buf || fread(buf, 1, sz, f) != (size_t)sz) { perror("fread"); fclose(f); return NULL; }
	fclose(f);
	*len = sz;
	return buf;
}

/* decode a C-escaped \n / \t / \\ into raw bytes */
static size_t unescape(const char *in, uint8_t *out, size_t cap)
{
	size_t n = 0;
	while (*in && n < cap) {
		if (*in == '\\' && in[1]) {
			in++;
			out[n++] = (*in == 'n') ? '\n' : (*in == 't') ? '\t' : (*in == 'r') ? '\r' : *in;
			in++;
		} else out[n++] = *in++;
	}
	return n;
}

int main(int argc, char **argv)
{
	uint64_t mem_size = 256ULL << 20;
	size_t image_len, initrd_len, disk_len = 0;
	uint8_t *image, *initrd, *disk = NULL, *ram_save, *disk_save = NULL;
	struct hvx_snapshot *snap;
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };
	struct hvx_disk dk;
	struct boot b;
	int have_disk;
	const char *snap_marker, *final_marker, *input = NULL;
	char cmd[512];

	if (argc < 6) {
		fprintf(stderr, "usage: %s bzImage initramfs {disk|-} \"snap\" \"final\" [\"input\\n\"]\n", argv[0]);
		return 2;
	}
	have_disk = strcmp(argv[3], "-") != 0;
	snap_marker = argv[4];
	final_marker = argv[5];
	if (argc > 6) input = argv[6];

	image = slurp(argv[1], &image_len);
	initrd = slurp(argv[2], &initrd_len);
	if (!image || !initrd) return 1;
	if (have_disk) {
		disk = slurp(argv[3], &disk_len);
		if (!disk) return 1;
		disk_save = malloc(disk_len);
		if (!disk_save) { perror("malloc"); return 1; }
	}

	if (have_disk)
		snprintf(cmd, sizeof(cmd), "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_phase=solo hydo_fsync=everysec");
	else
		snprintf(cmd, sizeof(cmd), "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys");

	memset(&b, 0, sizeof(b));
	b.mem = MAP_FAILED;
	b.mem_size = mem_size;
	b.ubuf = malloc(UBUF_CAP);
	ram_save = malloc(mem_size);
	snap = calloc(1, sizeof(*snap));
	if (!b.ubuf || !ram_save || !snap) { perror("malloc"); return 1; }

	b.fd = open("/dev/hvx", O_RDWR);
	if (b.fd < 0) { perror("open /dev/hvx"); return 1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	cfg.flags = HVX_VM_IDLE_HEARTBEAT;
	if (ioctl(b.fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return 1; }

	b.mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b.fd, 0);
	if (b.mem == MAP_FAILED) { perror("mmap"); return 1; }

	if (ioctl(b.fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return 1; }
	if (have_disk) {
		dk.len = disk_len; dk.data = (uint64_t)(uintptr_t)disk; dk.flags = HVX_DISK_NO_IDMAP;
		if (ioctl(b.fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return 1; }
	}

	build_page_tables_and_gdt(b.mem, mem_size);
	if (load_bzimage(b.mem, mem_size, image, image_len, initrd, initrd_len, cmd) < 0)
		return 1;
	fill_sregs(&sregs);
	if (ioctl(b.fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return 1; }
	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(b.fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return 1; }

	fprintf(stderr, "== booting to SNAP marker \"%s\" ==\n", snap_marker);
	if (run_until_marker(&b, snap_marker, 4000000) != 0) {
		fprintf(stderr, "FAIL: never reached snap marker \"%s\"\n", snap_marker);
		console_tail(&b, 400);
		return 1;
	}
	/* let the guest settle into the idle-blocked read a little past the marker */
	{ int i; struct hvx_exit ex; for (i = 0; i < 8; i++) { memset(&ex,0,sizeof(ex)); if (ioctl(b.fd, HVX_RUN, &ex) < 0) break; console_drain(&b);} }
	fprintf(stderr, "== reached snap marker at %zu console bytes; capturing snapshot ==\n", b.ulen);

	if (ioctl(b.fd, HVX_GET_SNAPSHOT, snap) < 0) { perror("HVX_GET_SNAPSHOT"); return 1; }
	memcpy(ram_save, b.mem, mem_size);
	if (have_disk) {
		dk.len = disk_len; dk.data = (uint64_t)(uintptr_t)disk_save; dk.flags = 0;
		if (ioctl(b.fd, HVX_GET_DISK, &dk) < 0) { perror("HVX_GET_DISK"); return 1; }
	}
	fprintf(stderr, "== snapshot captured (icount=%llu inject_valid=%u in_service=%#x tsc_deadline=%#llx) ==\n",
		(unsigned long long)snap->icount, snap->inject_valid,
		snap->lapic_in_service, (unsigned long long)snap->lapic_tsc_deadline);

	if (ioctl(b.fd, HVX_SET_SNAPSHOT, snap) < 0) { perror("HVX_SET_SNAPSHOT"); return 1; }
	memcpy(b.mem, ram_save, mem_size);
	if (have_disk) {
		dk.len = disk_len; dk.data = (uint64_t)(uintptr_t)disk_save; dk.flags = HVX_DISK_NO_IDMAP;
		if (ioctl(b.fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return 1; }
	}
	fprintf(stderr, "== RESTORED from deep snapshot ==\n");

	if (input) {
		struct hvx_input in;
		memset(&in, 0, sizeof(in));
		in.len = unescape(input, in.buf, HVX_INPUT_MAX);
		if (ioctl(b.fd, HVX_PUT_INPUT, &in) < 0) { perror("HVX_PUT_INPUT"); return 1; }
		fprintf(stderr, "== fed %u input bytes AFTER restore; RESUMING to FINAL \"%s\" ==\n",
			in.len, final_marker);
	} else {
		fprintf(stderr, "== RESUMING to FINAL \"%s\" ==\n", final_marker);
	}

	if (run_until_marker(&b, final_marker, 400000) == 0) {
		fprintf(stderr, "RESUME OK: restored deep guest reached FINAL marker\n");
		console_tail(&b, 200);
		return 0;
	}
	fprintf(stderr, "RESUME HANG: restored deep guest never reached FINAL (%zu console bytes)\n", b.ulen);
	console_tail(&b, 400);
	return 3;
}

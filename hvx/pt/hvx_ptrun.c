// SPDX-License-Identifier: GPL-2.0
#define _GNU_SOURCE	/* memmem */
/*
 * hvx_ptrun -- boot a real Linux guest once under hvx with Intel PT tracing of the
 * guest, run it to a console marker, and save what an offline decoder needs:
 *
 *   <outdir>/trace.bin    the raw PT packet stream (guest only)
 *   <outdir>/ram.bin      all guest RAM at the stop point (the code bytes to decode
 *                         against: guest kernel text is at RAM offset VA-0xffffffff80000000)
 *   <outdir>/console.txt  the guest console
 *   <outdir>/pt.txt       trace size, RTIT status/ctl, whether the buffer filled up
 *
 * Usage: hvx_ptrun <bzImage> <initramfs> <outdir> [memMiB]
 * Env:   HVX_DEV        device (default /dev/hvx; /dev/hvxpt for the side-by-side copy)
 *        HVX_PT_FLAGS   "kernel", "user" or "kernel,user" (default kernel)
 *        HVX_PT_R0 / HVX_PT_R1   IP filter ranges "lo-hi" (hex), hi exclusive
 *        HVX_PT_MIB     trace buffer (default 512)
 *        HVX_PT_NORETC  1 = no return compression
 *        HVX_CMDLINE    kernel command line (default: the boot harness's)
 *        HVX_MARKER     stop marker (default: the initramfs shell marker)
 *        HVX_PT_START   start tracing only once the console shows this marker (code the
 *                       kernel patches at run time -- alternatives, jump labels, static
 *                       calls -- is only stable after early boot, and the decoder reads
 *                       the code as it is at the end of the run)
 *
 * The second mode reads the trace a pt_force'd module kept from the last freed VM
 * (for harnesses that know nothing about PT):
 *        hvx_ptrun --last <outfile>
 */
#include "../hvx_boot.h"
#include <errno.h>
#include <sys/stat.h>

#define MARKER_DEFAULT "hydo-boot-marker: launching shell on ttyS0"
#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (8u << 20)
#define MAX_RUN_ITERS 50000000ULL

static const char *devpath(void)
{
	const char *d = getenv("HVX_DEV");
	return (d && d[0]) ? d : "/dev/hvx";
}

static int write_file(const char *path, const void *p, size_t n)
{
	FILE *f = fopen(path, "wb");
	if (!f) { perror(path); return -1; }
	if (n && fwrite(p, 1, n, f) != n) { perror(path); fclose(f); return -1; }
	fclose(f);
	return 0;
}

/* copy the trace (the VM's own, or the kept last one) into a file */
static int dump_trace(int fd, const char *path, struct hvx_pt_status *ps)
{
	uint8_t *buf;
	struct hvx_pt_read rq;

	memset(ps, 0, sizeof(*ps));
	if (ioctl(fd, HVX_GET_PT, ps) < 0) { perror("HVX_GET_PT"); return -1; }
	buf = malloc(ps->bytes ? ps->bytes : 1);
	if (!buf) { perror("malloc"); return -1; }
	rq.offset = 0; rq.len = ps->bytes; rq.addr = (uint64_t)(uintptr_t)buf;
	if (ps->bytes && ioctl(fd, HVX_READ_PT, &rq) < 0) { perror("HVX_READ_PT"); free(buf); return -1; }
	if (write_file(path, buf, ps->bytes) < 0) { free(buf); return -1; }
	free(buf);
	fprintf(stderr, "hvx_ptrun: trace %llu bytes (buffer %llu, status %#llx%s) -> %s\n",
		(unsigned long long)ps->bytes, (unsigned long long)ps->buf_bytes,
		(unsigned long long)ps->status,
		(ps->status & (1ULL << 5)) ? ", FULL: truncated" : "", path);
	return 0;
}

static void parse_range(const char *env, __u64 *lo, __u64 *hi)
{
	const char *v = getenv(env), *dash;

	*lo = *hi = 0;
	if (!v || !(dash = strchr(v, '-')))
		return;
	*lo = strtoull(v, NULL, 16);
	*hi = strtoull(dash + 1, NULL, 16);
}

int main(int argc, char **argv)
{
	struct hvx_config cfg;
	struct hvx_pt_config pc;
	struct hvx_pt_status ps;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };
	static struct hvx_console con;
	const char *flags_s, *marker, *cmdline, *outdir;
	uint8_t *image, *initrd, *mem, *ubuf;
	size_t image_len, initrd_len, ulen = 0, drained = 0;
	uint64_t mem_size, it;
	char path[4096];
	int fd, started = 0;

	if (argc == 3 && !strcmp(argv[1], "--last")) {
		fd = open(devpath(), O_RDWR);
		if (fd < 0) { perror(devpath()); return 1; }
		return dump_trace(fd, argv[2], &ps) ? 1 : 0;
	}
	if (argc < 4) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs> <outdir> [memMiB] | --last <outfile>\n", argv[0]);
		return 2;
	}
	outdir = argv[3];
	mkdir(outdir, 0755);
	mem_size = (argc >= 5 ? strtoull(argv[4], NULL, 0) : 256) << 20;
	marker = getenv("HVX_MARKER") ? getenv("HVX_MARKER") : MARKER_DEFAULT;
	cmdline = getenv("HVX_CMDLINE") ? getenv("HVX_CMDLINE") : DEFAULT_CMDLINE;
	image = read_file(argv[1], &image_len);
	initrd = read_file(argv[2], &initrd_len);
	ubuf = malloc(UBUF_CAP);
	if (!image || !initrd || !ubuf)
		return 1;

	fd = open(devpath(), O_RDWR);
	if (fd < 0) { perror(devpath()); return 1; }
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	cfg.flags = HVX_VM_PT_TRACE;
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return 1; }

	memset(&pc, 0, sizeof(pc));
	flags_s = getenv("HVX_PT_FLAGS") ? getenv("HVX_PT_FLAGS") : "kernel";
	if (strstr(flags_s, "kernel")) pc.flags |= HVX_PT_KERNEL;
	if (strstr(flags_s, "user"))   pc.flags |= HVX_PT_USER;
	if (getenv("HVX_PT_NORETC") && atoi(getenv("HVX_PT_NORETC"))) pc.flags |= HVX_PT_NORETCOMP;
	pc.buf_mib = getenv("HVX_PT_MIB") ? strtoull(getenv("HVX_PT_MIB"), NULL, 0) : 512;
	parse_range("HVX_PT_R0", &pc.lo[0], &pc.hi[0]);
	parse_range("HVX_PT_R1", &pc.lo[1], &pc.hi[1]);
	if (!getenv("HVX_PT_START") && ioctl(fd, HVX_SET_PT, &pc) < 0) { perror("HVX_SET_PT"); return 1; }

	mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); return 1; }
	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return 1; }
	build_page_tables_and_gdt(mem, mem_size);
	if (load_bzimage(mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0)
		return 1;
	fill_sregs(&sregs);
	if (ioctl(fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return 1; }
	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return 1; }

	for (it = 0; it < MAX_RUN_ITERS; it++) {
		struct hvx_exit ex;
		size_t n;

		memset(&ex, 0, sizeof(ex));
		if (ioctl(fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR) continue;
			perror("HVX_RUN");
			return 1;
		}
		if (ioctl(fd, HVX_GET_CONSOLE, &con) == 0) {
			n = con.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con.len;
			if (n > drained && ulen + (n - drained) <= UBUF_CAP) {
				memcpy(ubuf + ulen, con.buf + drained, n - drained);
				ulen += n - drained;
				drained = n;
			}
		}
		if (memmem(ubuf, ulen, marker, strlen(marker)))
			break;
		if (getenv("HVX_PT_START") && !started &&
		    memmem(ubuf, ulen, getenv("HVX_PT_START"), strlen(getenv("HVX_PT_START")))) {
			if (ioctl(fd, HVX_SET_PT, &pc) < 0) { perror("HVX_SET_PT"); return 1; }
			started = 1;
			fprintf(stderr, "hvx_ptrun: tracing from \"%s\" on\n", getenv("HVX_PT_START"));
		}
		if (ex.reason != HVX_EXIT_THROTTLE) {
			fprintf(stderr, "hvx_ptrun: stopped before the marker: exit reason %u\n", ex.reason);
			return 1;
		}
	}

	snprintf(path, sizeof(path), "%s/console.txt", outdir);
	write_file(path, ubuf, ulen);
	snprintf(path, sizeof(path), "%s/ram.bin", outdir);
	write_file(path, mem, mem_size);
	snprintf(path, sizeof(path), "%s/trace.bin", outdir);
	if (dump_trace(fd, path, &ps) < 0)
		return 1;
	snprintf(path, sizeof(path), "%s/pt.txt", outdir);
	{
		FILE *f = fopen(path, "w");
		if (f) {
			fprintf(f, "bytes=%llu buf_bytes=%llu status=%#llx ctl=%#llx truncated=%d\n",
				(unsigned long long)ps.bytes, (unsigned long long)ps.buf_bytes,
				(unsigned long long)ps.status, (unsigned long long)ps.ctl,
				!!(ps.status & (1ULL << 5)));
			fclose(f);
		}
	}
	return 0;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_repro - minimal reproducers of the hvx.c bugs found by the M97 safety net.
 *
 *   hvx_repro <case>      case = pmu | txlen | unsync | repins | vqloop | pcishift | diskrace | all | bigalloc
 *
 * RUN ONLY INSIDE THE DEBUG TEST VM (hv/hvx/safety/run-debug.sh), never against a shared module
 * on the bare-metal machine: on an unfixed module several of them oops the kernel or hold a CPU
 * with interrupts off forever. On a fixed module every case ends in well under a minute and
 * exits 0; the debug kernel must log no KASAN/UBSAN/lockdep splat while it runs.
 *
 * Build: cc -O2 -static -I.. -o hvx_repro hvx_repro.c -lpthread
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include "hvx_abi.h"

#define RAM (1u << 20)
#define DESC 0x8000
#define AVAIL 0x9000
#define USED 0x30000
#define HDR 0xb000
#define STATUS 0xb100

static const char *dev(void) { const char *d = getenv("HVX_DEV"); return d ? d : "/dev/hvx"; }

static int vm_open(uint8_t **ram)
{
	struct hvx_config cfg = { .mem_size = RAM };
	int fd = open(dev(), O_RDWR);

	if (fd < 0) { perror("open"); exit(2); }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("CREATE_VM"); exit(2); }
	*ram = mmap(NULL, RAM, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (*ram == MAP_FAILED) { perror("mmap"); exit(2); }
	return fd;
}

/* a tiny assembler for the guest program at GPA 0x3000 */
struct as { uint8_t *p; };
static void b(struct as *a, int n, ...)
{
	va_list ap;
	va_start(ap, n);
	while (n--) *a->p++ = (uint8_t)va_arg(ap, int);
	va_end(ap);
}
static void imm32(struct as *a, uint8_t op, uint32_t v) { b(a, 5, op, v, v >> 8, v >> 16, v >> 24); }
static void movabs(struct as *a, int reg, uint64_t v) { b(a, 2, 0x48, 0xb8 + reg); memcpy(a->p, &v, 8); a->p += 8; }
static void out32(struct as *a, uint16_t port, uint32_t v) { imm32(a, 0xba, port); imm32(a, 0xb8, v); b(a, 1, 0xef); }
static void hlt(struct as *a) { b(a, 3, 0xf4, 0xeb, 0xfd); }

/* run until the guest halts (or 64 calls); returns the last exit reason */
static uint32_t run(int fd)
{
	struct hvx_exit ex;
	int i;

	for (i = 0; i < 64; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("RUN"); exit(1); }
		if (ex.reason != HVX_EXIT_THROTTLE)
			break;
	}
	return ex.reason;
}

static void disk(int fd, uint64_t len)
{
	struct hvx_disk dk = { .len = len };

	if (ioctl(fd, HVX_SET_DISK, &dk) < 0) { perror("SET_DISK"); exit(2); }
}

/* Phase-A PIO transport: queue at DESC/AVAIL/USED with the given size, then kick */
static void phase_a_kick(struct as *a, uint32_t qsize)
{
	out32(a, 0x500, DESC); out32(a, 0x504, 0);
	out32(a, 0x508, AVAIL); out32(a, 0x50c, 0);
	out32(a, 0x510, USED); out32(a, 0x514, 0);
	out32(a, 0x518, qsize);
	out32(a, 0x51c, 1);
	out32(a, 0x524, 1);
	hlt(a);
}

struct vdesc { uint64_t addr; uint32_t len; uint16_t flags, next; };

/* 1. HVX_GET_PMU before HVX_CREATE_VM dereferenced the NULL vCPU pointer */
static int c_pmu(void)
{
	struct hvx_pmu pm;
	int fd = open(dev(), O_RDWR);

	if (fd < 0) { perror("open"); return 2; }
	if (ioctl(fd, HVX_GET_PMU, &pm) == 0) { puts("pmu: GET_PMU without a VM succeeded (no crash)"); close(fd); return 0; }
	printf("pmu: GET_PMU without a VM -> %s (fixed)\n", strerror(errno));
	close(fd);
	return 0;
}

/* 2. HVX_SET_SNAPSHOT accepted any vnet_tx_len; GET_TX then copied that many bytes out of the
 *    8 KiB tx_buf (and the next TX capture wrote at that offset) */
static int c_txlen(void)
{
	uint8_t *ram;
	int fd = vm_open(&ram);
	struct hvx_net_config nc = { .flags = HVX_NET_NO_IDMAP };
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	static uint8_t buf[1 << 20];
	struct hvx_tx tx = { .len = sizeof(buf), .data = (uintptr_t)buf };

	if (ioctl(fd, HVX_NET_ATTACH, &nc) < 0 || ioctl(fd, HVX_GET_SNAPSHOT, s) < 0) { perror("setup"); return 2; }
	s->vnet_tx_len = 0x10000;
	if (ioctl(fd, HVX_SET_SNAPSHOT, s) < 0) { printf("txlen: SET_SNAPSHOT with tx_len 0x10000 -> %s (fixed)\n", strerror(errno)); return 0; }
	if (ioctl(fd, HVX_GET_TX, &tx) < 0) { perror("GET_TX"); return 1; }
	printf("txlen: GET_TX returned %llu bytes from an 8192-byte buffer (BUG)\n", (unsigned long long)tx.len);
	return 1;
}

/* 3. HVX_SET_UNSYNC accepted a held write at doff near 2^64; a guest FLUSH then committed it
 *    because doff + len wrapped below the disk size: a write far outside the page array */
static int c_unsync(void)
{
	uint8_t *ram;
	int fd = vm_open(&ram);
	struct hvx_fault f = { .classes = HVX_FAULT_DROP_UNFLUSHED, .probability = 100, .max_delay = 1 };
	static uint8_t blob[sizeof(struct hvx_unsync_rec) + HVX_UNSYNC_ENTBYTES];
	struct hvx_unsync_rec *r = (void *)blob;
	struct hvx_unsync_io io = { .addr = (uintptr_t)blob, .len = sizeof(blob), .n = 1 };
	struct vdesc *d = (void *)(ram + DESC);
	struct as a = { ram + HVX_GUEST_CODE_BASE };
	uint32_t reason;

	disk(fd, 65536);
	if (ioctl(fd, HVX_SET_FAULT, &f) < 0) { perror("SET_FAULT"); return 2; }
	r->doff = 0xfffffffffffff000ULL; r->len = 8192; r->valid = 1;
	memset(blob + sizeof(*r), 0x41, HVX_UNSYNC_ENTBYTES);
	if (ioctl(fd, HVX_SET_UNSYNC, &io) < 0) { printf("unsync: SET_UNSYNC -> %s\n", strerror(errno)); return 0; }
	/* one FLUSH request: header {type 4}, status byte */
	d[0] = (struct vdesc){ HDR, 16, 1, 1 };
	d[1] = (struct vdesc){ STATUS, 1, 2, 0 };
	*(uint32_t *)(ram + HDR) = 4;
	*(uint16_t *)(ram + AVAIL + 2) = 1;
	*(uint16_t *)(ram + AVAIL + 4) = 0;
	phase_a_kick(&a, 4);
	reason = run(fd);
	printf("unsync: guest FLUSH done, exit %u, status %u (no splat = fixed)\n", reason, ram[STATUS]);
	return 0;
}

static void on_alarm(int s) { (void)s; puts("TIMEOUT: HVX_RUN did not return (BUG: host CPU held)"); _exit(1); }

/* 4. REP INS/OUTS looped RCX times in the exit handler, IRQs off, whatever the port */
static int c_repins(void)
{
	uint8_t *ram;
	int fd = vm_open(&ram);
	struct as a = { ram + HVX_GUEST_CODE_BASE };
	uint32_t reason;

	movabs(&a, 7, 0x10000);			/* rdi */
	movabs(&a, 1, 1ULL << 40);		/* rcx */
	imm32(&a, 0xba, 0x80);			/* edx = port 0x80 */
	b(&a, 2, 0xf3, 0x6c);			/* rep insb */
	movabs(&a, 6, 0x10000);			/* rsi */
	movabs(&a, 1, 1ULL << 40);
	imm32(&a, 0xba, 0x3f8);			/* the UART: bytes go to the console */
	b(&a, 2, 0xf3, 0x6e);			/* rep outsb */
	hlt(&a);
	signal(SIGALRM, on_alarm);
	alarm(60);
	reason = run(fd);
	printf("repins: HVX_RUN returned, exit %u (fixed)\n", reason);
	return 0;
}

/* 5. a virtqueue kick walked qsize+2 descriptors per request (qsize is any u32 the guest
 *    writes) for up to 65535 requests: a self-linked descriptor held the CPU for hours */
static int c_vqloop(void)
{
	uint8_t *ram;
	int fd = vm_open(&ram);
	struct vdesc *d = (void *)(ram + DESC);
	struct as a = { ram + HVX_GUEST_CODE_BASE };
	uint32_t reason;

	disk(fd, 65536);
	d[0] = (struct vdesc){ HDR, 16, 1, 0 };	/* NEXT, next = itself */
	*(uint16_t *)(ram + AVAIL + 2) = 0xffff;	/* 65535 requests, all head 0 */
	phase_a_kick(&a, 0x7ffffff0);
	signal(SIGALRM, on_alarm);
	alarm(60);
	reason = run(fd);
	printf("vqloop: HVX_RUN returned, exit %u (fixed)\n", reason);
	return 0;
}

/* 6. a PCI config write at 0xcff with CONFIG_ADDRESS bits 1:0 set shifted by 48 bits (UB) */
static int c_pcishift(void)
{
	uint8_t *ram;
	int fd = vm_open(&ram);
	struct as a = { ram + HVX_GUEST_CODE_BASE };

	disk(fd, 65536);
	out32(&a, 0xcf8, 0x80001803);		/* bus 0, dev 3 (virtio-blk), reg 0, bits 1:0 = 3 */
	imm32(&a, 0xba, 0xcff);
	b(&a, 3, 0xb0, 0x5a, 0xee);		/* mov al, 0x5a; out dx, al */
	hlt(&a);
	printf("pcishift: exit %u (no UBSAN splat = fixed)\n", run(fd));
	return 0;
}

/* 7. the disk-mmap fault handler read the page array without a lock while HVX_SET_DISK
 *    (another thread) freed and replaced it */
static volatile int stop;
static int g_fd;
static void *resizer(void *p)
{
	uint64_t n = 0;

	(void)p;
	while (!stop) {
		struct hvx_disk dk = { .len = (n++ & 1) ? 65536 : 131072 };
		ioctl(g_fd, HVX_SET_DISK, &dk);
	}
	return NULL;
}
static int c_diskrace(void)
{
	uint8_t *ram;
	pthread_t t;
	time_t end = time(NULL) + 15;
	unsigned long faults = 0;

	g_fd = vm_open(&ram);
	disk(g_fd, 65536);
	pthread_create(&t, NULL, resizer, NULL);
	while (time(NULL) < end) {
		volatile uint8_t *m = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, g_fd,
					   (off_t)HVX_DISK_MMAP_PGOFF * 4096);
		int i;

		if (m == MAP_FAILED)
			continue;
		for (i = 0; i < 16; i++)
			faults += m[i * 4096] + 1;	/* each page faults in through hvx_disk_fault */
		munmap((void *)m, 65536);
	}
	stop = 1;
	pthread_join(t, NULL);
	printf("diskrace: %lu page faults against a resizing disk (no KASAN splat = fixed)\n", faults);
	return 0;
}

/* 8. guest RAM (up to 3 GiB) and disk images (up to 4 GiB) larger than the free memory were
 *    allocated with plain GFP_KERNEL: instead of failing, the allocator OOM-killed other
 *    processes and then panicked "System is deadlocked on memory". Run in a VM with less
 *    memory than 3 GiB (run-debug.sh with VM_MEM=2048). */
static int c_bigalloc(void)
{
	struct hvx_config cfg = { .mem_size = 3ULL << 30 };
	int fd = open(dev(), O_RDWR), fd2;
	struct hvx_disk dk = { .len = HVX_DISK_MAX };

	if (fd < 0) { perror("open"); return 2; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0)
		printf("bigalloc: 3 GiB VM -> %s\n", strerror(errno));
	else
		puts("bigalloc: 3 GiB VM created (enough memory here)");
	close(fd);
	fd2 = open(dev(), O_RDWR);
	cfg.mem_size = 1 << 20;
	if (ioctl(fd2, HVX_CREATE_VM, &cfg) < 0) { perror("CREATE_VM"); return 2; }
	if (ioctl(fd2, HVX_SET_DISK, &dk) < 0)
		printf("bigalloc: 4 GiB disk -> %s (fixed)\n", strerror(errno));
	else
		puts("bigalloc: 4 GiB disk allocated (enough memory here)");
	close(fd2);
	return 0;
}

/* 9. page accounting: kmemleak does not track pages from __get_free_page, so this checks
 *    that 200 VM lifecycles (RAM, disk, volatile cache, both mmap windows faulted in, a run)
 *    give their memory back. Fails if MemAvailable drops by more than 64 MiB. */
static long mem_avail_kb(void)
{
	char line[128];
	long v = -1;
	FILE *f = fopen("/proc/meminfo", "r");

	while (f && fgets(line, sizeof(line), f))
		if (sscanf(line, "MemAvailable: %ld kB", &v) == 1)
			break;
	if (f)
		fclose(f);
	return v;
}
static int c_pageleak(void)
{
	long before, after;
	int it;

	before = mem_avail_kb();
	for (it = 0; it < 200; it++) {
		uint8_t *ram;
		int fd = vm_open(&ram);
		struct hvx_fault f = { .classes = HVX_FAULT_DROP_UNFLUSHED, .probability = 100, .max_delay = 1 };
		struct as a = { ram + HVX_GUEST_CODE_BASE };
		volatile uint8_t *dm;
		int i;

		disk(fd, 1 << 20);
		ioctl(fd, HVX_SET_FAULT, &f);
		dm = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)HVX_DISK_MMAP_PGOFF * 4096);
		for (i = 0; dm != MAP_FAILED && i < 256; i++)
			dm[i * 4096] = (uint8_t)i;
		for (i = 0; i < (int)(RAM / 4096); i++)
			ram[i * 4096] |= 0;
		hlt(&a);
		run(fd);
		if (dm != MAP_FAILED)
			munmap((void *)dm, 1 << 20);
		munmap(ram, RAM);
		close(fd);
	}
	sleep(2);
	after = mem_avail_kb();
	printf("pageleak: MemAvailable %ld kB before, %ld kB after 200 VMs (%+ld kB)\n",
	       before, after, after - before);
	return before - after > 64 * 1024;
}

int main(int argc, char **argv)
{
	const char *c = argc > 1 ? argv[1] : "all";
	int rc = 0;

	setvbuf(stdout, NULL, _IONBF, 0);
	if (!strcmp(c, "pmu") || !strcmp(c, "all")) rc |= c_pmu();
	if (!strcmp(c, "txlen") || !strcmp(c, "all")) rc |= c_txlen();
	if (!strcmp(c, "unsync") || !strcmp(c, "all")) rc |= c_unsync();
	if (!strcmp(c, "repins") || !strcmp(c, "all")) rc |= c_repins();
	if (!strcmp(c, "vqloop") || !strcmp(c, "all")) rc |= c_vqloop();
	if (!strcmp(c, "pcishift") || !strcmp(c, "all")) rc |= c_pcishift();
	if (!strcmp(c, "diskrace") || !strcmp(c, "all")) rc |= c_diskrace();
	if (!strcmp(c, "bigalloc")) rc |= c_bigalloc();	/* not in "all": needs a small VM */
	if (!strcmp(c, "pageleak") || !strcmp(c, "all")) rc |= c_pageleak();
	return rc;
}

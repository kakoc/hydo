// SPDX-License-Identifier: GPL-2.0
/*
 * vmcost - the fixed per-VM costs of the explorer's run path, step by step (PERF-RUN-COST.md).
 *
 * hvx_explore's vm_fresh() (hvx_stack_verify.c's boot_setup) does, for every schedule and again
 * for every recovery boot: HVX_CREATE_VM (the module allocates and zeroes all guest RAM pages),
 * mmap of RAM, HVX_SET_DISK (a zeroed page array as large as the whole image), mmap of the disk,
 * a memcpy of the whole base image into it, page tables + bzImage + initramfs into RAM, and the
 * register setup. Then snap_restore() copies all of RAM and the data partition back from the
 * file-backed save buffers, and every fingerprint hashes all of RAM and the data partition with
 * a byte-wise FNV-1a. This tool times each of those steps on its own, in a VM that never runs.
 *
 *   vmcost <bzImage> <initramfs> <disk image> [ram MiB] [repeats]
 *
 * Run it as root through ~/bin/hvx-run (it opens /dev/hvx). It creates one VM at a time.
 */
#define HVX_STACK_NO_MAIN
#include "../hvx_stack_verify.c"
#undef main
#include <time.h>

static double now_s(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec / 1e9;
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
	size_t ilen, rlen, dlen;
	uint8_t *img, *rd, *base;
	uint64_t mem;
	int reps, r;
	struct boot geom;

	if (argc < 4) { fprintf(stderr, "usage: %s <bzImage> <initramfs> <disk> [ram MiB] [repeats]\n", argv[0]); return 2; }
	mem = (argc > 4 ? strtoull(argv[4], NULL, 10) : 1024) << 20;
	reps = argc > 5 ? atoi(argv[5]) : 3;
	if (!(img = map_file(argv[1], &ilen)) || !(rd = map_file(argv[2], &rlen)) || !(base = map_file(argv[3], &dlen)))
		return 1;
	memset(&geom, 0, sizeof(geom));
	geom.disk_len = dlen;
	if (mbr_partitions(base, dlen, &geom) < 0) return 1;
	printf("VMCOST mem=%llu disk=%zu p1=%llu p2=%llu snapstruct=%zu\n", (unsigned long long)mem, dlen,
	       (unsigned long long)geom.p1_len, (unsigned long long)geom.p2_len, sizeof(struct hvx_snapshot));
	{	/* touch the image once so every repeat reads it from the page cache, as the explorer does */
		volatile uint8_t s = 0;
		size_t o;
		for (o = 0; o < dlen; o += 4096) s ^= base[o];
		(void)s;
	}
	for (r = 0; r < reps; r++) {
		struct hvx_config cfg;
		struct hvx_disk dk;
		struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };
		struct hvx_sregs sregs;
		struct hvx_regs regs;
		struct hvx_snapshot *snap = calloc(1, sizeof(*snap));
		struct savebuf sram, sdisk;
		double t0, t_open, t_create, t_mmap, t_disk, t_dmmap, t_dcopy, t_kern, t_regs, t_getsnap, t_setsnap,
		       t_fnvram, t_fnvdisk, t_store_ram, t_load_ram, t_load_ram2, t_store_disk, t_load_disk, t_memcpy_ram,
		       t_munmap, t_close;
		uint8_t *m, *d, *copy;
		uint64_t h;
		int fd;

		t0 = now_s();
		fd = open("/dev/hvx", O_RDWR);
		if (fd < 0) { perror("/dev/hvx"); return 1; }
		t_open = now_s();
		memset(&cfg, 0, sizeof(cfg));
		cfg.mem_size = mem;
		cfg.flags = HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT | HVX_VM_PAUSE_EXIT | HVX_VM_PMU_CLOCK;
		if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return 1; }
		t_create = now_s();
		m = mmap(NULL, mem, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (m == MAP_FAILED) { perror("mmap"); return 1; }
		ioctl(fd, HVX_SET_TSC, &tsc);
		t_mmap = now_s();
		memset(&dk, 0, sizeof(dk));
		dk.len = dlen; dk.flags = HVX_DISK_NO_IDMAP;
		if (ioctl(fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); return 1; }
		t_disk = now_s();
		d = mmap(NULL, dlen, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)HVX_DISK_MMAP_PGOFF * 4096);
		if (d == MAP_FAILED) { perror("mmap disk"); return 1; }
		t_dmmap = now_s();
		memcpy(d, base, dlen);
		t_dcopy = now_s();
		build_page_tables_and_gdt(m, mem);
		if (load_bzimage_n(m, mem, img, ilen, rd, rlen, "console=ttyS0", 1) < 0) return 1;
		t_kern = now_s();
		fill_sregs(&sregs);
		ioctl(fd, HVX_SET_SREGS, &sregs);
		memset(&regs, 0, sizeof(regs));
		regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
		ioctl(fd, HVX_SET_REGS, &regs);
		t_regs = now_s();
		ioctl(fd, HVX_GET_SNAPSHOT, snap);
		t_getsnap = now_s();
		ioctl(fd, HVX_SET_SNAPSHOT, snap);
		t_setsnap = now_s();
		h = fnv(0xcbf29ce484222325ULL, m, mem);
		t_fnvram = now_s();
		h = fnv(h, d + geom.p2_off, geom.p2_len);
		t_fnvdisk = now_s();
		/* the save buffers as the explorer uses them: a file in HVX_SNAP_DIR, written with
		 * fdatasync + FADV_DONTNEED, read back with pread (first from disk, then from cache) */
		if (savebuf_open(&sram, mem, "vmcost") || savebuf_store(&sram, m, mem)) return 1;
		t_store_ram = now_s();
		savebuf_load(&sram, m, mem);
		t_load_ram = now_s();
		savebuf_load(&sram, m, mem);
		t_load_ram2 = now_s();
		if (savebuf_open(&sdisk, geom.p2_len, "vmcostd") || savebuf_store(&sdisk, d + geom.p2_off, geom.p2_len)) return 1;
		t_store_disk = now_s();
		savebuf_load(&sdisk, d + geom.p2_off, geom.p2_len);
		t_load_disk = now_s();
		copy = malloc(mem);
		if (copy) { memcpy(copy, m, mem); memcpy(m, copy, mem); }
		t_memcpy_ram = now_s();
		free(copy);
		savebuf_close(&sram);
		savebuf_close(&sdisk);
		munmap(d, dlen);
		munmap(m, mem);
		t_munmap = now_s();
		close(fd);
		t_close = now_s();
		printf("VMCOST rep=%d open=%.3f create_vm=%.3f mmap_ram=%.3f set_disk=%.3f mmap_disk=%.3f disk_memcpy=%.3f "
		       "kernel_load=%.3f regs=%.3f get_snapshot=%.4f set_snapshot=%.4f fnv_ram=%.3f fnv_p2=%.3f "
		       "store_ram_file=%.3f load_ram_file_cold=%.3f load_ram_file_warm=%.3f store_p2_file=%.3f load_p2_file=%.3f "
		       "memcpy_ram_x2=%.3f munmap=%.3f close_vm=%.3f vm_fresh_equiv=%.3f h=%016llx\n",
		       r, t_open - t0, t_create - t_open, t_mmap - t_create, t_disk - t_mmap, t_dmmap - t_disk, t_dcopy - t_dmmap,
		       t_kern - t_dcopy, t_regs - t_kern, t_getsnap - t_regs, t_setsnap - t_getsnap, t_fnvram - t_setsnap,
		       t_fnvdisk - t_fnvram, t_store_ram - t_fnvdisk, t_load_ram - t_store_ram, t_load_ram2 - t_load_ram,
		       t_store_disk - t_load_ram2, t_load_disk - t_store_disk, t_memcpy_ram - t_load_disk,
		       t_munmap - t_memcpy_ram, t_close - t_munmap, t_regs - t0, (unsigned long long)h);
		fflush(stdout);
		free(snap);
	}
	return 0;
}

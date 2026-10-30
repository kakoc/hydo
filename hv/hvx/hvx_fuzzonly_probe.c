// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_fuzzonly_probe - HOST-SIDE probe of the deep-restore-doesn't-re-dispatch wall.
 *
 * repeat-62's bisection: real Linux booting JUST fuzz-read (one busy-poll userspace
 * process, no brokers) is NOT re-dispatched after a deep-snapshot restore -- the guest
 * idle-HLTs on the heartbeat and the staged byte is never read. This probe reproduces
 * that on the spread3 fuzzonly guest and answers, HOST-SIDE (via HVX_GET_REGS /
 * HVX_GET_SNAPSHOT between HVX_RUNs -- no in-kernel dmesg, so it can't wedge L1):
 *   - What RIP does the snapshot capture (fuzz-read spin @0x40xxxx vs kernel/idle)?
 *   - After restore + staging, does the guest EVER run fuzz-read, or only idle/kernel?
 *   - Is a LAPIC timer armed (tsc_deadline) and is in_service stuck?
 *
 * Boots real Linux (ext4 ro-root switch_root), mem 768 MiB, CPU_V2 + idle-heartbeat,
 * whole spread3-disk.img attached (NO_IDMAP), cmdline spread_mode=fuzzonly.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_fuzzonly_probe hvx_fuzzonly_probe.c
 * Run:  sudo ./hvx_fuzzonly_probe /tmp/bzImage /tmp/initramfs-spread3.cpio.gz /tmp/spread3-disk.img
 */
#include "hvx_boot.h"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define ull unsigned long long
#define READY_MARKER "FUZZREADY"
#define DONE_MARKER  "FUZZONLY-READ="

struct boot { int fd; uint8_t *mem; uint64_t mem_size; uint8_t *ubuf; size_t ulen, drained; };

static long mem_find(const uint8_t *h, size_t hl, const char *n)
{
	size_t nl = strlen(n), i;
	if (!nl || hl < nl) return -1;
	for (i = 0; i + nl <= hl; i++) if (!memcmp(h + i, n, nl)) return (long)i;
	return -1;
}
static struct hvx_console con;
static void drain(struct boot *b)
{
	size_t ml, add;
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con) < 0) return;
	ml = con.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con.len;
	if (ml <= b->drained) return;
	add = ml - b->drained;
	if (b->ulen + add > UBUF_CAP) add = UBUF_CAP - b->ulen;
	memcpy(b->ubuf + b->ulen, con.buf + b->drained, add);
	b->ulen += add; b->drained = ml;
}
static int run_to(struct boot *b, const char *marker, unsigned long long cap)
{
	struct hvx_exit ex; unsigned long long i;
	for (i = 0; i < cap; i++) {
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) { if (errno==EINTR) continue; perror("HVX_RUN"); return -1; }
		drain(b);
		if (mem_find(b->ubuf, b->ulen, marker) >= 0) return 0;
	}
	return -1;
}
static uint8_t *slurp(const char *p, size_t *l)
{
	FILE *f = fopen(p, "rb"); uint8_t *b; long s;
	if (!f) { perror(p); return NULL; }
	fseek(f,0,SEEK_END); s=ftell(f); fseek(f,0,SEEK_SET);
	b = malloc(s); if (!b || fread(b,1,s,f)!=(size_t)s) { perror("fread"); return NULL; }
	fclose(f); *l=s; return b;
}
/* classify a guest RIP */
static const char *rip_class(uint64_t rip)
{
	if (rip >= 0xffffffff80000000ULL) return "kernel";
	if (rip >= 0x400000 && rip < 0x500000) return "fuzz-read(user)";
	if (rip < 0x100000) return "low";
	return "other-user";
}

int main(int argc, char **argv)
{
	uint64_t mem_size = 768ULL << 20;
	size_t il, rl, dl;
	uint8_t *img, *ird, *disk, *ram_save;
	struct hvx_snapshot *snap;
	struct hvx_config cfg; struct hvx_sregs sr; struct hvx_regs rg;
	struct hvx_tsc tsc = { .tsc=0, .step=BOOT_TSC_STEP, .aux=0, .mode=HVX_TSC_COUNTER };
	struct hvx_disk dk; struct boot b; struct hvx_fuzz_input fi;
	const char *cmd = "root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys spread_mode=fuzzonly";
	int i;

	if (argc < 4) { fprintf(stderr, "usage: %s bzImage initramfs-spread3 spread3-disk.img\n", argv[0]); return 2; }
	img = slurp(argv[1], &il); ird = slurp(argv[2], &rl); disk = slurp(argv[3], &dl);
	if (!img || !ird || !disk) return 1;

	memset(&b,0,sizeof(b)); b.mem=MAP_FAILED; b.mem_size=mem_size;
	b.ubuf = malloc(UBUF_CAP); ram_save = malloc(mem_size); snap = calloc(1,sizeof(*snap));
	if (!b.ubuf || !ram_save || !snap) { perror("malloc"); return 1; }

	b.fd = open("/dev/hvx", O_RDWR); if (b.fd<0) { perror("open"); return 1; }
	memset(&cfg,0,sizeof(cfg)); cfg.mem_size=mem_size; cfg.flags = HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT;
	if (ioctl(b.fd, HVX_CREATE_VM, &cfg) < 0) { perror("CREATE_VM"); return 1; }
	b.mem = mmap(NULL, mem_size, PROT_READ|PROT_WRITE, MAP_SHARED, b.fd, 0);
	if (b.mem==MAP_FAILED) { perror("mmap"); return 1; }
	if (ioctl(b.fd, HVX_SET_TSC, &tsc) < 0) { perror("SET_TSC"); return 1; }
	dk.len=dl; dk.data=(uint64_t)(uintptr_t)disk; dk.flags=HVX_DISK_NO_IDMAP;
	if (ioctl(b.fd, HVX_SET_DISK, &dk) < 0) { perror("SET_DISK"); return 1; }
	build_page_tables_and_gdt(b.mem, mem_size);
	if (load_bzimage(b.mem, mem_size, img, il, ird, rl, cmd) < 0) return 1;
	fill_sregs(&sr); if (ioctl(b.fd, HVX_SET_SREGS, &sr)<0) { perror("SET_SREGS"); return 1; }
	memset(&rg,0,sizeof(rg)); rg.rip=KERNEL_LOAD_ADDR+KERNEL_ENTRY_OFF; rg.rsi=BOOT_PARAMS_ADDR; rg.rsp=STACK_TOP; rg.rflags=0x2;
	if (ioctl(b.fd, HVX_SET_REGS, &rg)<0) { perror("SET_REGS"); return 1; }

	fprintf(stderr, "== boot to FUZZREADY ==\n");
	if (run_to(&b, READY_MARKER, 4000000) != 0) { fprintf(stderr, "FAIL: no FUZZREADY\n"); return 1; }
	/* settle into the busy-spin (like the harness's 40-iter settle) */
	for (i=0;i<40;i++){ struct hvx_exit ex; if (ioctl(b.fd,HVX_RUN,&ex)<0) break; drain(&b);}
	fprintf(stderr, "== FUZZREADY reached (%zu console bytes) ==\n", b.ulen);

	/* capture: what context is current at the snapshot? */
	if (ioctl(b.fd, HVX_GET_SNAPSHOT, snap) < 0) { perror("GET_SNAPSHOT"); return 1; }
	memcpy(ram_save, b.mem, mem_size);
	{
		static uint8_t *disk_save; disk_save = malloc(dl);
		dk.data=(uint64_t)(uintptr_t)disk_save; dk.flags=0;
		if (ioctl(b.fd, HVX_GET_DISK, &dk) < 0) { perror("GET_DISK"); return 1; }
		fprintf(stderr, "== SNAPSHOT captured: rip=%#llx (%s) rsp=%#llx rflags=%#llx inject_valid=%u in_service=%#x tsc_deadline=%#llx icount=%llu ==\n",
			(ull)snap->regs.rip, rip_class(snap->regs.rip), (ull)snap->regs.rsp, (ull)snap->regs.rflags,
			snap->inject_valid, snap->lapic_in_service, (ull)snap->lapic_tsc_deadline, (ull)snap->icount);
		/* restore-in-place (RAM+disk reload, matching the harness) */
		if (ioctl(b.fd, HVX_SET_SNAPSHOT, snap) < 0) { perror("SET_SNAPSHOT"); return 1; }
		memcpy(b.mem, ram_save, mem_size);
		dk.len=dl; dk.data=(uint64_t)(uintptr_t)disk_save; dk.flags=HVX_DISK_NO_IDMAP;
		if (ioctl(b.fd, HVX_SET_DISK, &dk) < 0) { perror("SET_DISK2"); return 1; }
	}
	if (ioctl(b.fd, HVX_GET_REGS, &rg)==0)
		fprintf(stderr, "== post-restore rip=%#llx (%s) ==\n", (ull)rg.rip, rip_class(rg.rip));

	/* stage [u32 len=1][ 'Z' ] AFTER restore (fuzz-read's framed protocol) */
	memset(&fi,0,sizeof(fi)); fi.len=5; fi.buf[0]=1; fi.buf[1]=0; fi.buf[2]=0; fi.buf[3]=0; fi.buf[4]='Z';
	if (ioctl(b.fd, HVX_SET_FUZZ_INPUT, &fi) < 0) { perror("SET_FUZZ_INPUT"); return 1; }
	fprintf(stderr, "== staged [len=1]['Z']; resuming + probing RIP for 120 iters ==\n");

	/* bounded probe: run, classify RIP, watch for the byte */
	{
		int k, n_kernel=0, n_fuzz=0, n_other=0, done=0;
		for (k=0; k<120; k++) {
			struct hvx_exit ex;
			if (ioctl(b.fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); break; }
			drain(&b);
			if (ioctl(b.fd, HVX_GET_REGS, &rg)==0) {
				const char *c = rip_class(rg.rip);
				if (!strcmp(c,"kernel")) n_kernel++;
				else if (!strcmp(c,"fuzz-read(user)")) n_fuzz++;
				else n_other++;
			}
			if (k<8 || (k%25)==0) {
				struct hvx_snapshot *s2 = calloc(1,sizeof(*s2));
				if (s2 && ioctl(b.fd, HVX_GET_SNAPSHOT, s2)==0)
					fprintf(stderr, "  [it=%d] reason=%u rip=%#llx (%s) tsc_deadline=%#llx in_service=%#x inject_valid=%u icount=%llu\n",
						k, ex.reason, (ull)rg.rip, rip_class(rg.rip),
						(ull)s2->lapic_tsc_deadline, s2->lapic_in_service, s2->inject_valid, (ull)s2->icount);
				free(s2);
			}
			if (mem_find(b.ubuf, b.ulen, DONE_MARKER) >= 0) { done=1; break; }
		}
		fprintf(stderr, "== RESULT: done=%d  RIP tally over probe: kernel/idle=%d fuzz-read=%d other=%d ==\n",
			done, n_kernel, n_fuzz, n_other);
		{ size_t from = b.ulen > 300 ? b.ulen - 300 : 0;
		  fprintf(stderr, "== console tail (%zu bytes total): |%.*s|\n",
			b.ulen, (int)(b.ulen - from), b.ubuf + from); }
		{ long at = mem_find(b.ubuf, b.ulen, DONE_MARKER);
		  if (at>=0) fprintf(stderr, "  FUZZONLY-READ line: |%.*s|\n", 40, b.ubuf+at); }
		if (done) { fprintf(stderr, "RESUME OK: fuzz-read re-dispatched + read the byte.\n"); return 0; }
		fprintf(stderr, "RESUME HANG: fuzz-read never re-dispatched (see RIP tally: is it all kernel/idle?).\n");
	}
	return 3;
}

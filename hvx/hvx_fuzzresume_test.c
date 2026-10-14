// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_fuzzresume_test - deep-snapshot-resume + fuzz-port regression, incl. the
 * 0x510/0x511-vs-virtio-doorbell COLLISION GUARD.
 *
 * A ~30-byte guest busy-polls the fuzz STATUS port (0x511) exactly like the Phase-3
 * fuzz-read reader, then reads the DATA port (0x510), stores the byte, and HLTs:
 *
 *   poll: mov edx,0x511 ; in al,dx ; test al,al ; jz poll   (spin while not ready)
 *         mov edx,0x510 ; in al,dx                          (read the data byte)
 *         mov edi,0x5000; mov [rdi],al                      (store it to GPA 0x5000)
 *         mov edx,0x3f8 ; out dx,al                         (echo to console)
 *         hlt
 *
 *   Phase A: sanity, NO snapshot -- stage 'A', run, guest must read 'A'.
 *   Phase B: DEEP RESUME, no disk -- run to the spin, GET/SET_SNAPSHOT, stage 'B', run.
 *   Phase C: DEEP RESUME **with a disk attached** -- this is the regression guard for the
 *            port collision: 0x510/0x511 fall inside the M20 virtio-blk Phase-A doorbell
 *            range 0x500-0x52f (0x510 == HVX_VPIO_USED_LO), so attaching a disk enables
 *            hvx_virtio_pio for those ports. Without hvx_pio routing an IN of the fuzz
 *            ports past the doorbell, `in 0x511` returns the doorbell default (0) and the
 *            reader spins forever. Phase C must read 'C' -> proves the fix holds.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_fuzzresume_test hvx_fuzzresume_test.c
 * Run   (inside L1):  sudo ./hvx_fuzzresume_test
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

#include "hvx_abi.h"

#define EXIT_HLT        12
#define GUEST_MEM_BYTES (64UL * 4096)
#define STORE_GPA       0x5000

static const unsigned char guest_code[] = {
	0xba, 0x11, 0x05, 0x00, 0x00,	/* @0  mov edx, 0x511 (STATUS)      */
	0xec,				/* @5  in  al, dx                   */
	0x84, 0xc0,			/* @6  test al, al                  */
	0x74, 0xf6,			/* @8  jz  -10 -> @0 (spin)         */
	0xba, 0x10, 0x05, 0x00, 0x00,	/* @10 mov edx, 0x510 (DATA)        */
	0xec,				/* @15 in  al, dx                   */
	0xbf, 0x00, 0x50, 0x00, 0x00,	/* @16 mov edi, 0x5000              */
	0x88, 0x07,			/* @21 mov [rdi], al                */
	0xba, 0xf8, 0x03, 0x00, 0x00,	/* @23 mov edx, 0x3f8 (UART THR)    */
	0xee,				/* @28 out dx, al                   */
	0xf4,				/* @29 hlt                          */
};

static void load_guest(void *mem)
{
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));
	((volatile unsigned char *)mem)[STORE_GPA] = 0;
}

static void set_start_regs(int fd)
{
	struct hvx_regs regs;
	memset(&regs, 0, sizeof(regs));
	regs.rip = HVX_GUEST_CODE_BASE;
	regs.rsp = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); exit(1); }
}

static void stage_fuzz(int fd, char byte)
{
	struct hvx_fuzz_input fi;
	memset(&fi, 0, sizeof(fi));
	fi.len = 1;
	fi.buf[0] = (uint8_t)byte;
	if (ioctl(fd, HVX_SET_FUZZ_INPUT, &fi) < 0) { perror("HVX_SET_FUZZ_INPUT"); exit(1); }
}

static int run_to_hlt(int fd, int cap, int *hlt)
{
	struct hvx_exit ex;
	int i;
	*hlt = 0;
	for (i = 0; i < cap; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT) { *hlt = 1; return EXIT_HLT; }
		if (ex.reason == HVX_EXIT_THROTTLE) continue;
		fprintf(stderr, "  unexpected exit reason=%u\n", ex.reason);
		return (int)ex.reason;
	}
	return HVX_EXIT_THROTTLE;	/* cap reached without HLT */
}

static int newvm(int *fd, void **mem)
{
	struct hvx_config cfg;
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = GUEST_MEM_BYTES;
	*fd = open("/dev/hvx", O_RDWR);
	if (*fd < 0) { perror("open /dev/hvx"); return -1; }
	if (ioctl(*fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }
	*mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, *fd, 0);
	if (*mem == MAP_FAILED) { perror("mmap"); return -1; }
	return 0;
}

/* run to the poll spin (THROTTLE), snapshot mid-spin, restore in place, stage `byte`,
 * resume; return the byte the guest stored to STORE_GPA (0 on failure). */
static unsigned char deep_resume(int fd, void *mem, char byte)
{
	struct hvx_exit ex;
	struct hvx_snapshot *s;
	int i, hlt, saw_throttle = 0;

	for (i = 0; i < 4; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return 0; }
		if (ex.reason == HVX_EXIT_THROTTLE) { saw_throttle = 1; break; }
		if (ex.reason == EXIT_HLT) { fprintf(stderr, "  guest HLT before staging?!\n"); return 0; }
	}
	if (!saw_throttle) { fprintf(stderr, "  guest never entered the poll spin\n"); return 0; }

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }
	if (ioctl(fd, HVX_SET_SNAPSHOT, s) < 0) { perror("HVX_SET_SNAPSHOT"); free(s); return 0; }
	free(s);

	stage_fuzz(fd, byte);
	run_to_hlt(fd, 200, &hlt);
	if (!hlt) return 0;
	return ((volatile unsigned char *)mem)[STORE_GPA];
}

int main(void)
{
	int fd, hlt, rc = 1;
	void *mem;
	unsigned char got;
	static uint8_t diskbuf[65536];	/* small backing image to ENABLE the virtio device */
	struct hvx_disk dk;

	/* ---- Phase A: sanity, no snapshot ---- */
	if (newvm(&fd, &mem) < 0) return 1;
	load_guest(mem);
	set_start_regs(fd);
	stage_fuzz(fd, 'A');
	run_to_hlt(fd, 64, &hlt);
	got = ((volatile unsigned char *)mem)[STORE_GPA];
	printf("Phase A (no snapshot):        hlt=%d stored='%c'\n", hlt, got ? got : '?');
	if (!hlt || got != 'A') { fprintf(stderr, "Phase A FAIL: fuzz port broken\n"); close(fd); return 1; }
	close(fd);

	/* ---- Phase B: deep resume, no disk ---- */
	if (newvm(&fd, &mem) < 0) return 1;
	load_guest(mem);
	set_start_regs(fd);
	got = deep_resume(fd, mem, 'B');
	printf("Phase B (deep resume):        hlt=1 stored='%c'\n", got ? got : '?');
	if (got != 'B') { fprintf(stderr, "Phase B FAIL: deep restore did not resume the poll\n"); close(fd); return 1; }
	close(fd);

	/* ---- Phase C: deep resume WITH a disk attached (the collision guard) ---- */
	if (newvm(&fd, &mem) < 0) return 1;
	load_guest(mem);
	set_start_regs(fd);
	memset(&dk, 0, sizeof(dk));
	dk.len = sizeof(diskbuf);
	dk.data = (uint64_t)(uintptr_t)diskbuf;
	dk.flags = HVX_DISK_NO_IDMAP;	/* just enable the virtio device; don't touch guest tables */
	if (ioctl(fd, HVX_SET_DISK, &dk) < 0) { perror("HVX_SET_DISK"); close(fd); return 1; }
	got = deep_resume(fd, mem, 'C');
	printf("Phase C (deep resume + disk): hlt=1 stored='%c'\n", got ? got : '?');
	if (got != 'C') {
		fprintf(stderr, "Phase C FAIL: with a disk attached, the fuzz ports 0x510/0x511 were "
			"shadowed by the virtio Phase-A doorbell -- the collision has regressed.\n");
		close(fd); return 1;
	}
	close(fd);

	printf("hvx_fuzzresume_test: PASS: deep-snapshot resume re-dispatches a busy-poll reader and "
	       "reads staged fuzz input -- with AND without a disk attached (0x510/0x511 vs virtio "
	       "Phase-A doorbell collision guarded).\n");
	rc = 0;
	return rc;
}

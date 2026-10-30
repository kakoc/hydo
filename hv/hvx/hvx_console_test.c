// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_console_test - prove PIO trap + serial console (milestone 10).
 *
 * The guest writes "HELLO\n" to the 16550 UART transmit register (port 0x3f8),
 * then reads the line-status register (0x3fd) and stores it at GPA 0x5000:
 *
 *   mov edx, 0x3f8
 *   mov al,'H'; out dx,al   ...   mov al,'\n'; out dx,al
 *   mov edx, 0x3fd
 *   in  al, dx              ; line-status register
 *   mov [0x5000], al
 *   hlt
 *
 * Without I/O-exiting these OUTs would hit L1's real COM1; with milestone 10 the
 * module traps them, so HVX_GET_CONSOLE returns "HELLO\n" and the LSR read
 * returns 0x60 (THRE|TEMT = transmit ready).
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_console_test hvx_console_test.c
 * Run   (inside L1):  sudo ./hvx_console_test
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
#define LSR_GPA         0x5000UL

static const unsigned char guest_code[] = {
	0xba, 0xf8, 0x03, 0x00, 0x00,	/* mov edx, 0x3f8 */
	0xb0, 'H', 0xee,		/* mov al,'H'; out dx,al */
	0xb0, 'E', 0xee,
	0xb0, 'L', 0xee,
	0xb0, 'L', 0xee,
	0xb0, 'O', 0xee,
	0xb0, '\n', 0xee,
	0xba, 0xfd, 0x03, 0x00, 0x00,	/* mov edx, 0x3fd (LSR) */
	0xec,				/* in al, dx */
	0x48, 0xbb, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  /* movabs $0x5000,%rbx */
	0x88, 0x03,			/* mov %al,(%rbx) */
	0xf4,				/* hlt */
};

int main(void)
{
	int fd, i, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_regs regs;
	struct hvx_exit ex;
	struct hvx_console con;
	unsigned char lsr;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); goto out; }

	for (i = 0; i < 16; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); goto out; }
		if (ex.reason == EXIT_HLT)
			break;
		fprintf(stderr, "hvx_console_test: unexpected exit reason=%u\n", ex.reason);
		goto out;
	}

	if (ioctl(fd, HVX_GET_CONSOLE, &con) < 0) { perror("HVX_GET_CONSOLE"); goto out; }
	lsr = *(volatile unsigned char *)((unsigned char *)mem + LSR_GPA);
	printf("hvx_console_test: console len=%u buf=\"%.*s\" (LSR read=0x%02x)\n",
	       con.len, (int)con.len, con.buf, lsr);

	if (con.len != 6 || memcmp(con.buf, "HELLO\n", 6) != 0) {
		fprintf(stderr, "hvx_console_test: FAIL: console mismatch (len=%u)\n", con.len);
		goto out;
	}
	if (lsr != 0x60) {
		fprintf(stderr, "hvx_console_test: FAIL: LSR read 0x%02x, expected 0x60\n", lsr);
		goto out;
	}

	printf("hvx_console_test: MILESTONE 10 PASS: guest PIO trapped, UART console "
	       "captured \"HELLO\\n\", LSR reads transmit-ready\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

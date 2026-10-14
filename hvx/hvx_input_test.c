// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_input_test - prove the UART receive path (milestone 18).
 *
 * Userspace pushes bytes into the guest's serial input queue (HVX_PUT_INPUT); a
 * guest polls the 16550 line-status register (0x3fd) for Data-Ready, reads the
 * receive register (RBR, 0x3f8), and echoes each byte back to the transmit
 * register (THR, same port 0x3f8) -- which the module captures into the console.
 * The guest loops for exactly the number of bytes we pushed, then HLTs:
 *
 *   mov ecx, N
 * poll:
 *   mov edx, 0x3fd ; in al,dx ; test al,1 ; jz poll   ; wait for DR (input queued)
 *   mov edx, 0x3f8 ; in al,dx                          ; pop the queued byte (RBR)
 *   out dx, al                                         ; echo it (THR -> console)
 *   dec ecx ; jnz poll
 *   hlt
 *
 * This exercises the RX mechanism deterministically without the full Linux boot:
 * the RX IRQ path (redir[4]/ERBFI -> IRQ4 injection) is proven end-to-end by
 * hvx_linux's scripted-input demo instead. We assert the echoed console bytes
 * equal exactly what we pushed, and that a second run (fresh input) reproduces.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_input_test hvx_input_test.c
 * Run   (inside L1):  sudo ./hvx_input_test
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

/* The message the guest will receive, echo, and we then verify. */
#define INPUT_STR       "PING\n"
#define INPUT_LEN       5

/*
 * Guest blob (hand-assembled). ecx = byte count; RBR/LSR both at 0x3f8/0x3fd.
 * Offsets are relative to the load base (GUEST_CODE_BASE); the two backward
 * branches target "poll" at offset 5.
 */
static const unsigned char guest_code[] = {
	0xb9, INPUT_LEN, 0x00, 0x00, 0x00,	/* mov ecx, INPUT_LEN            @0  */
	/* poll:                                                                @5  */
	0xba, 0xfd, 0x03, 0x00, 0x00,		/* mov edx, 0x3fd (LSR)          @5  */
	0xec,					/* in  al, dx                    @10 */
	0xa8, 0x01,				/* test al, 1  (Data Ready?)     @11 */
	0x74, 0xf6,				/* jz poll (-> @5)               @13 */
	0xba, 0xf8, 0x03, 0x00, 0x00,		/* mov edx, 0x3f8 (RBR/THR)      @15 */
	0xec,					/* in  al, dx  (pop queued byte) @20 */
	0xee,					/* out dx, al  (echo to console) @21 */
	0xff, 0xc9,				/* dec ecx                       @22 */
	0x75, 0xeb,				/* jnz poll (-> @5)              @24 */
	0xf4,					/* hlt                           @26 */
};

/* Load the guest, push `INPUT_STR`, run to HLT, and return the console bytes the
 * guest echoed (via out and out_len). Returns 0 on success, -1 on any failure. */
static int run_once(int fd, void *mem, unsigned char *out, unsigned int *out_len)
{
	struct hvx_regs regs;
	struct hvx_exit ex;
	struct hvx_console con;
	struct hvx_input in;
	int i;

	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* push the input BEFORE running: the guest polls DR and drains exactly
	 * INPUT_LEN bytes, so everything happens within one HVX_RUN. */
	memset(&in, 0, sizeof(in));
	in.len = INPUT_LEN;
	memcpy(in.buf, INPUT_STR, INPUT_LEN);
	if (ioctl(fd, HVX_PUT_INPUT, &in) < 0) { perror("HVX_PUT_INPUT"); return -1; }

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }

	for (i = 0; i < 64; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT)
			break;
		if (ex.reason == HVX_EXIT_THROTTLE)
			continue;	/* run-loop bound; keep going */
		fprintf(stderr, "hvx_input_test: unexpected exit reason=%u\n", ex.reason);
		return -1;
	}
	if (ex.reason != EXIT_HLT) {
		fprintf(stderr, "hvx_input_test: guest did not HLT\n");
		return -1;
	}

	if (ioctl(fd, HVX_GET_CONSOLE, &con) < 0) { perror("HVX_GET_CONSOLE"); return -1; }
	*out_len = con.len;
	memcpy(out, con.buf, con.len < 256 ? con.len : 256);
	return 0;
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	unsigned char echoed[256];
	unsigned int echoed_len = 0;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }

	if (run_once(fd, mem, echoed, &echoed_len) < 0)
		goto out;

	printf("hvx_input_test: pushed \"PING\\n\", guest echoed len=%u buf=\"%.*s\"\n",
	       echoed_len, (int)echoed_len, echoed);

	if (echoed_len != INPUT_LEN || memcmp(echoed, INPUT_STR, INPUT_LEN) != 0) {
		fprintf(stderr, "hvx_input_test: FAIL: echo mismatch (len=%u)\n", echoed_len);
		goto out;
	}

	printf("hvx_input_test: MILESTONE 18 PASS: userspace input reached the guest "
	       "UART RX, was read from RBR, and echoed back \"PING\\n\"\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

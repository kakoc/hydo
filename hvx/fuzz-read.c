// SPDX-License-Identifier: GPL-2.0
/*
 * fuzz-read - guest-side reader for hvx's SYNCHRONOUS fuzz-input port (Phase 3 search loop).
 *
 * WHY a port, not the serial console: a blocking serial read idle-parks the guest, and under hvx's
 * idle heartbeat a permanently-idle guest makes HVX_RUN never return to the host -> the host restore
 * loop cannot drain the READY marker or feed the next program. A port read is SYNCHRONOUS: each
 * `in al, dx` is a VM-exit the module services and returns from at once (no interrupt, no idle), so
 * HVX_RUN keeps returning, the heartbeat stays on for the workload, and the read is a pure function
 * of the host-staged buffer -> byte-deterministic across snapshot restores.
 *
 * Protocol (matches hvx.c / hvx_spread_search.c):
 *   STATUS port 0x511: IN -> 1 while bytes remain to read, else 0.
 *   DATA   port 0x510: IN -> next staged byte, advancing the module's offset (0 when drained).
 *   The host stages [u32 length little-endian][length program bytes] and resets the offset per
 *   restore (HVX_SET_FUZZ_INPUT). We SPIN on the status port (busy, synchronous -> HVX_RUN keeps
 *   returning) until the host stages input, then read the length then that many bytes to stdout.
 *
 * Built static+musl and staged onto the search ro-root; /init runs it once per restore before the
 * driver. Needs CAP_SYS_RAWIO for ioperm (/init is root).
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/io.h>

#define FUZZ_DATA   0x510
#define FUZZ_STATUS 0x511
#define FUZZ_MAX    (1u << 20)	/* sanity cap on the program size */

int main(void)
{
	/* Grant I/O-port access. ioperm() only covers ports 0..0x3ff, and our ports are 0x510/0x511
	 * (> 1023), so we need iopl(3) which grants access to ALL 65536 ports. Needs CAP_SYS_RAWIO
	 * (/init is root). Without this an IN on 0x510 faults (#GP) and the reader is killed. */
	if (iopl(3) != 0) {
		perror("fuzz-read: iopl");
		return 1;
	}
	/* Emit the snapshot-boundary marker HERE -- AFTER the binary is loaded and I/O permission is
	 * granted, right before the spin -- so the host snapshots the guest at a clean, resumable point
	 * (fuzz-read busy-spinning on the port, no pending fork/exec/disk I/O). Raw write to stderr so it
	 * reaches the console (stdout is redirected to the program file). */
	(void)!write(2, "FUZZREADY\n", 10);
	/* Busy-spin until the host stages a program (synchronous port I/O -> the vCPU stays runnable,
	 * so HVX_RUN keeps returning and the host can snapshot the marker + stage input). The status
	 * read does NOT advance the data offset, so the offset is still 0 when we read below. */
	while (inb(FUZZ_STATUS) == 0)
		;
	/* u32 little-endian length prefix. */
	uint32_t len = 0;
	for (int i = 0; i < 4; i++)
		len |= (uint32_t)inb(FUZZ_DATA) << (8 * i);
	if (len > FUZZ_MAX)
		len = FUZZ_MAX;
	/* Read the program into a buffer, then write it once (deterministic bytes to /tmp/prog.txt). */
	static unsigned char buf[FUZZ_MAX];
	for (uint32_t i = 0; i < len; i++)
		buf[i] = inb(FUZZ_DATA);
	if (len)
		(void)!fwrite(buf, 1, len, stdout);
	fflush(stdout);
	return 0;
}

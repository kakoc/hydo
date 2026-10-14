// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_seedrng - deterministically seed the guest kernel CRNG (milestone 27).
 *
 * WHY: a real deterministic hypervisor exposes NO hardware entropy -- RDRAND/RDSEED are
 * not advertised (they would be nondeterministic) and the TSC advances by a fixed step
 * per exit, so it carries NO jitter. Linux's getrandom()/CRNG therefore never gathers
 * enough entropy to initialise: the kernel's jitter-entropy loop (try_to_generate_entropy)
 * reads the TSC in a tight loop expecting timing variance, finds none, and SPINS -- so any
 * program that calls blocking getrandom() (e.g. redis-server for its hash seed) hangs at
 * startup, burning millions of RDTSC exits.
 *
 * FIX: as root in the guest, credit a FIXED block of entropy via the RNDADDENTROPY ioctl
 * on /dev/urandom. That marks the CRNG initialised, so getrandom() returns immediately.
 * The seed bytes are constant, and the whole boot is already deterministic (M16), so the
 * CRNG stream -- and every getrandom() consumer -- stays byte-for-byte reproducible. This
 * is exactly the "provide deterministic boot entropy" a deterministic hypervisor should do,
 * done from userspace so the shared bzImage loader (and every other boot's fingerprint) is
 * untouched.
 *
 * Built static+musl (like hydokv) and staged only into the redis initramfs; /init runs it
 * once before starting redis-server.
 */
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <linux/random.h>

int main(void)
{
	struct {
		int entropy_count;	/* in BITS */
		int buf_size;		/* in BYTES */
		unsigned char buf[256];
	} pool;
	int fd, i;

	pool.entropy_count = 256 * 8;	/* credit full entropy for the 256 seed bytes */
	pool.buf_size = sizeof(pool.buf);
	for (i = 0; i < (int)sizeof(pool.buf); i++)
		pool.buf[i] = (unsigned char)(i * 7 + 13);	/* fixed, deterministic seed */

	fd = open("/dev/urandom", O_RDWR);
	if (fd < 0) { perror("hvx_seedrng: open /dev/urandom"); return 1; }
	if (ioctl(fd, RNDADDENTROPY, &pool) < 0) {
		perror("hvx_seedrng: RNDADDENTROPY");
		close(fd);
		return 1;
	}
	close(fd);
	printf("hvx_seedrng: credited %d bytes of deterministic entropy (CRNG initialised)\n",
	       pool.buf_size);
	return 0;
}

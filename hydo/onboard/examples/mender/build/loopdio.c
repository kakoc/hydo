/* loopdio <loop device> on|off: switch a loop device to direct I/O (LOOP_SET_DIRECT_IO).
 * With direct I/O, a write to the loop device goes to the disk under it when the loop device's
 * page cache is written back, as a write to an eMMC partition goes to the eMMC; without it, the
 * write waits in the backing file's page cache too, as on a second device that only a flush of
 * its own makes durable. RESULTS.md, "The storage model". */
#include <fcntl.h>
#include <linux/loop.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(int argc, char **argv)
{
	if (argc != 3 || (strcmp(argv[2], "on") && strcmp(argv[2], "off"))) {
		fprintf(stderr, "usage: loopdio <loop device> on|off\n");
		return 2;
	}
	int fd = open(argv[1], O_RDWR);
	if (fd < 0) {
		perror(argv[1]);
		return 1;
	}
	if (ioctl(fd, LOOP_SET_DIRECT_IO, strcmp(argv[2], "on") == 0 ? 1UL : 0UL) < 0) {
		perror("LOOP_SET_DIRECT_IO");
		return 1;
	}
	close(fd);
	return 0;
}

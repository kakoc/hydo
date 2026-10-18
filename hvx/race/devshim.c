// SPDX-License-Identifier: GPL-2.0
/*
 * devshim - LD_PRELOAD shim that sends open("/dev/hvx") to $HVX_DEV, so the unmodified
 * test suite can run against a side-by-side copy of the module (e.g. /dev/hvxpre).
 *   cc -O2 -shared -fPIC -o devshim.so devshim.c -ldl
 *   sudo env LD_PRELOAD=$PWD/devshim.so HVX_DEV=/dev/hvxpre ./hvx_test
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

int open(const char *path, int flags, ...)
{
	static int (*real)(const char *, int, ...);
	const char *dev = getenv("HVX_DEV");
	mode_t mode = 0;

	if (!real)
		real = (int (*)(const char *, int, ...))dlsym(RTLD_NEXT, "open");
	if (flags & O_CREAT) {
		va_list ap;
		va_start(ap, flags);
		mode = va_arg(ap, mode_t);
		va_end(ap);
	}
	if (dev && path && !strcmp(path, "/dev/hvx"))
		path = dev;
	return real(path, flags, mode);
}

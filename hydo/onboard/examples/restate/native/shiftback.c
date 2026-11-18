/* LD_PRELOAD shim: the wall clock (CLOCK_REALTIME, gettimeofday, time) reads SHIFT_BACK_SECS
 * seconds earlier than the kernel's. The monotonic clocks are unchanged. It calls the kernel
 * directly, so it needs no dlsym and no allocation (libfaketime hung restate-server at start). */
#define _GNU_SOURCE
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static long shift = -1;

static long back(void) {
    if (shift < 0) {
        const char *s = getenv("SHIFT_BACK_SECS");
        shift = s ? atol(s) : 0;
    }
    return shift;
}

int clock_gettime(clockid_t id, struct timespec *ts) {
    long r = syscall(SYS_clock_gettime, id, ts);
    if (r == 0 && ts && (id == CLOCK_REALTIME || id == CLOCK_REALTIME_COARSE))
        ts->tv_sec -= back();
    return (int)r;
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz) {
    (void)tz;
    struct timespec ts;
    long r = syscall(SYS_clock_gettime, CLOCK_REALTIME, &ts);
    if (r == 0 && tv) {
        tv->tv_sec = ts.tv_sec - back();
        tv->tv_usec = ts.tv_nsec / 1000;
    }
    return (int)r;
}

time_t time(time_t *t) {
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_REALTIME, &ts);
    time_t v = ts.tv_sec - back();
    if (t) *t = v;
    return v;
}

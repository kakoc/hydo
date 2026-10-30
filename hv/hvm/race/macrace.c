/*
 * macrace - hv/hvx/race/hydorace.c's seat-selling race (once-per-run mode) for the Mac VMM.
 *
 * Two processes (fork, a MAP_SHARED page: there is no libc here, so no threads) sell seats
 * from one shared counter. Every sale is check-then-act, but only the sale of seat number
 * ONCE has work between the check and the act -- the race window. If the guest kernel
 * switches to the other process inside that window and the other process sells a seat,
 * one sale is lost (lost=1). On one vCPU the only way to hit it is a thread switch landing
 * in the window, which is what tick jitter is for.
 *
 * Between sales each process does OUT iterations of plain work and one clock read. On the
 * hvm guest kernel (CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT, no vDSO counter) every clock read
 * is a VM exit, and the Mac VMM can deliver a timer interrupt only at an exit. Two builds:
 *   -DWIN_CLK: the window reads the clock K times (K exits inside the window);
 *   otherwise: the window is W iterations of plain work (no exit inside the window).
 * -DNWIN=2: two windows, at seats ONCE (15,000) and ONCE2 (5,000). Each window notes whether the
 * other process sold a seat inside it (hit1, hit2); a bug that needs BOTH is a two-ordering bug.
 * Prints: HYDO-RACE: <mem|clk> seats=.. sold=.. lost=.. a=.. b=.. hit1=.. hit2=.. nvcsw=..
 * (nvcsw: involuntary context switches of both processes, from /proc/self/status.)
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -ffreestanding -fno-builtin -fno-pie -no-pie
 *     -mgeneral-regs-only -O2 [-DWIN_CLK] -o macrace macrace.c
 */
#define SEATS 20000L
#ifndef NWIN
#define NWIN 1
#endif
#if NWIN == 2
#define ONCE  15000L
#define ONCE2 5000L
#else
#define ONCE  10000L
#define ONCE2 (-1L)
#endif
#define OUT   200L
#define W     50000L      /* plain-work window: about as many instructions as K clock reads */
#define K     50L         /* clock reads in the clock window */

static long sys(long n, long a, long b, long c, long d, long e, long f)
{
	register long x8 asm("x8") = n, x0 asm("x0") = a, x1 asm("x1") = b, x2 asm("x2") = c,
	              x3 asm("x3") = d, x4 asm("x4") = e, x5 asm("x5") = f;
	asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5) : "memory");
	return x0;
}
struct shared { volatile long seats, sold[2], hit[2], nvcsw[2]; };
static long ts[2] = { 1, 1 };   /* in .data: Linux 6.6 cannot load a bss-only segment with no file bytes */
static void clock_read(void) { sys(113, 1, (long)ts, 0, 0, 0, 0); }   /* clock_gettime(CLOCK_MONOTONIC) */
static void work(long n) { for (volatile long i = 0; i < n; i++) ; }

static void sell(struct shared *sh, int me)
{
	for (;;) {
		work(OUT);
		clock_read();
		long s = sh->seats;            /* check ... */
		if (s <= 0) break;
		if (s == ONCE || s == ONCE2) { /* ... the race window ... */
#ifdef WIN_CLK
			for (long i = 0; i < K; i++) clock_read();
#else
			work(W);
#endif
			if (sh->seats != s) sh->hit[s == ONCE2] = 1;   /* the other process sold inside the window */
		}
		sh->seats = s - 1;             /* ... act */
		sh->sold[me]++;
	}
}
static long nvcsw(void)            /* "nonvoluntary_ctxt_switches:" from /proc/self/status */
{
	static char buf[4096] = { 1 };
	static const char key[] = "nonvoluntary_ctxt_switches:";
	long fd = sys(56, -100, (long)"/proc/self/status", 0, 0, 0, 0), n, v = -1;
	if (fd < 0) return -1;
	n = sys(63, fd, (long)buf, sizeof buf - 1, 0, 0, 0);
	sys(57, fd, 0, 0, 0, 0, 0);
	for (long i = 0; i + (long)sizeof key - 1 < n; i++) {
		long j = 0;
		while (key[j] && buf[i + j] == key[j]) j++;
		if (key[j]) continue;
		i += j; while (i < n && (buf[i] == ' ' || buf[i] == '\t')) i++;
		for (v = 0; i < n && buf[i] >= '0' && buf[i] <= '9'; i++) v = v * 10 + (buf[i] - '0');
		break;
	}
	return v;
}
static int put(char *b, long v) { char t[24]; int n = 0, o = 0; if (!v) t[n++] = '0'; while (v) { t[n++] = '0' + v % 10; v /= 10; } while (n) b[o++] = t[--n]; return o; }
static int cat(char *b, const char *s) { int o = 0; while (s[o]) { b[o] = s[o]; o++; } return o; }

void _start(void)
{
	struct shared *sh = (struct shared *)sys(222, 0, 4096, 3, 0x21, -1, 0);   /* mmap shared anonymous */
	sh->seats = SEATS;
	long pid = sys(220, 17, 0, 0, 0, 0, 0);                                   /* fork: clone(SIGCHLD) */
	sell(sh, pid == 0);
	sh->nvcsw[pid == 0] = nvcsw();
	if (pid == 0) sys(93, 0, 0, 0, 0, 0, 0);
	sys(260, pid, 0, 0, 0, 0, 0);                                             /* wait4 */
	char b[160]; int o = 0;
#ifdef WIN_CLK
	o += cat(b + o, "HYDO-RACE: clk seats=");
#else
	o += cat(b + o, "HYDO-RACE: mem seats=");
#endif
	o += put(b + o, SEATS); o += cat(b + o, " sold="); o += put(b + o, sh->sold[0] + sh->sold[1]);
	o += cat(b + o, " lost="); o += put(b + o, sh->sold[0] + sh->sold[1] - SEATS);
	o += cat(b + o, " a="); o += put(b + o, sh->sold[0]); o += cat(b + o, " b="); o += put(b + o, sh->sold[1]);
	o += cat(b + o, " hit1="); o += put(b + o, sh->hit[0]); o += cat(b + o, " hit2="); o += put(b + o, sh->hit[1]);
	o += cat(b + o, " nvcsw="); o += put(b + o, sh->nvcsw[0] + sh->nvcsw[1]);
	b[o++] = '\n';
	sys(64, 1, (long)b, o, 0, 0, 0);
	sys(94, 0, 0, 0, 0, 0, 0);
}
/*
 * Boot it (hv/hvm/race, after cross-compiling on the build machine): an overlay cpio with /bin/macrace and an /init
 * that runs it and prints "hydo-race-done", appended to ../guest/initramfs.cpio.gz (concatenated archives
 * overlay); seeds.sh boots one seed per run. Results: ../explore/README.md "Tick jitter".
 */

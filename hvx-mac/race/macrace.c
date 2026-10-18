/*
 * macrace - hvx/race/hydorace.c's seat-selling race (once-per-run mode) for the Mac VMM.
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
 * Prints: HYDO-RACE: <mem|clk> seats=.. sold=.. lost=..
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -ffreestanding -fno-builtin -fno-pie -no-pie
 *     -mgeneral-regs-only -O2 [-DWIN_CLK] -o macrace macrace.c
 */
#define SEATS 20000L
#define ONCE  10000L
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
struct shared { volatile long seats, sold[2]; };
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
		if (s == ONCE) {               /* ... the race window ... */
#ifdef WIN_CLK
			for (long i = 0; i < K; i++) clock_read();
#else
			work(W);
#endif
		}
		sh->seats = s - 1;             /* ... act */
		sh->sold[me]++;
	}
}
static int put(char *b, long v) { char t[24]; int n = 0, o = 0; if (!v) t[n++] = '0'; while (v) { t[n++] = '0' + v % 10; v /= 10; } while (n) b[o++] = t[--n]; return o; }
static int cat(char *b, const char *s) { int o = 0; while (s[o]) { b[o] = s[o]; o++; } return o; }

void _start(void)
{
	struct shared *sh = (struct shared *)sys(222, 0, 4096, 3, 0x21, -1, 0);   /* mmap shared anonymous */
	sh->seats = SEATS;
	long pid = sys(220, 17, 0, 0, 0, 0, 0);                                   /* fork: clone(SIGCHLD) */
	sell(sh, pid == 0);
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
	b[o++] = '\n';
	sys(64, 1, (long)b, o, 0, 0, 0);
	sys(94, 0, 0, 0, 0, 0, 0);
}
/*
 * Boot it (hvx-mac/race, after cross-compiling on the build machine): an overlay cpio with /bin/macrace and an /init
 * that runs it and prints "hydo-race-done", appended to ../guest/initramfs.cpio.gz (concatenated archives
 * overlay); seeds.sh boots one seed per run. Results: ../explore/README.md "Tick jitter".
 */

/* spinwait: arm a 100 ms interval timer, then busy-wait in user space for the SIGALRM
 * handler to set a flag -- a loop with no system calls and no clock reads. Under a
 * deterministic VMM with no instruction counter, nothing interrupts the loop, so the
 * timer interrupt that would end it is never delivered (the PROBE.md blocker).
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -O2 -o spinwait spinwait.c */
static long sys(long n, long a, long b, long c, long d)
{
	register long x8 asm("x8") = n, x0 asm("x0") = a, x1 asm("x1") = b, x2 asm("x2") = c, x3 asm("x3") = d;
	asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3) : "memory");
	return x0;
}
static volatile int fired = 2;   /* in .data: Linux 6.6 cannot load a bss-only segment with no file bytes */
static void handler(int s) { (void)s; fired = 0; }
extern void restorer(void);                                 /* rt_sigreturn, with sp untouched */
asm(".text\n.global restorer\nrestorer:\n mov x8, #139\n svc #0\n");
void _start(void)
{
	struct { void (*h)(int); unsigned long flags; void (*r)(void); unsigned long mask; } sa = { handler, 0x04000000, restorer, 0 };
	sys(134, 14, (long)&sa, 0, 8);                         /* rt_sigaction(SIGALRM, SA_RESTORER) */
	long it[4] = { 0, 0, 0, 100000 };                      /* it_value = 100 ms */
	sys(103, 0, (long)it, 0, 0);                           /* setitimer(ITIMER_REAL) */
	sys(64, 1, (long)"hydo-hvm: spinwait start\n", 25, 0);
	while (fired) ;
	sys(64, 1, (long)"hydo-hvm: spinwait done\n", 24, 0);
	sys(93, 0, 0, 0, 0);
}

/* spinvar: exit-free spin-wait variants for testing hvm-spin. Each arms a 100 ms interval timer and
 * busy-waits in user space for the SIGALRM handler to clear `fired`, in a different loop shape.
 * Built with -DMODE=<n>:
 *   1 load      while (fired) ;                       pure read loop (a cycle)
 *   2 ldar      while (__atomic_load_n(&fired, ACQUIRE))  load-acquire (a cycle)
 *   3 yield     while (fired) asm("yield");           Go-runtime style procyield (a cycle)
 *   4 samestore while (fired) sink = 1;               stores the same value every iteration
 *   5 count     while (fired) n++;                    an iteration counter in a register (NOT a cycle)
 *   6 ldxr      while (ldaxr(&fired)) ;               exclusive load (stepping would break it)
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -O2 -DMODE=1 -o spin_load spinvar.c */
static long sys(long n, long a, long b, long c, long d)
{
	register long x8 asm("x8") = n, x0 asm("x0") = a, x1 asm("x1") = b, x2 asm("x2") = c, x3 asm("x3") = d;
	asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3) : "memory");
	return x0;
}
static volatile int fired = 2;
static volatile int sink = 2;
static volatile unsigned long count_out = 7;
static void handler(int s) { (void)s; fired = 0; }
extern void restorer(void);
asm(".text\n.global restorer\nrestorer:\n mov x8, #139\n svc #0\n");
static void say(const char *s) { long n = 0; while (s[n]) n++; sys(64, 1, (long)s, n, 0); }
void _start(void)
{
	struct { void (*h)(int); unsigned long flags; void (*r)(void); unsigned long mask; } sa = { handler, 0x04000000, restorer, 0 };
	sys(134, 14, (long)&sa, 0, 8);
	long it[4] = { 0, 0, 0, 100000 };
	sys(103, 0, (long)it, 0, 0);
	say("hydo-spin: start " NAME "\n");
#if MODE == 1
	while (fired) ;
#elif MODE == 2
	while (__atomic_load_n(&fired, __ATOMIC_ACQUIRE)) ;
#elif MODE == 3
	while (fired) asm volatile("yield");
#elif MODE == 4
	while (fired) sink = 1;
#elif MODE == 5
	unsigned long n = 0;
	while (fired) n++;
	count_out = n;
#elif MODE == 6
	for (;;) { int v; asm volatile("ldaxr %w0, [%1]" : "=r"(v) : "r"(&fired) : "memory"); if (!v) break; }
#endif
	say("hydo-spin: done " NAME "\n");
	sys(93, 0, 0, 0, 0);
}

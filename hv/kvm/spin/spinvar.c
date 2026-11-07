/* spinvar for x86_64: exit-free spin-waits for the KVM engine's spin observer (the Mac's
 * hv/hvm/spin/spinvar.c for x86_64 Linux). Each arms a 100 ms interval timer and busy-waits in
 * user space for the SIGALRM handler to clear `fired`, with no system call and no clock read:
 *   1 load   while (fired) ;          a pure read loop: an exact cycle (resolved in every run)
 *   5 count  while (fired) n++;       an iteration counter in a register: a counter loop, which
 *                                     only the rewind mode resolves (Milestone 161)
 * Built by build.sh with the pinned static gcc (docs/BUILD-PINS.md):
 *   gcc -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -fno-stack-protector -O2
 *       -DMODE=1 -DNAME='"load"' -o spin_load spinvar.c   (and MODE=5, NAME "count") */
static long sys(long n, long a, long b, long c, long d)
{
	long r;
	register long r10 asm("r10") = d;
	asm volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10) : "rcx", "r11", "memory");
	return r;
}
static volatile int fired = 2;
static volatile unsigned long count_out = 7;
static void handler(int s) { (void)s; fired = 0; }
extern void restorer(void);
asm(".text\n.global restorer\nrestorer:\n mov $15, %eax\n syscall\n");
static void say(const char *s) { long n = 0; while (s[n]) n++; sys(1, 1, (long)s, n, 0); }
void _start(void)
{
	/* rt_sigaction(SIGALRM, {handler, SA_RESTORER, restorer, 0}, 0, 8) */
	struct { void (*h)(int); unsigned long flags; void (*r)(void); unsigned long mask; } sa = { handler, 0x04000000, restorer, 0 };
	sys(13, 14, (long)&sa, 0, 8);
	/* setitimer(ITIMER_REAL, {interval 0, value 100 ms}) */
	long it[4] = { 0, 0, 0, 100000 };
	sys(38, 0, (long)it, 0, 0);
	say("hydo-spin: start " NAME "\n");
#if MODE == 1
	while (fired) ;
#elif MODE == 5
	unsigned long n = 0;
	while (fired) n++;
	count_out = n;
#endif
	say("hydo-spin: done " NAME "\n");
	sys(60, 0, 0, 0, 0);
	for (;;) ;
}

/* cntvct: read the virtual counter directly from user space (EL0), as JITs and some
 * libcs do, and print the values. Under hvm the kernel traps EL0 CNTVCT_EL0 reads and
 * answers with the deterministic physical counter. Static, no libc:
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -O2 -o cntvct cntvct.c */
static long sys(long n, long a, long b, long c)
{
	register long x8 asm("x8") = n, x0 asm("x0") = a, x1 asm("x1") = b, x2 asm("x2") = c;
	asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
	return x0;
}
static void put(const char *s, unsigned long n) { sys(64, 1, (long)s, n); }
static void hex(unsigned long v)
{
	char b[17]; for (int i = 15; i >= 0; i--) { b[i] = "0123456789abcdef"[v & 15]; v >>= 4; } b[16] = ' ';
	put(b, 17);
}
void _start(void)
{
	put("hydo-hvm: EL0 cntvct ", 21);
	for (int i = 0; i < 4; i++) { unsigned long v; asm volatile("isb; mrs %0, cntvct_el0" : "=r"(v)); hex(v); }
	put("\n", 1);
	sys(93, 0, 0, 0);
}

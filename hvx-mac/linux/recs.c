/* recs: the durability workload and oracle for the virtio-blk tests. Static, no libc:
 *   aarch64-unknown-linux-gnu-gcc -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -O2 -o recs recs.c
 *
 *   recs write <file> <n> <nsync>   write records 0..n-1 (4 KiB each, at offset i*4096); fsync after
 *                                   each of the first <nsync> ("acked": durable before the ack line)
 *   recs check <file> <n> <nsync>   read them back: every acked record must be intact; prints
 *                                   HYDO-ORACLE: OK|VIOLATION and which records are present
 * Record i: "REC" + 4 decimal digits, then bytes (i*37 + k) & 0xff. Deterministic, no time, no RNG. */
/* gcc may turn loops into calls to these even with -nostdlib */
void *memset(void *d, int c, unsigned long n) { char *p = d; while (n--) *p++ = (char)c; return d; }
void *memcpy(void *d, const void *s, unsigned long n) { char *p = d; const char *q = s; while (n--) *p++ = *q++; return d; }
static long sys(long n, long a, long b, long c, long d)
{
	register long x8 asm("x8") = n, x0 asm("x0") = a, x1 asm("x1") = b, x2 asm("x2") = c, x3 asm("x3") = d;
	asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3) : "memory");
	return x0;
}
#define RS 4096
static char buf[RS] = { 1 };          /* in .data (Linux 6.6 cannot load a bss-only segment) */
static char line[160] = { 1 };
static unsigned long slen(const char *s) { unsigned long n = 0; while (s[n]) n++; return n; }
static void put(const char *s) { sys(64, 1, (long)s, slen(s), 0); }
static int num(const char *s) { int v = 0; while (*s >= '0' && *s <= '9') v = v * 10 + (*s++ - '0'); return v; }
static char *utoa(char *p, int v) { char t[12]; int n = 0; do { t[n++] = '0' + v % 10; v /= 10; } while (v); while (n) *p++ = t[--n]; *p = 0; return p; }
static void say(const char *a, int v, const char *b)
{ char *p = line; for (const char *s = a; *s; ) *p++ = *s++; p = utoa(p, v); for (const char *s = b; *s; ) *p++ = *s++; *p++ = '\n'; *p = 0; put(line); }
static void fill(int i)
{
	buf[0] = 'R'; buf[1] = 'E'; buf[2] = 'C';
	buf[3] = '0' + i / 1000 % 10; buf[4] = '0' + i / 100 % 10; buf[5] = '0' + i / 10 % 10; buf[6] = '0' + i % 10;
	for (int k = 7; k < RS; k++) buf[k] = (char)((i * 37 + k) & 0xff);
}
static int same(int i)
{
	static char want[RS] = { 1 };
	for (int k = 0; k < RS; k++) want[k] = buf[k];
	fill(i);
	for (int k = 0; k < RS; k++) if (want[k] != buf[k]) return 0;
	return 1;
}
void cmain(long *sp)
{
	long argc = sp[0]; char **argv = (char **)(sp + 1);
	if (argc < 5) { put("usage: recs write|check <file> <n> <nsync>\n"); sys(93, 2, 0, 0, 0); }
	int n = num(argv[3]), nsync = num(argv[4]);
	if (argv[1][0] == 'w') {
		long fd = sys(56, -100, (long)argv[2], 2 | 64, 0644);          /* openat(AT_FDCWD, O_RDWR|O_CREAT) */
		if (fd < 0) { put("HYDO-ORACLE: VIOLATION cannot open the record file\n"); sys(93, 1, 0, 0, 0); }
		for (int i = 0; i < n; i++) {
			fill(i);
			if (sys(68, fd, (long)buf, RS, (long)i * RS) != RS) { say("hydo-rec: write failed at record ", i, ""); sys(93, 1, 0, 0, 0); }
			if (i < nsync) {
				if (sys(82, fd, 0, 0, 0) != 0) { say("hydo-rec: fsync failed at record ", i, ""); sys(93, 1, 0, 0, 0); }
				say("hydo-rec: acked ", i, " durable");
			} else say("hydo-rec: wrote ", i, " (not synced)");
		}
		sys(57, fd, 0, 0, 0);
		sys(93, 0, 0, 0, 0);
	}
	long fd = sys(56, -100, (long)argv[2], 0, 0);
	if (fd < 0) { put("HYDO-ORACLE: VIOLATION the record file is missing\n"); sys(93, 1, 0, 0, 0); }
	static char map[257] = { 1 }; int bad = -1, present = 0;
	for (int i = 0; i < n && i < 255; i++) {
		long r = sys(67, fd, (long)buf, RS, (long)i * RS);
		int ok = r == RS && same(i);
		map[i] = ok ? '1' : '0'; present += ok;
		if (!ok && i < nsync && bad < 0) bad = i;
	}
	map[n < 255 ? n : 255] = 0;
	put("hydo-rec: present "); put(map); put("\n");
	if (bad >= 0) say("HYDO-ORACLE: VIOLATION record=", bad, " was acked durable but is missing or corrupt");
	else say("HYDO-ORACLE: OK acked=", nsync, " durable");
	sys(93, 0, 0, 0, 0);
}
asm(".text\n.global _start\n_start:\n mov x0, sp\n b cmain\n");

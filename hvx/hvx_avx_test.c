// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_avx_test - guest AVX/AVX2 (HVX_VM_X86_64_V3).
 *
 * A hand-written long-mode guest, in two parts separated by a HLT (so the guest's
 * YMM registers must survive an HVX_RUN boundary, i.e. the module's XSAVE/XRSTOR):
 *   part 1: CPUID leaves 1/7/0xD/0x80000001 before and after setting CR4.OSXSAVE,
 *           XGETBV (reset value 1), five invalid XSETBVs that must each raise #GP
 *           (ECX != 0, bit 0 clear, AVX without SSE, a bit outside 0x7, a bit above
 *           31), a valid XSETBV of 7, XGETBV again, then it loads YMM0-15 from a
 *           table, does VPADDD into YMM15, and halts;
 *   part 2: stores YMM0-15, runs FMA / F16C / BMI1 / BMI2 / LZCNT / MOVBE on known
 *           inputs, turns AVX off and on again (XSETBV 3 then 7; the upper halves
 *           must come back zero), then VZEROALL and halts.
 * Checks: every CPUID/XGETBV value, the #GP count, YMM values and results equal the
 * host's own computation; a snapshot taken at the part-1 HLT and restored into a
 * FRESH VM (after a clobbering guest ran in it) finishes with the same fingerprint as
 * the straight run; the whole thing twice gives the same fingerprint; and without the
 * flag CPUID shows no XSAVE/AVX and XSETBV still exits to userspace (reason 55).
 *
 * Build: cc -O2 -Wall -o hvx_avx_test hvx_avx_test.c   (the host checks use target attributes)
 * Run:   sudo ./hvx_avx_test          (HVX_DEV=/dev/hvxavx for a side module)
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <immintrin.h>

#include "hvx_abi.h"

#define EXIT_HLT	12
#define EXIT_XSETBV	55
#define MEMB		(64UL * 4096)
#define R		0x5000UL	/* scalar results */
#define TAB		0x6000UL	/* 16 x 32-byte YMM inputs */
#define OUT		0x7000UL	/* stored YMM0-15 */
#define RES2		0x7200UL	/* part-2 results */
#define GP_COUNT	0x5f00UL	/* our #GP handler counts here */
#define GPA_ISR		0x2800UL

/* #GP handler: drop the error code, skip the 3-byte XSETBV, count, return */
static const unsigned char gp_isr[] = {
	0x48, 0x83, 0xc4, 0x08,			/* add $8,%rsp */
	0x48, 0x83, 0x04, 0x24, 0x03,		/* addq $3,(%rsp) */
	0x48, 0xff, 0x04, 0x25, 0x00, 0x5f, 0x00, 0x00,	/* incq 0x5f00 */
	0x48, 0xcf,				/* iretq */
};

extern const unsigned char g1_s[], g1_e[], g2_s[], g2_e[], gc_s[], gc_e[];
__asm__(
	".pushsection .rodata\n"
	"g1_s:\n"
	"  mov $0x5000, %edi\n"
	"  mov $1, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %ecx, 0(%rdi)\n"
	"  mov %cr4, %rax\n  or $0x40000, %rax\n  mov %rax, %cr4\n"
	"  mov $1, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %ecx, 4(%rdi)\n"
	"  mov $7, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %ebx, 8(%rdi)\n"
	"  mov $0xd, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %eax, 12(%rdi)\n  mov %ebx, 16(%rdi)\n  mov %ecx, 20(%rdi)\n"
	"  mov $0xd, %eax\n  mov $2, %ecx\n  cpuid\n  mov %eax, 24(%rdi)\n  mov %ebx, 28(%rdi)\n"
	"  mov $0x80000001, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %ecx, 32(%rdi)\n"
	"  xor %ecx, %ecx\n  xgetbv\n  mov %eax, 36(%rdi)\n"
	"  mov $1, %ecx\n  mov $7, %eax\n  xor %edx, %edx\n  xsetbv\n"	/* ECX != 0 */
	"  xor %ecx, %ecx\n  xor %eax, %eax\n  xsetbv\n"			/* bit 0 clear */
	"  mov $5, %eax\n  xsetbv\n"						/* AVX w/o SSE */
	"  mov $0xf, %eax\n  xsetbv\n"						/* bit 3 */
	"  mov $7, %eax\n  mov $1, %edx\n  xsetbv\n"				/* bit 32 */
	"  xor %edx, %edx\n  mov $7, %eax\n  xsetbv\n"				/* valid */
	"  xor %ecx, %ecx\n  xgetbv\n  mov %eax, 40(%rdi)\n"
	"  mov $0xd, %eax\n  xor %ecx, %ecx\n  cpuid\n  mov %ebx, 44(%rdi)\n"
	"  mov $0x6000, %esi\n"
	"  vmovdqu 0x000(%rsi), %ymm0\n  vmovdqu 0x020(%rsi), %ymm1\n  vmovdqu 0x040(%rsi), %ymm2\n"
	"  vmovdqu 0x060(%rsi), %ymm3\n  vmovdqu 0x080(%rsi), %ymm4\n  vmovdqu 0x0a0(%rsi), %ymm5\n"
	"  vmovdqu 0x0c0(%rsi), %ymm6\n  vmovdqu 0x0e0(%rsi), %ymm7\n  vmovdqu 0x100(%rsi), %ymm8\n"
	"  vmovdqu 0x120(%rsi), %ymm9\n  vmovdqu 0x140(%rsi), %ymm10\n  vmovdqu 0x160(%rsi), %ymm11\n"
	"  vmovdqu 0x180(%rsi), %ymm12\n  vmovdqu 0x1a0(%rsi), %ymm13\n  vmovdqu 0x1c0(%rsi), %ymm14\n"
	"  vpaddd %ymm1, %ymm0, %ymm15\n"
	"  hlt\n"
	"g1_e:\n"
	"g2_s:\n"
	"  mov $0x7000, %edi\n"
	"  vmovdqu %ymm0, 0x000(%rdi)\n  vmovdqu %ymm1, 0x020(%rdi)\n  vmovdqu %ymm2, 0x040(%rdi)\n"
	"  vmovdqu %ymm3, 0x060(%rdi)\n  vmovdqu %ymm4, 0x080(%rdi)\n  vmovdqu %ymm5, 0x0a0(%rdi)\n"
	"  vmovdqu %ymm6, 0x0c0(%rdi)\n  vmovdqu %ymm7, 0x0e0(%rdi)\n  vmovdqu %ymm8, 0x100(%rdi)\n"
	"  vmovdqu %ymm9, 0x120(%rdi)\n  vmovdqu %ymm10, 0x140(%rdi)\n  vmovdqu %ymm11, 0x160(%rdi)\n"
	"  vmovdqu %ymm12, 0x180(%rdi)\n  vmovdqu %ymm13, 0x1a0(%rdi)\n  vmovdqu %ymm14, 0x1c0(%rdi)\n"
	"  vmovdqu %ymm15, 0x1e0(%rdi)\n"
	"  mov $0x7200, %edi\n"
	"  vfmadd231ps %ymm5, %ymm4, %ymm6\n  vmovdqu %ymm6, 0(%rdi)\n"
	"  vcvtps2ph $0, %ymm6, %xmm7\n  vmovdqu %xmm7, 0x20(%rdi)\n"
	"  mov 0x6000, %rax\n  mov 0x6008, %rcx\n"
	"  andn %rcx, %rax, %rdx\n  mov %rdx, 0x30(%rdi)\n"
	"  pdep %rcx, %rax, %rdx\n  mov %rdx, 0x38(%rdi)\n"
	"  lzcnt %rcx, %rdx\n  mov %rdx, 0x40(%rdi)\n"
	"  movbe 0x6000, %rdx\n  mov %rdx, 0x48(%rdi)\n"
	"  xor %ecx, %ecx\n  xor %edx, %edx\n  mov $3, %eax\n  xsetbv\n  mov $7, %eax\n  xsetbv\n"
	"  vmovdqu %ymm0, 0x60(%rdi)\n"
	"  vzeroall\n"
	"  hlt\n"
	"g2_e:\n"
	"gc_s:\n"			/* clobber: AVX on, every YMM = all-ones */
	"  mov %cr4, %rax\n  or $0x40000, %rax\n  mov %rax, %cr4\n"
	"  xor %ecx, %ecx\n  xor %edx, %edx\n  mov $7, %eax\n  xsetbv\n"
	"  vpcmpeqd %ymm0, %ymm0, %ymm0\n"
	"  vmovdqa %ymm0, %ymm1\n  vmovdqa %ymm0, %ymm2\n  vmovdqa %ymm0, %ymm3\n  vmovdqa %ymm0, %ymm4\n"
	"  vmovdqa %ymm0, %ymm5\n  vmovdqa %ymm0, %ymm6\n  vmovdqa %ymm0, %ymm7\n  vmovdqa %ymm0, %ymm8\n"
	"  vmovdqa %ymm0, %ymm9\n  vmovdqa %ymm0, %ymm10\n  vmovdqa %ymm0, %ymm11\n  vmovdqa %ymm0, %ymm12\n"
	"  vmovdqa %ymm0, %ymm13\n  vmovdqa %ymm0, %ymm14\n  vmovdqa %ymm0, %ymm15\n"
	"  hlt\n"
	"gc_e:\n"
	".popsection\n");

#define G1	0x3000UL
#define G2	0x3800UL
#define GC	0x4000UL

static const char *dev(void) { const char *d = getenv("HVX_DEV"); return d ? d : "/dev/hvx"; }

static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const unsigned char *b = p;
	while (n--) h = (h ^ *b++) * 0x100000001b3ULL;
	return h;
}

static int vm_new(uint32_t flags, unsigned char **mem)
{
	struct hvx_config cfg = { .mem_size = MEMB, .flags = flags };
	int fd = open(dev(), O_RDWR);

	if (fd < 0) { perror("open"); return -1; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) {
		if (errno == ENODEV && (flags & HVX_VM_X86_64_V3)) {
			printf("hvx_avx_test: SKIP (no host AVX2, or the module lacks guest AVX)\n");
			exit(0);
		}
		perror("HVX_CREATE_VM"); close(fd); return -1;
	}
	*mem = mmap(NULL, MEMB, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (*mem == MAP_FAILED) { perror("mmap"); close(fd); return -1; }
	memcpy(*mem + GPA_ISR, gp_isr, sizeof(gp_isr));
	memcpy(*mem + G1, g1_s, g1_e - g1_s);
	memcpy(*mem + G2, g2_s, g2_e - g2_s);
	memcpy(*mem + GC, gc_s, gc_e - gc_s);
	return fd;
}

/* run from rip (or the current rip when rip == 0) to the next exit */
static int run_at(int fd, uint64_t rip, uint32_t *reason)
{
	struct hvx_regs r;
	struct hvx_exit ex;

	if (ioctl(fd, HVX_GET_REGS, &r) < 0) return -1;
	if (rip) {
		r.rip = rip;
		r.rsp = MEMB - 16;
		r.rflags = 0x2;
		if (ioctl(fd, HVX_SET_REGS, &r) < 0) return -1;
	}
	for (;;) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == HVX_EXIT_THROTTLE) continue;
		*reason = ex.reason;
		return 0;
	}
}

static int to_hlt(int fd, uint64_t rip)
{
	uint32_t why;

	if (run_at(fd, rip, &why) < 0) return -1;
	if (why != EXIT_HLT) { fprintf(stderr, "unexpected exit %u\n", why); return -1; }
	return 0;
}

static void fill_tab(unsigned char *m)
{
	int i, k;

	for (i = 0; i < 16; i++) {
		if (i >= 4 && i <= 6) {
			float *f = (float *)(m + TAB + 32 * i);
			for (k = 0; k < 8; k++) f[k] = 0.25f * (k + 1) + (float)i;
		} else {
			uint32_t *u = (uint32_t *)(m + TAB + 32 * i);
			for (k = 0; k < 8; k++) u[k] = 0x01020304u * (uint32_t)(i + 1) + 0x9e3779b9u * (uint32_t)k;
		}
	}
}

static uint64_t state_fp(int fd, unsigned char *m)
{
	struct hvx_snapshot *s = calloc(1, sizeof(*s));
	uint64_t h = 0xcbf29ce484222325ULL;

	ioctl(fd, HVX_GET_SNAPSHOT, s);
	h = fnv(h, &s->regs, sizeof(s->regs));
	h = fnv(h, s->fx, sizeof(s->fx));
	h = fnv(h, &s->xcr0, sizeof(s->xcr0));
	h = fnv(h, s->xsave_ext, sizeof(s->xsave_ext));
	h = fnv(h, m + R, 0x100);
	h = fnv(h, m + OUT, 0x300);
	free(s);
	return h;
}

static int fails;
#define CHECK(c, ...) do { if (!(c)) { fails++; printf("  FAIL: " __VA_ARGS__); printf("\n"); } } while (0)

/* the guest's part-2 results against the host's own AVX2/FMA/F16C/BMI computation */
__attribute__((target("avx2,fma,f16c,bmi,bmi2,lzcnt")))
static void check_part2(unsigned char *m)
{
	unsigned char *o = m + OUT, *t = m + TAB, *q = m + RES2;
	__m256i a = _mm256_loadu_si256((void *)t), b = _mm256_loadu_si256((void *)(t + 32));
	__m256 x = _mm256_loadu_ps((void *)(t + 128)), y = _mm256_loadu_ps((void *)(t + 160)),
	       z = _mm256_loadu_ps((void *)(t + 192)), f = _mm256_fmadd_ps(x, y, z);
	__m256i sum = _mm256_add_epi32(a, b);
	__m128i h = _mm256_cvtps_ph(f, 0);
	uint64_t ra = *(uint64_t *)t, rc = *(uint64_t *)(t + 8);
	int i, ok = 1;
	unsigned char zero[16] = {0};

	for (i = 0; i < 15; i++)
		ok &= !memcmp(o + 32 * i, t + 32 * i, 32);
	CHECK(ok, "YMM0-14 did not survive the HVX_RUN boundary");
	CHECK(!memcmp(o + 480, &sum, 32), "VPADDD result (YMM15) wrong");
	CHECK(!memcmp(q, &f, 32), "FMA result wrong");
	CHECK(!memcmp(q + 0x20, &h, 16), "F16C result wrong");
	CHECK(*(uint64_t *)(q + 0x30) == _andn_u64(ra, rc), "ANDN wrong");
	CHECK(*(uint64_t *)(q + 0x38) == _pdep_u64(ra, rc), "PDEP wrong");
	CHECK(*(uint64_t *)(q + 0x40) == (uint64_t)_lzcnt_u64(rc), "LZCNT wrong");
	CHECK(*(uint64_t *)(q + 0x48) == __builtin_bswap64(ra), "MOVBE wrong");
	CHECK(!memcmp(q + 0x60, t, 16) && !memcmp(q + 0x70, zero, 16),
	      "AVX off/on: lower half not kept or upper half not zero");
}

/* one full pass: straight run + snapshot/restore into a fresh VM; returns the fingerprint */
static uint64_t pass(int verbose)
{
	unsigned char *m, *m2, *ram;
	int fd, fd2;
	uint32_t *r;
	struct hvx_snapshot *snap;
	uint64_t fp_a, fp_b;
	uint32_t c1, c1o;

	if ((fd = vm_new(HVX_VM_X86_64_V3, &m)) < 0) exit(1);
	fill_tab(m);
	if (to_hlt(fd, G1) < 0) exit(1);
	r = (uint32_t *)(m + R);
	c1 = r[0]; c1o = r[1];
	if (verbose)
		printf("  leaf1.ecx=%08x (OSXSAVE on: %08x) leaf7.ebx=%08x leafD.0=%x/%u/%u leafD.2=%u@%u "
		       "ext.ecx=%08x xgetbv=%u->%u leafD.0.ebx->%u #GP=%" PRIu64 "\n",
		       c1, c1o, r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11],
		       *(uint64_t *)(m + GP_COUNT));
	CHECK((c1 & (1u << 26)) && (c1 & (1u << 28)) && (c1 & (1u << 12)) && (c1 & (1u << 29)) &&
	      (c1 & (1u << 22)) && !(c1 & (1u << 27)), "leaf 1 ECX before OSXSAVE %08x", c1);
	CHECK(c1o == (c1 | (1u << 27)), "leaf 1 ECX does not mirror CR4.OSXSAVE %08x", c1o);
	CHECK(r[2] == ((1u << 3) | (1u << 5) | (1u << 8)), "leaf 7 EBX %08x", r[2]);
	CHECK(r[3] == 7 && r[4] == 576 && r[5] == 832, "leaf 0xD.0 %x %u %u", r[3], r[4], r[5]);
	CHECK(r[6] == 256 && r[7] == 576, "leaf 0xD.2 %u %u", r[6], r[7]);
	CHECK((r[8] & 0x21) == 0x21, "leaf 0x80000001 ECX %08x", r[8]);
	CHECK(r[9] == 1 && r[10] == 7, "XGETBV %u -> %u", r[9], r[10]);
	CHECK(r[11] == 832, "leaf 0xD.0 EBX after XCR0=7: %u", r[11]);
	CHECK(*(uint64_t *)(m + GP_COUNT) == 5, "#GP count %" PRIu64, *(uint64_t *)(m + GP_COUNT));

	/* snapshot at the part-1 HLT (RIP -> part 2) */
	{
		struct hvx_regs rg;
		ioctl(fd, HVX_GET_REGS, &rg);
		rg.rip = G2;
		ioctl(fd, HVX_SET_REGS, &rg);
	}
	snap = calloc(1, sizeof(*snap));
	ram = malloc(MEMB);
	ioctl(fd, HVX_GET_SNAPSHOT, snap);
	memcpy(ram, m, MEMB);
	CHECK(snap->xcr0 == 7, "snapshot xcr0 %" PRIx64, (uint64_t)snap->xcr0);
	if (verbose)
		printf("  snapshot at part-1 HLT: xcr0=%" PRIx64 " XSTATE_BV=%" PRIx64 "\n",
		       (uint64_t)snap->xcr0, *(uint64_t *)snap->xsave_ext);

	/* straight: finish part 2 */
	if (to_hlt(fd, 0) < 0) exit(1);
	check_part2(m);
	{
		struct hvx_snapshot *e = calloc(1, sizeof(*e));
		unsigned char z[256] = {0};
		ioctl(fd, HVX_GET_SNAPSHOT, e);
		CHECK(!memcmp(e->xsave_ext + 64, z, 256), "YMM upper halves not zero after VZEROALL");
		if (verbose)
			printf("  after VZEROALL: XSTATE_BV=%" PRIx64 "\n", *(uint64_t *)e->xsave_ext);
		free(e);
	}
	fp_a = state_fp(fd, m);

	/* branch: fresh VM, clobber its YMM state, then restore the snapshot and finish */
	if ((fd2 = vm_new(HVX_VM_X86_64_V3, &m2)) < 0) exit(1);
	if (to_hlt(fd2, GC) < 0) exit(1);
	memcpy(m2, ram, MEMB);
	if (ioctl(fd2, HVX_SET_SNAPSHOT, snap) < 0) { perror("SET_SNAPSHOT"); exit(1); }
	if (to_hlt(fd2, 0) < 0) exit(1);
	fp_b = state_fp(fd2, m2);
	printf("  straight %016" PRIx64 "  restored-into-fresh-VM %016" PRIx64 " %s\n", fp_a, fp_b,
	       fp_a == fp_b ? "IDENTICAL" : "DIFFER");
	CHECK(fp_a == fp_b, "branch-from-snapshot differs");
	munmap(m, MEMB); munmap(m2, MEMB); close(fd); close(fd2);
	free(snap); free(ram);
	return fp_a;
}

int main(void)
{
	unsigned char *m;
	uint64_t f1, f2;
	uint32_t why;
	int fd;

	printf("hvx_avx_test: device %s\n", dev());
	f1 = pass(1);
	f2 = pass(0);
	printf("  pass 1 %016" PRIx64 "  pass 2 %016" PRIx64 " %s\n", f1, f2, f1 == f2 ? "IDENTICAL" : "DIFFER");
	CHECK(f1 == f2, "two passes differ");

	/* flag off: no XSAVE/AVX in CPUID, XSETBV is not emulated */
	if ((fd = vm_new(0, &m)) < 0) return 1;
	fill_tab(m);
	if (run_at(fd, G1, &why) < 0) return 1;
	printf("  flag off: leaf1.ecx=%08x leaf7.ebx=%08x leafD.0.eax=%x first stop reason=%u\n",
	       ((uint32_t *)(m + R))[0], ((uint32_t *)(m + R))[2], ((uint32_t *)(m + R))[3], why);
	CHECK(!(((uint32_t *)(m + R))[0] & ((1u << 26) | (1u << 28))) && ((uint32_t *)(m + R))[2] == 0 &&
	      ((uint32_t *)(m + R))[3] == 0, "flag off: CPUID shows XSAVE/AVX");
	CHECK(why == EXIT_XSETBV || why == 0x80000021 || why == 2, "flag off: unexpected stop %u", why);
	close(fd);

	printf("hvx_avx_test: %s (fingerprint %016" PRIx64 ")\n", fails ? "FAIL" : "PASS", f1);
	return fails ? 1 : 0;
}

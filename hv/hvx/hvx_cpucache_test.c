// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_cpucache_test - the cache leaves of CPUID with and without HVX_VM_CPU_CACHE (milestone 195).
 *
 * A guest runs CPUID for a list of (leaf, sub-leaf) pairs and stores the four registers of
 * each. The test makes one VM without the flag and one with it and checks:
 *   1. without the flag, the cache leaves (2, 4 with every sub-leaf, 0x80000005, 0x80000006)
 *      are zero and the max extended leaf is 0x80000001, exactly as before milestone 195;
 *   2. with it, leaf 2 is HVX_CPUID2_EAX, leaf 4's sub-leaves 0..3 are HVX_CPUID4_SUB (one
 *      logical processor) and the sub-leaves after them are zero, the max extended leaf is
 *      0x80000006 and its ECX is HVX_CPUID_E6_ECX, and every other leaf answers as without it;
 *   3. a second run of each VM gives the same bytes.
 * HVX_GET_CAPS must list HVX_VM_CPU_CACHE. A module older than milestone 195 refuses
 * HVX_GET_CAPS: the test then checks only (1) and (3) without the flag, and passes (so the
 * regression suite can run it against the shared module before it is reloaded).
 *
 * Build: cc -O2 -Wall -o hvx_cpucache_test hvx_cpucache_test.c
 * Run:   sudo ./hvx_cpucache_test     (through ~/bin/hvx-run on the bare-metal machine)
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

#include "hvx_abi.h"

/* deploy-metal.sh compiles the tests against the bare-metal machine's header, which stays the
 * loaded module's until the module is reloaded: the milestone 195 definitions, as hvx_abi.h has
 * them, for a header that predates them */
#ifndef HVX_GET_CAPS
#define HVX_VM_CPU_CACHE	(1u << 10)
#define HVX_CPUID2_EAX		0x0000ff01u
#define HVX_CPUID4_N		4
#define HVX_CPUID4_SUB { \
	{ 0x00000121u, 0x01c0003fu, 0x0000003fu, 0x00000000u }, \
	{ 0x00000122u, 0x01c0003fu, 0x0000003fu, 0x00000000u }, \
	{ 0x00000143u, 0x00c0003fu, 0x000003ffu, 0x00000000u }, \
	{ 0x00000163u, 0x03c0003fu, 0x00001fffu, 0x00000006u }, \
}
#define HVX_CPUID_E6_ECX	0x01004040u
struct hvx_caps {
	__u32 vm_flags;
	__u32 reserved[7];
};
#define HVX_GET_CAPS	_IOR(HVX_IOC_MAGIC, 70, struct hvx_caps)
#endif

#define EXIT_HLT        12
#define GUEST_MEM_BYTES (64UL * 4096)
#define TABLE_GPA       0x4000UL
#define RESULT_GPA      0x5000UL

struct q { uint32_t leaf, sub; };
static const struct q queries[] = {
	{ 0, 0 }, { 1, 0 }, { 2, 0 },
	{ 4, 0 }, { 4, 1 }, { 4, 2 }, { 4, 3 }, { 4, 4 }, { 4, 5 },
	{ 7, 0 }, { 0xb, 0 }, { 0xb, 1 }, { 0x1f, 0 },
	{ 0x80000000u, 0 }, { 0x80000001u, 0 }, { 0x80000002u, 0 }, { 0x80000005u, 0 },
	{ 0x80000006u, 0 }, { 0x80000008u, 0 },
};
#define NQ (sizeof(queries) / sizeof(queries[0]))

/*
 * Guest code at HVX_GUEST_CODE_BASE: for each (leaf, sub-leaf) pair at TABLE_GPA until leaf
 * 0xffffffff, CPUID and store EAX, EBX, ECX, EDX at RESULT_GPA + 16 * i; then HLT.
 *   mov rsi, TABLE_GPA ; mov rdi, RESULT_GPA
 * L: mov eax,[rsi] ; cmp eax,-1 ; je D ; mov ecx,[rsi+4] ; cpuid
 *    mov [rdi],eax ; mov [rdi+4],ebx ; mov [rdi+8],ecx ; mov [rdi+12],edx
 *    add rsi,8 ; add rdi,16 ; jmp L
 * D: hlt
 */
static const unsigned char guest_code[] = {
	0x48, 0xbe, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,	/* mov rsi,0x4000 */
	0x48, 0xbf, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,	/* mov rdi,0x5000 */
	0x8b, 0x06,				/* L: mov eax,[rsi] */
	0x83, 0xf8, 0xff,			/* cmp eax,-1 */
	0x74, 0x1a,				/* je D */
	0x8b, 0x4e, 0x04,			/* mov ecx,[rsi+4] */
	0x0f, 0xa2,				/* cpuid */
	0x89, 0x07,				/* mov [rdi],eax */
	0x89, 0x5f, 0x04,			/* mov [rdi+4],ebx */
	0x89, 0x4f, 0x08,			/* mov [rdi+8],ecx */
	0x89, 0x57, 0x0c,			/* mov [rdi+12],edx */
	0x48, 0x83, 0xc6, 0x08,			/* add rsi,8 */
	0x48, 0x83, 0xc7, 0x10,			/* add rdi,16 */
	0xeb, 0xdf,				/* jmp L */
	0xf4,					/* D: hlt */
};

static int run_vm(uint32_t flags, uint32_t out[NQ][4])
{
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES, .flags = flags };
	struct hvx_tsc tsc = { .tsc = 0, .step = 1, .aux = 0, .mode = HVX_TSC_COUNTER };
	struct hvx_regs regs;
	struct hvx_exit ex;
	unsigned char *mem;
	uint32_t *t;
	unsigned i;
	int fd, rc = -1;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return -1; }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy(mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));
	t = (uint32_t *)(mem + TABLE_GPA);
	for (i = 0; i < NQ; i++) { t[2 * i] = queries[i].leaf; t[2 * i + 1] = queries[i].sub; }
	t[2 * NQ] = 0xffffffffu;
	memset(mem + RESULT_GPA, 0, NQ * 16);
	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); goto out; }
	memset(&regs, 0, sizeof(regs));
	regs.rip = HVX_GUEST_CODE_BASE;
	regs.rsp = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); goto out; }
	if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); goto out; }
	if (ex.reason != EXIT_HLT) {
		fprintf(stderr, "hvx_cpucache_test: unexpected exit reason=%u\n", ex.reason);
		goto out;
	}
	memcpy(out, mem + RESULT_GPA, NQ * 16);
	rc = 0;
out:
	close(fd);
	return rc;
}

static uint64_t fnv(const void *p, size_t n)
{
	const unsigned char *b = p;
	uint64_t h = 0xcbf29ce484222325ULL;

	while (n--) { h ^= *b++; h *= 0x100000001b3ULL; }
	return h;
}

static int bad;

static void want(const char *what, unsigned i, const uint32_t got[4], const uint32_t w[4])
{
	if (memcmp(got, w, 16) == 0)
		return;
	fprintf(stderr, "hvx_cpucache_test: FAIL (%s) CPUID 0x%x.%u = %08x %08x %08x %08x, expected "
		"%08x %08x %08x %08x\n", what, queries[i].leaf, queries[i].sub, got[0], got[1],
		got[2], got[3], w[0], w[1], w[2], w[3]);
	bad = 1;
}

static int is_cache_leaf(uint32_t leaf)
{
	return leaf == 2 || leaf == 4 || leaf == 0x80000000u || leaf == 0x80000005u ||
	       leaf == 0x80000006u;
}

int main(void)
{
	static const uint32_t sub4[HVX_CPUID4_N][4] = HVX_CPUID4_SUB;
	static const uint32_t zero[4];
	uint32_t off[NQ][4], off2[NQ][4], on[NQ][4], on2[NQ][4];
	struct hvx_caps caps;
	int fd, have_caps;
	unsigned i;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	memset(&caps, 0, sizeof(caps));
	have_caps = ioctl(fd, HVX_GET_CAPS, &caps) == 0;
	if (!have_caps && errno != ENOTTY) { perror("HVX_GET_CAPS"); return 1; }
	close(fd);

	if (run_vm(0, off) < 0 || run_vm(0, off2) < 0)
		return 1;
	if (memcmp(off, off2, sizeof(off))) { fprintf(stderr, "hvx_cpucache_test: FAIL: two runs without the flag differ\n"); return 1; }
	for (i = 0; i < NQ; i++) {
		uint32_t e[4] = { 0x80000001u, 0, 0, 0 };

		if (queries[i].leaf == 0x80000000u)
			want("without the flag", i, off[i], e);
		else if (is_cache_leaf(queries[i].leaf))
			want("without the flag", i, off[i], zero);
	}
	printf("hvx_cpucache_test: without HVX_VM_CPU_CACHE: leaves 2, 4.0-5, 0x80000005/6 zero, max extended leaf 0x80000001 (fp=%016" PRIx64 ")\n",
	       fnv(off, sizeof(off)));
	if (!have_caps) {
		printf("hvx_cpucache_test: this module refuses HVX_GET_CAPS (older than milestone 195): HVX_VM_CPU_CACHE not checked\n");
		if (bad) return 1;
		printf("hvx_cpucache_test: PASS (without the flag only)\n");
		return 0;
	}
	printf("hvx_cpucache_test: HVX_GET_CAPS vm_flags=0x%x\n", caps.vm_flags);
	if (!(caps.vm_flags & HVX_VM_CPU_CACHE)) {
		fprintf(stderr, "hvx_cpucache_test: FAIL: HVX_GET_CAPS lacks HVX_VM_CPU_CACHE\n");
		return 1;
	}
	if (run_vm(HVX_VM_CPU_CACHE, on) < 0 || run_vm(HVX_VM_CPU_CACHE, on2) < 0)
		return 1;
	if (memcmp(on, on2, sizeof(on))) { fprintf(stderr, "hvx_cpucache_test: FAIL: two runs with the flag differ\n"); return 1; }
	for (i = 0; i < NQ; i++) {
		uint32_t l = queries[i].leaf, s = queries[i].sub;
		uint32_t e[4] = { 0, 0, 0, 0 };

		if (l == 2)
			e[0] = HVX_CPUID2_EAX;
		else if (l == 4 && s < HVX_CPUID4_N)
			memcpy(e, sub4[s], 16);
		else if (l == 0x80000000u)
			e[0] = 0x80000006u;
		else if (l == 0x80000006u)
			e[2] = HVX_CPUID_E6_ECX;
		else if (!is_cache_leaf(l))
			memcpy(e, off[i], 16);	/* every other leaf as without the flag */
		want("with the flag", i, on[i], e);
		printf("hvx_cpucache_test:   CPUID 0x%08x.%u: %08x %08x %08x %08x\n", l, s,
		       on[i][0], on[i][1], on[i][2], on[i][3]);
	}
	if (bad) return 1;
	printf("hvx_cpucache_test: with HVX_VM_CPU_CACHE: fp=%016" PRIx64 "\n", fnv(on, sizeof(on)));
	printf("hvx_cpucache_test: PASS\n");
	return 0;
}

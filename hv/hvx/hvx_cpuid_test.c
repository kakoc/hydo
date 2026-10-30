// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_cpuid_test - prove deterministic in-kernel CPUID + MSR emulation (M12).
 *
 * Under stock hardware CPUID and RDMSR return facts about the REAL host CPU --
 * exactly the kind of thing that makes two "identical" runs diverge (different
 * host, microcode, or feature bits), and a prerequisite the guest OS reads very
 * early. Now that hvx owns the VM-execution controls, CPUID and RDMSR/WRMSR are
 * emulated IN-KERNEL against a fixed synthetic table: no exit reaches userspace,
 * and every run sees the same values.
 *
 * The guest queries CPUID leaves 0, 1 and 0x80000001; does a WRMSR+RDMSR round
 * trip on a scratch MSR (LSTAR); and RDMSRs IA32_TSC. It stores every result to
 * guest RAM (base 0x5000). The test asserts:
 *   1. the exact synthetic CPUID values hvx advertises (GenuineIntel, family 6,
 *      the curated feature masks -- and crucially NO XSAVE/AVX/OSXSAVE),
 *   2. the LSTAR write read back byte-for-byte and IA32_TSC read the virtual TSC,
 *   3. a second run reproduces every value bit-for-bit (determinism).
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_cpuid_test hvx_cpuid_test.c
 * Run   (inside L1):  sudo ./hvx_cpuid_test
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

#include "hvx_abi.h"

#define EXIT_HLT        12
#define GUEST_MEM_BYTES (64UL * 4096)
#define RESULT_GPA      0x5000UL

/* virtual TSC (COUNTER mode): high dword set so RDMSR IA32_TSC exercises EDX */
#define TSC_BASE        0x0000000abcdef000ULL

/* scratch MSR round-trip value written by the guest via WRMSR LSTAR */
#define LSTAR_VALUE     0x12345678deadbeefULL

/* expected synthetic CPUID results (must mirror hvx.c::hvx_cpuid) */
#define L0_EAX  0x00000016u	/* max basic leaf raised to 0x16 (M15: TSC-freq leaves) */
#define L0_EBX  0x756e6547u	/* "Genu" */
#define L0_ECX  0x6c65746eu	/* "ntel" */
#define L0_EDX  0x49656e69u	/* "ineI" */
#define L1_EAX  0x000006f1u
#define L1_EBX  0x00000800u
#define L1_ECX  ((1u<<21)|(1u<<24)|(1u<<31))	/* x2APIC | TSC-Deadline | HYPERVISOR (M15) */
#define L1_EDX  ((1u<<0)|(1u<<3)|(1u<<4)|(1u<<5)|(1u<<6)|(1u<<8)|(1u<<9)| \
		 (1u<<13)|(1u<<15)|(1u<<19)|(1u<<24)|(1u<<25)|(1u<<26))
#define LE_EAX  0x00000000u
#define LE_EBX  0x00000000u
#define LE_ECX  0x00000000u
#define LE_EDX  ((1u<<11)|(1u<<20)|(1u<<29))	/* SYSCALL | NX | LM */

/*
 * Guest machine code at HVX_GUEST_CODE_BASE (0x3000). rdi = result base 0x5000;
 * CPUID clobbers eax/ebx/ecx/edx but never rdi, so rdi survives as the pointer.
 *
 *   mov  rdi, 0x5000
 *   xor eax,eax; xor ecx,ecx; cpuid            ; leaf 0   -> [rdi+0x00..0x0c]
 *   mov eax,1;   xor ecx,ecx; cpuid            ; leaf 1   -> [rdi+0x10..0x1c]
 *   mov eax,0x80000001; xor ecx,ecx; cpuid     ; ext leaf -> [rdi+0x20..0x2c]
 *   mov ecx,LSTAR; mov eax,lo; mov edx,hi; wrmsr
 *   mov ecx,LSTAR; rdmsr                       ; LSTAR    -> [rdi+0x30] (u64)
 *   mov ecx,0x10; rdmsr                        ; IA32_TSC -> [rdi+0x38] (u64)
 *   hlt
 */
static const unsigned char guest_code[] = {
	0x48, 0xbf, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, /* mov rdi,0x5000 */
	/* leaf 0 */
	0x31, 0xc0,			/* xor eax,eax */
	0x31, 0xc9,			/* xor ecx,ecx */
	0x0f, 0xa2,			/* cpuid */
	0x89, 0x47, 0x00,		/* mov [rdi+0x00],eax */
	0x89, 0x5f, 0x04,		/* mov [rdi+0x04],ebx */
	0x89, 0x4f, 0x08,		/* mov [rdi+0x08],ecx */
	0x89, 0x57, 0x0c,		/* mov [rdi+0x0c],edx */
	/* leaf 1 */
	0xb8, 0x01, 0x00, 0x00, 0x00,	/* mov eax,1 */
	0x31, 0xc9,			/* xor ecx,ecx */
	0x0f, 0xa2,			/* cpuid */
	0x89, 0x47, 0x10,
	0x89, 0x5f, 0x14,
	0x89, 0x4f, 0x18,
	0x89, 0x57, 0x1c,
	/* leaf 0x80000001 */
	0xb8, 0x01, 0x00, 0x00, 0x80,	/* mov eax,0x80000001 */
	0x31, 0xc9,			/* xor ecx,ecx */
	0x0f, 0xa2,			/* cpuid */
	0x89, 0x47, 0x20,
	0x89, 0x5f, 0x24,
	0x89, 0x4f, 0x28,
	0x89, 0x57, 0x2c,
	/* WRMSR LSTAR = 0x12345678deadbeef */
	0xb9, 0x82, 0x00, 0x00, 0xc0,	/* mov ecx,0xc0000082 */
	0xb8, 0xef, 0xbe, 0xad, 0xde,	/* mov eax,0xdeadbeef */
	0xba, 0x78, 0x56, 0x34, 0x12,	/* mov edx,0x12345678 */
	0x0f, 0x30,			/* wrmsr */
	/* RDMSR LSTAR -> [rdi+0x30] */
	0xb9, 0x82, 0x00, 0x00, 0xc0,	/* mov ecx,0xc0000082 */
	0x0f, 0x32,			/* rdmsr */
	0x89, 0x47, 0x30,		/* mov [rdi+0x30],eax */
	0x89, 0x57, 0x34,		/* mov [rdi+0x34],edx */
	/* RDMSR IA32_TSC -> [rdi+0x38] */
	0xb9, 0x10, 0x00, 0x00, 0x00,	/* mov ecx,0x10 */
	0x0f, 0x32,			/* rdmsr */
	0x89, 0x47, 0x38,		/* mov [rdi+0x38],eax */
	0x89, 0x57, 0x3c,		/* mov [rdi+0x3c],edx */
	0xf4,				/* hlt */
};

/* Run the guest to HLT from a fresh TSC seed; copy the 64-byte result region. */
static int run_once(int fd, void *mem, unsigned char out[64])
{
	struct hvx_tsc tsc = { .tsc = TSC_BASE, .step = 1, .aux = 0, .mode = HVX_TSC_COUNTER };
	struct hvx_regs regs;
	struct hvx_exit ex;
	int i;

	if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	memset((unsigned char *)mem + RESULT_GPA, 0, 64);

	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }

	for (i = 0; i < 8; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); return -1; }
		if (ex.reason == EXIT_HLT)
			break;
		fprintf(stderr, "run_once: unexpected exit reason=%u qual=0x%" PRIx64 "\n",
			ex.reason, (uint64_t)ex.qualification);
		return -1;
	}
	memcpy(out, (unsigned char *)mem + RESULT_GPA, 64);
	return 0;
}

static uint32_t u32at(const unsigned char *p, int off)
{
	uint32_t v;
	memcpy(&v, p + off, sizeof(v));
	return v;
}
static uint64_t u64at(const unsigned char *p, int off)
{
	uint64_t v;
	memcpy(&v, p + off, sizeof(v));
	return v;
}

static int check_u32(const char *name, uint32_t got, uint32_t want)
{
	if (got != want) {
		fprintf(stderr, "hvx_cpuid_test: FAIL: %s=0x%08x, expected 0x%08x\n",
			name, got, want);
		return -1;
	}
	printf("hvx_cpuid_test:   %-12s = 0x%08x  OK\n", name, got);
	return 0;
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	unsigned char a[64], b[64];
	uint64_t lstar, tsc;
	int bad = 0;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	if (run_once(fd, mem, a) < 0) goto out;

	printf("hvx_cpuid_test: CPUID leaf 0 (vendor / max leaf):\n");
	bad |= check_u32("leaf0.eax", u32at(a, 0x00), L0_EAX);
	bad |= check_u32("leaf0.ebx", u32at(a, 0x04), L0_EBX);
	bad |= check_u32("leaf0.ecx", u32at(a, 0x08), L0_ECX);
	bad |= check_u32("leaf0.edx", u32at(a, 0x0c), L0_EDX);
	printf("hvx_cpuid_test: CPUID leaf 1 (family/features):\n");
	bad |= check_u32("leaf1.eax", u32at(a, 0x10), L1_EAX);
	bad |= check_u32("leaf1.ebx", u32at(a, 0x14), L1_EBX);
	bad |= check_u32("leaf1.ecx", u32at(a, 0x18), L1_ECX);
	bad |= check_u32("leaf1.edx", u32at(a, 0x1c), L1_EDX);
	printf("hvx_cpuid_test: CPUID leaf 0x80000001 (ext features):\n");
	bad |= check_u32("ext.eax", u32at(a, 0x20), LE_EAX);
	bad |= check_u32("ext.ebx", u32at(a, 0x24), LE_EBX);
	bad |= check_u32("ext.ecx", u32at(a, 0x28), LE_ECX);
	bad |= check_u32("ext.edx", u32at(a, 0x2c), LE_EDX);
	if (bad) goto out;

	/* ECX bit 26 (XSAVE) must be clear so the guest never runs XSETBV. */
	if (u32at(a, 0x18) & (1u << 26)) {
		fprintf(stderr, "hvx_cpuid_test: FAIL: leaf1.ecx advertises XSAVE\n");
		goto out;
	}

	lstar = u64at(a, 0x30);
	tsc   = u64at(a, 0x38);
	printf("hvx_cpuid_test: MSR round-trip: LSTAR=0x%016" PRIx64 " IA32_TSC=0x%016" PRIx64 "\n",
	       lstar, tsc);
	if (lstar != LSTAR_VALUE) {
		fprintf(stderr, "hvx_cpuid_test: FAIL: LSTAR read 0x%016" PRIx64
			", expected 0x%016" PRIx64 "\n", lstar, (uint64_t)LSTAR_VALUE);
		goto out;
	}
	if (tsc != TSC_BASE) {
		fprintf(stderr, "hvx_cpuid_test: FAIL: IA32_TSC read 0x%016" PRIx64
			", expected virtual TSC 0x%016" PRIx64 "\n", tsc, (uint64_t)TSC_BASE);
		goto out;
	}

	/* determinism: a second run reproduces every byte. */
	if (run_once(fd, mem, b) < 0) goto out;
	if (memcmp(a, b, 64) != 0) {
		fprintf(stderr, "hvx_cpuid_test: FAIL: run B differs from run A (non-deterministic!)\n");
		goto out;
	}
	printf("hvx_cpuid_test: determinism: run B == run A (64 bytes identical)\n");

	printf("hvx_cpuid_test: MILESTONE 12 PASS: CPUID + RDMSR/WRMSR emulated "
	       "in-kernel with fixed synthetic values; two runs bit-identical\n");
	rc = 0;

out:
	close(fd);
	return rc;
}

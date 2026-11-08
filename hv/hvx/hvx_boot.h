/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hvx_boot.h - shared Linux-boot loader setup for the hvx userspace clients.
 *
 * Milestone 16 factored the ~500 lines of boot-environment construction out of
 * hvx_linux.c so a second client (hvx_boot_verify.c, the whole-boot determinism
 * harness) can reuse EXACTLY the same recipe without duplicating it. Everything
 * here is the byte-for-byte port from src/boot.rs + src/detlinux.rs that used to
 * live in hvx_linux.c; the only change is that the functions are `static inline`
 * so either translation unit may include this header and leave unused helpers
 * out with no warning.
 *
 * A client uses it like this:
 *   fd = open("/dev/hvx"); ioctl(HVX_CREATE_VM); mem = mmap(...);
 *   build_page_tables_and_gdt(mem, mem_size);
 *   load_bzimage(mem, mem_size, image, image_len, initrd, initrd_len, cmdline);
 *   fill_sregs(&sregs); ioctl(HVX_SET_SREGS, &sregs);
 *   regs.rip=KERNEL_LOAD_ADDR+KERNEL_ENTRY_OFF; regs.rsi=BOOT_PARAMS_ADDR; ...
 *   ioctl(HVX_SET_REGS, &regs); then loop HVX_RUN.
 */
#ifndef _HVX_BOOT_H
#define _HVX_BOOT_H

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
#include <sys/stat.h>

#include "hvx_abi.h"

/* ==========================================================================
 * Guest-physical memory map (ported from boot.rs + detlinux.rs). Because hvx
 * runs the guest with guest-virtual == guest-physical == the offset into the
 * mmap()'d region, every GPA below is also the byte offset into `mem`.
 * ========================================================================== */

/* --- long-mode paging + descriptor tables (detlinux.rs:207-210) --- */
#define PML4_ADDR   0x1000ULL
#define PDPT_ADDR   0x2000ULL
#define PD_ADDR     0x3000ULL
#define GDT_ADDR    0x4000ULL
#define STACK_TOP   0x8000ULL   /* detlinux.rs:248  regs.rsp = 0x8000 */

/* Page-table entry bits (detlinux.rs:211-213). */
#define PTE_PRESENT (1ULL << 0)
#define PTE_RW      (1ULL << 1)
#define PTE_PS      (1ULL << 7)   /* 2 MiB large page in a PD entry */

/* Control-register / EFER bits (detlinux.rs:214-218). */
#define CR0_PE      (1ULL << 0)
#define CR0_NE      (1ULL << 5)   /* required=1 for VMX guest CR0 */
#define CR0_PG      (1ULL << 31)
#define CR4_PAE     (1ULL << 5)
#define EFER_LME    (1ULL << 8)
#define EFER_LMA    (1ULL << 10)

/* --- Linux boot-protocol addresses (boot.rs:17-26) --- */
#define BOOT_PARAMS_ADDR  0x10000ULL   /* boot.rs:17 */
#define CMDLINE_ADDR      0x20000ULL   /* boot.rs:18 */
#define KERNEL_LOAD_ADDR  0x100000ULL  /* boot.rs:19 */
#define KERNEL_ENTRY_OFF  0x200ULL     /* boot.rs:21 */
#define MPF_ADDR          0x9FC00ULL   /* boot.rs:25 */
#define MPC_ADDR          0x9FC10ULL   /* boot.rs:26 */

/* Conventional-memory top / hi-mem start for e820 (boot.rs:35-36). */
#define EBDA_START        0x9FC00ULL
#define HIMEM_START       0x100000ULL
/* Guest RAM above RAM_LOW_MAX is at RAM_HIGH_BASE (milestone 179; hvx_abi.h's HVX_RAM_LOW_MAX
 * and HVX_RAM_HIGH_BASE, repeated here because this header does not include hvx_abi.h). */
#define RAM_LOW_MAX       0xc0000000ULL
#define RAM_HIGH_BASE     0x100000000ULL

/* e820 memory types (boot.rs:29). */
#define E820_RAM          1u

/* setup_header field offsets inside boot_params (boot.rs:38-48). */
#define OFF_SETUP_SECTS    0x1f1
#define OFF_TYPE_OF_LOADER 0x210
#define OFF_LOADFLAGS      0x211
#define OFF_RAMDISK_IMAGE  0x218
#define OFF_RAMDISK_SIZE   0x21c
#define OFF_HEAP_END_PTR   0x224
#define OFF_CMD_LINE_PTR   0x228
#define OFF_CMDLINE_SIZE   0x238
#define OFF_E820_ENTRIES   0x1e8
#define OFF_E820_TABLE     0x2d0

/* The setup header we copy verbatim from the image spans [0x1f1, 0x268). */
#define HDR_COPY_START     0x1f1
#define HDR_COPY_END       0x268

/* loadflags bits (boot.rs:55-58). */
#define LOADED_HIGH   (1u << 0)
#define QUIET_FLAG    (1u << 5)
#define KEEP_SEGMENTS (1u << 6)
#define CAN_USE_HEAP  (1u << 7)

/* Default kernel command line (matches `hydo boot-linux`, src/main.rs:387). */
#define DEFAULT_CMDLINE "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

/* Default guest RAM: 256 MiB (matches hydo's --mem default, src/main.rs). */
#define DEFAULT_MEM_MIB   256UL

/* 1 GiB: the bootloader identity-map limit the initramfs must fit under. */
#define IDENTITY_MAP_CAP  0x40000000ULL

/* --- selected VMX basic exit reasons for stop decisions + pretty-printing. --- */
#define EXIT_EXC_NMI       0
#define EXIT_EXTERNAL_INT  1
#define EXIT_TRIPLE_FAULT  2
#define EXIT_INTR_WINDOW   7
#define EXIT_CPUID         10
#define EXIT_HLT           12
#define EXIT_RDTSC         16
#define EXIT_CR_ACCESS     28
#define EXIT_IO_INSN       30
#define EXIT_RDMSR         31
#define EXIT_WRMSR         32
#define EXIT_ENTRY_INVAL   33
#define EXIT_MTF           37   /* == HVX_EXIT_STEP */
#define EXIT_EPT_VIOLATION 48
#define EXIT_EPT_MISCONFIG 49
#define EXIT_RDTSCP        51

/* ==========================================================================
 * Small helpers.
 * ========================================================================== */

/* Little-endian stores into a byte buffer (the zero page / MP table). */
static inline void wr16(uint8_t *b, size_t off, uint16_t v)
{
	b[off]     = (uint8_t)(v & 0xff);
	b[off + 1] = (uint8_t)((v >> 8) & 0xff);
}
static inline void wr32(uint8_t *b, size_t off, uint32_t v)
{
	b[off]     = (uint8_t)(v & 0xff);
	b[off + 1] = (uint8_t)((v >> 8) & 0xff);
	b[off + 2] = (uint8_t)((v >> 16) & 0xff);
	b[off + 3] = (uint8_t)((v >> 24) & 0xff);
}
static inline void wr64(uint8_t *b, size_t off, uint64_t v)
{
	wr32(b, off, (uint32_t)(v & 0xffffffff));
	wr32(b, off + 4, (uint32_t)(v >> 32));
}
static inline uint16_t rd16(const uint8_t *b, size_t off)
{
	return (uint16_t)(b[off] | ((uint16_t)b[off + 1] << 8));
}

/* Store a u64 directly into guest RAM at a GPA (guest-phys == mmap offset). */
static inline void guest_wr64(uint8_t *mem, uint64_t mem_size, uint64_t gpa, uint64_t v)
{
	if (gpa + 8 > mem_size) {
		fprintf(stderr, "hvx_boot: BUG: guest_wr64 GPA %#" PRIx64
			" out of RAM (%#" PRIx64 ")\n", gpa, mem_size);
		exit(1);
	}
	wr64(mem + gpa, 0, v);
}

/* Copy a blob into guest RAM at a GPA, bounds-checked. */
static inline int guest_write(uint8_t *mem, uint64_t mem_size, uint64_t gpa,
			      const void *src, size_t len)
{
	if (gpa + len > mem_size) {
		fprintf(stderr, "hvx_boot: guest_write %#zx bytes @ GPA %#" PRIx64
			" exceeds guest RAM (%#" PRIx64 ")\n", len, gpa, mem_size);
		return -1;
	}
	memcpy(mem + gpa, src, len);
	return 0;
}

/* Two's-complement checksum byte so a structure sums to zero mod 256. */
static inline uint8_t checksum(const uint8_t *bytes, size_t n)
{
	uint8_t sum = 0;
	size_t i;
	for (i = 0; i < n; i++)
		sum = (uint8_t)(sum + bytes[i]);
	return (uint8_t)(0u - sum);
}

/* Read an entire file into a malloc'd buffer, robust to short reads. */
static inline uint8_t *read_file(const char *path, size_t *out_len)
{
	int fd;
	struct stat st;
	uint8_t *buf;
	size_t got = 0;

	fd = open(path, O_RDONLY);
	if (fd < 0) {
		fprintf(stderr, "hvx_boot: open %s: %s\n", path, strerror(errno));
		return NULL;
	}
	if (fstat(fd, &st) < 0 || st.st_size <= 0) {
		fprintf(stderr, "hvx_boot: stat %s: %s\n", path, strerror(errno));
		close(fd);
		return NULL;
	}
	buf = malloc((size_t)st.st_size);
	if (!buf) {
		fprintf(stderr, "hvx_boot: malloc(%lld) failed\n", (long long)st.st_size);
		close(fd);
		return NULL;
	}
	while (got < (size_t)st.st_size) {
		ssize_t n = read(fd, buf + got, (size_t)st.st_size - got);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_boot: read %s: %s\n", path, strerror(errno));
			free(buf);
			close(fd);
			return NULL;
		}
		if (n == 0)
			break; /* short file - use what we got */
		got += (size_t)n;
	}
	close(fd);
	*out_len = got;
	return buf;
}

/* Human-readable VMX exit reason (plus the hvx sentinels). */
static inline const char *exit_reason_name(uint32_t r)
{
	switch (r) {
	case EXIT_EXC_NMI:        return "exception/NMI";
	case EXIT_EXTERNAL_INT:   return "external interrupt";
	case EXIT_TRIPLE_FAULT:   return "TRIPLE FAULT";
	case EXIT_INTR_WINDOW:    return "interrupt window";
	case EXIT_CPUID:          return "CPUID";
	case EXIT_HLT:            return "HLT";
	case EXIT_RDTSC:          return "RDTSC";
	case EXIT_CR_ACCESS:      return "control-register access";
	case EXIT_IO_INSN:        return "I/O instruction";
	case EXIT_RDMSR:          return "RDMSR";
	case EXIT_WRMSR:          return "WRMSR";
	case EXIT_ENTRY_INVAL:    return "VM-entry failed: invalid guest state";
	case EXIT_MTF:            return "monitor trap flag (single-step)";
	case EXIT_EPT_VIOLATION:  return "EPT violation";
	case EXIT_EPT_MISCONFIG:  return "EPT misconfiguration";
	case EXIT_RDTSCP:         return "RDTSCP";
	case HVX_EXIT_VMFAIL:     return "VMFAIL (VMLAUNCH/VMRESUME failed)";
	case HVX_EXIT_THROTTLE:   return "THROTTLE (run-loop bound; re-run)";
	default:                  return "other";
	}
}

/* ==========================================================================
 * Build the 64-bit boot environment inside guest RAM.
 * ========================================================================== */

/* Page tables + GDT, byte-for-byte from detlinux.rs:220-227. */
static inline void build_page_tables_and_gdt(uint8_t *mem, uint64_t mem_size)
{
	uint64_t i;

	guest_wr64(mem, mem_size, PML4_ADDR, PDPT_ADDR | PTE_PRESENT | PTE_RW);
	guest_wr64(mem, mem_size, PDPT_ADDR, PD_ADDR | PTE_PRESENT | PTE_RW);
	for (i = 0; i < 512; i++)
		guest_wr64(mem, mem_size, PD_ADDR + i * 8,
			   (i * 0x200000ULL) | PTE_PRESENT | PTE_RW | PTE_PS);

	guest_wr64(mem, mem_size, GDT_ADDR + 0,  0x0000000000000000ULL);
	guest_wr64(mem, mem_size, GDT_ADDR + 8,  0x00AF9A000000FFFFULL); /* code64 */
	guest_wr64(mem, mem_size, GDT_ADDR + 16, 0x00CF92000000FFFFULL); /* data   */
}

/* Intel MP Floating Pointer + MP Configuration Table (boot.rs:186-276). With ncpus > 1
 * (SMP Stage 2) it lists one processor entry per vCPU, APIC ID = vCPU index, CPU 0 the
 * boot CPU; with ncpus == 1 the bytes are exactly the one-CPU table of every earlier boot. */
static inline void build_mptable_n(uint8_t *mem, uint64_t mem_size, unsigned int ncpus)
{
	const uint32_t APIC_DEFAULT_PHYS_BASE   = 0xfee00000u;
	const uint32_t IOAPIC_DEFAULT_PHYS_BASE = 0xfec00000u;
	const uint8_t  APIC_VERSION             = 0x14;
	const uint32_t CPU_STEPPING             = 0x600;
	const uint32_t CPU_FEATURE_APIC         = 0x200;
	const uint32_t CPU_FEATURE_FPU          = 0x001;

	uint8_t mpc[512];
	unsigned int cpu;
	size_t p = 0;
	uint16_t entry_count = 0;
	uint16_t len;
	uint8_t mpf[16];
	uint8_t irq;

	memset(mpc, 0, sizeof(mpc));

	memcpy(mpc + p, "PCMP", 4); p += 4;      /* signature */
	wr16(mpc, p, 0); p += 2;                 /* length (patched) - offset 0x04 */
	mpc[p++] = 4;                            /* spec revision 1.4 */
	mpc[p++] = 0;                            /* checksum (patched) - offset 0x07 */
	memcpy(mpc + p, "HYDO    ", 8); p += 8;
	memcpy(mpc + p, "MPTB        ", 12); p += 12;
	wr32(mpc, p, 0); p += 4;                 /* oem table ptr */
	wr16(mpc, p, 0); p += 2;                 /* oem table size */
	wr16(mpc, p, 0); p += 2;                 /* entry count (patched) - offset 0x22 */
	wr32(mpc, p, APIC_DEFAULT_PHYS_BASE); p += 4; /* LAPIC address */
	wr16(mpc, p, 0); p += 2;                 /* extended table length */
	mpc[p++] = 0;                            /* extended table checksum */
	mpc[p++] = 0;                            /* reserved */
	/* p == 44 (0x2c) here. */

	for (cpu = 0; cpu < (ncpus ? ncpus : 1); cpu++) {
		mpc[p++] = 0;                    /* entry type: processor */
		mpc[p++] = (uint8_t)cpu;         /* local APIC id */
		mpc[p++] = APIC_VERSION;         /* local APIC version */
		mpc[p++] = cpu ? 0x01 : 0x03;    /* flags: enabled (| bootstrap for CPU 0) */
		wr32(mpc, p, CPU_STEPPING); p += 4; /* cpu signature */
		wr32(mpc, p, CPU_FEATURE_APIC | CPU_FEATURE_FPU); p += 4; /* features */
		memset(mpc + p, 0, 8); p += 8;   /* reserved */
		entry_count++;
	}

	mpc[p++] = 1;
	mpc[p++] = 0;                            /* bus id */
	memcpy(mpc + p, "ISA   ", 6); p += 6;    /* bus type string */
	entry_count++;

	mpc[p++] = 2;
	mpc[p++] = 0;                            /* ioapic id */
	mpc[p++] = APIC_VERSION;
	mpc[p++] = 1;                            /* flags: enabled */
	wr32(mpc, p, IOAPIC_DEFAULT_PHYS_BASE); p += 4;
	entry_count++;

	for (irq = 0; irq < 16; irq++) {
		mpc[p++] = 3;
		mpc[p++] = 0;                    /* interrupt type: INT */
		wr16(mpc, p, 0); p += 2;         /* flags (conforming) */
		mpc[p++] = 0;                    /* source bus id (ISA) */
		mpc[p++] = irq;                  /* source bus irq */
		mpc[p++] = 0;                    /* dest IOAPIC id */
		mpc[p++] = irq;                  /* dest IOAPIC INTIN */
		entry_count++;
	}

	len = (uint16_t)p;
	wr16(mpc, 0x04, len);
	wr16(mpc, 0x22, entry_count);
	mpc[0x07] = checksum(mpc, p);

	if (guest_write(mem, mem_size, MPC_ADDR, mpc, p) < 0)
		exit(1);

	memset(mpf, 0, sizeof(mpf));
	memcpy(mpf, "_MP_", 4);
	wr32(mpf, 4, (uint32_t)MPC_ADDR);        /* physical addr of MPC */
	mpf[8]  = 1;                             /* length in 16-byte units */
	mpf[9]  = 4;                             /* spec revision 1.4 */
	mpf[10] = 0;                             /* checksum (patched) */
	mpf[10] = checksum(mpf, 16);

	if (guest_write(mem, mem_size, MPF_ADDR, mpf, 16) < 0)
		exit(1);
}

static inline void build_mptable(uint8_t *mem, uint64_t mem_size)
{
	build_mptable_n(mem, mem_size, 1);
}

/* Load the bzImage + initramfs into guest RAM and build the zero page / e820 /
 * cmdline / MP table. Ported from boot.rs:80-180 (load_linux). */
static inline int load_bzimage_n(uint8_t *mem, uint64_t mem_size,
				 const uint8_t *image, size_t image_len,
				 const uint8_t *initrd, size_t initrd_len,
				 const char *cmdline, unsigned int ncpus)
{
	uint16_t boot_flag, xloadflags, version;
	size_t setup_sects, setup_size, kernel_len;
	uint64_t kend;
	uint8_t bp[4096];   /* the boot_params zero page (boot.rs:118) */
	uint8_t lf;
	size_t cmd_len;

	if (image_len < 0x1000) {
		fprintf(stderr, "hvx_boot: kernel image too small\n");
		return -1;
	}
	boot_flag = rd16(image, 0x1fe);
	if (boot_flag != 0xAA55 || memcmp(image + 0x202, "HdrS", 4) != 0) {
		fprintf(stderr, "hvx_boot: not a bzImage (bad boot_flag/HdrS magic)\n");
		return -1;
	}
	version    = rd16(image, 0x206);
	xloadflags = rd16(image, 0x236);
	if ((xloadflags & 0x1) == 0) {                  /* XLF_KERNEL_64 */
		fprintf(stderr, "hvx_boot: kernel lacks a 64-bit entry point "
			"(XLF_KERNEL_64 clear)\n");
		return -1;
	}
	(void)version;

	setup_sects = (image[OFF_SETUP_SECTS] == 0) ? 4u : image[OFF_SETUP_SECTS];
	setup_size  = (setup_sects + 1) * 512;
	if (setup_size >= image_len) {
		fprintf(stderr, "hvx_boot: bzImage smaller than its declared setup area\n");
		return -1;
	}

	kernel_len = image_len - setup_size;
	kend = KERNEL_LOAD_ADDR + kernel_len;
	if (kend >= mem_size) {
		fprintf(stderr, "hvx_boot: guest RAM too small for kernel image "
			"(need > %#" PRIx64 ", have %#" PRIx64 ")\n", kend, mem_size);
		return -1;
	}
	if (guest_write(mem, mem_size, KERNEL_LOAD_ADDR, image + setup_size, kernel_len) < 0)
		return -1;

	memset(bp, 0, sizeof(bp));
	memcpy(bp + HDR_COPY_START, image + HDR_COPY_START, HDR_COPY_END - HDR_COPY_START);

	bp[OFF_TYPE_OF_LOADER] = 0xff;
	lf  = bp[OFF_LOADFLAGS];
	lf |= (LOADED_HIGH | CAN_USE_HEAP);
	lf &= (uint8_t)~(QUIET_FLAG | KEEP_SEGMENTS);
	bp[OFF_LOADFLAGS] = lf;
	wr16(bp, OFF_HEAP_END_PTR, 0xfe00);

	cmd_len = strlen(cmdline);
	if (guest_write(mem, mem_size, CMDLINE_ADDR, cmdline, cmd_len + 1) < 0)
		return -1;
	wr32(bp, OFF_CMD_LINE_PTR, (uint32_t)CMDLINE_ADDR);
	wr32(bp, OFF_CMDLINE_SIZE, (uint32_t)cmd_len);

	if (initrd && initrd_len > 0) {
		uint64_t cap  = (mem_size < IDENTITY_MAP_CAP) ? mem_size : IDENTITY_MAP_CAP;
		uint64_t addr;
		if ((uint64_t)initrd_len + 0x100000ULL >= cap) {
			fprintf(stderr, "hvx_boot: guest RAM too small for initramfs\n");
			return -1;
		}
		addr = (cap - initrd_len) & ~0xfffULL;
		if (guest_write(mem, mem_size, addr, initrd, initrd_len) < 0)
			return -1;
		wr32(bp, OFF_RAMDISK_IMAGE, (uint32_t)addr);
		wr32(bp, OFF_RAMDISK_SIZE, (uint32_t)initrd_len);
	}

	{
		struct { uint64_t addr, size; uint32_t type; } e820[3];
		size_t n = 0;
		size_t i;
		/* M179: above RAM_LOW_MAX the rest of RAM sits at RAM_HIGH_BASE (hvx_abi.h) */
		uint64_t low = mem_size < RAM_LOW_MAX ? mem_size : RAM_LOW_MAX;
		e820[n].addr = 0; e820[n].size = EBDA_START; e820[n].type = E820_RAM; n++;
		if (low > HIMEM_START) {
			e820[n].addr = HIMEM_START;
			e820[n].size = low - HIMEM_START;
			e820[n].type = E820_RAM;
			n++;
		}
		if (mem_size > low) {
			e820[n].addr = RAM_HIGH_BASE;
			e820[n].size = mem_size - low;
			e820[n].type = E820_RAM;
			n++;
		}
		bp[OFF_E820_ENTRIES] = (uint8_t)n;
		for (i = 0; i < n; i++) {
			size_t base = OFF_E820_TABLE + i * 20;
			wr64(bp, base + 0, e820[i].addr);
			wr64(bp, base + 8, e820[i].size);
			wr32(bp, base + 16, e820[i].type);
		}
	}

	if (guest_write(mem, mem_size, BOOT_PARAMS_ADDR, bp, sizeof(bp)) < 0)
		return -1;

	build_mptable_n(mem, mem_size, ncpus);

	fprintf(stderr, "hvx_boot: bzImage v%u.%u  setup=%zuB  kernel=%zuB @ %#llx"
		"  initramfs=%zuB  cmdline=\"%s\"\n",
		version >> 8, version & 0xff, setup_size, kernel_len,
		(unsigned long long)KERNEL_LOAD_ADDR, initrd_len, cmdline);
	return 0;
}

static inline int load_bzimage(uint8_t *mem, uint64_t mem_size,
			       const uint8_t *image, size_t image_len,
			       const uint8_t *initrd, size_t initrd_len,
			       const char *cmdline)
{
	return load_bzimage_n(mem, mem_size, image, image_len, initrd, initrd_len, cmdline, 1);
}

/* ==========================================================================
 * Initial CPU state (ported from detlinux.rs:206-243).
 * ========================================================================== */
static inline void fill_sregs(struct hvx_sregs *s)
{
	memset(s, 0, sizeof(*s));

	s->cr3  = PML4_ADDR;
	s->cr4  = CR4_PAE;
	s->cr0  = CR0_PE | CR0_NE | CR0_PG;
	s->efer = EFER_LME | EFER_LMA;
	s->dr7  = 0x400;

	s->cs.selector = 0x08;
	s->cs.base     = 0;
	s->cs.limit    = 0xffffffff;
	s->cs.ar       = 0xa09b;

	s->ds.selector = 0x10; s->ds.base = 0; s->ds.limit = 0xffffffff; s->ds.ar = 0xc093;
	s->es = s->ds;
	s->fs = s->ds;
	s->gs = s->ds;
	s->ss = s->ds;

	s->tr.selector = 0x18;
	s->tr.base     = 0;
	s->tr.limit    = 0x67;
	s->tr.ar       = 0x8b;

	s->ldtr.selector = 0;
	s->ldtr.base     = 0;
	s->ldtr.limit    = 0;
	/* The VMCS "unusable" bit (16) does not fit hvx_segment.ar (__u16); leave it
	 * 0 -- setup_vmcs forces GUEST_LDTR_AR = 0x10000 (unusable) regardless, and
	 * save_sregs reads it back truncated to 0, so this is what the field holds. */
	s->ldtr.ar       = 0;

	s->gdt_base  = GDT_ADDR;
	s->gdt_limit = 3 * 8 - 1;   /* 23 */

	s->idt_base  = 0;
	s->idt_limit = 0;
}

/* ==========================================================================
 * Console draining (hvx_linux.c's interactive printer).
 * ========================================================================== */
static inline void drain_console(int fd, size_t *shown)
{
	struct hvx_console con;
	size_t from, to;

	if (ioctl(fd, HVX_GET_CONSOLE, &con) < 0)
		return; /* console is best-effort; ignore errors */

	to = con.len;
	if (to > HVX_CONSOLE_SZ)
		to = HVX_CONSOLE_SZ;
	from = *shown;
	if (from > HVX_CONSOLE_SZ)
		from = HVX_CONSOLE_SZ;
	if (to > from) {
		fwrite(con.buf + from, 1, to - from, stdout);
		fflush(stdout);
	}
	*shown = con.len;
}

/* Dump the bytes of the faulting instruction. The decompressed x86-64 kernel
 * maps its text at __START_KERNEL_map + phys (0xffffffff80000000 + phys). */
static inline void dump_fault_insn(uint8_t *mem, uint64_t mem_size, unsigned long long rip)
{
	const unsigned long long KBASE = 0xffffffff80000000ULL;
	unsigned long long phys;
	int i;

	if (rip < KBASE)
		return;
	phys = rip - KBASE;
	if (phys + 16 > mem_size) {
		fprintf(stderr, "  (fault phys %#llx beyond guest RAM)\n", phys);
		return;
	}
	fprintf(stderr, "  fault insn @ phys %#llx:", phys);
	for (i = 0; i < 16; i++)
		fprintf(stderr, " %02x", mem[phys + i]);
	fprintf(stderr, "\n");
}

#endif /* _HVX_BOOT_H */

// hydo Milestone 97: syzkaller executor extension for /dev/hvx (the hvx kernel module).
// build-syzkaller.sh copies this file over syzkaller's executor/common_ext.h, the
// documented place for out-of-tree pseudo-syscalls (names must start with syz_ext_).
//
// syz_ext_hvx_guest(fd, flags, ops, nops, mem, nmem) writes a small guest program into the
// guest RAM of an hvx VM (through the fd's mmap window) so that the next HVX_RUN executes
// it. The program is assembled from structured operations -- port I/O, MMIO, MSRs, CPUID,
// PCI config cycles, string I/O, control-register writes, single instructions and raw
// x86-64 bytes -- so the fuzzer reaches the module's exit handlers and device models
// (16550, IOAPIC, x2APIC, virtio-blk/net PCI and the PIO test transport) instead of only
// the ioctl argument checks. `mem` holds extra guest-memory writes (virtqueue rings,
// descriptors, buffers). The guest runs in the module's default flat long mode, CPL0,
// identity paging of the low 512 MiB, code at GPA 0x3000 (where the module points RIP).
//
// Every op is 32 bytes: { u64 kind, a, b, c }. Unknown kinds are skipped.

#if SYZ_EXECUTOR || __NR_syz_ext_hvx_guest
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>

#define HVX_SYZ_CODE 0x3000
#define HVX_SYZ_CODE_MAX 0x2000 // on the stack: the executor builds with -Wframe-larger-than=16384
#define HVX_SYZ_F_IDMAP_MMIO 1 // map the 4th GiB (MMIO window) in the guest page tables
#define HVX_SYZ_F_NO_HLT 2 // do not end the program with HLT (let it run off)

struct hvx_syz_op {
	uint64_t kind, a, b, c;
};
struct hvx_syz_mem {
	uint64_t gpa, len, data;
};

struct hvx_syz_asm {
	uint8_t* p;
	uint8_t* end;
};

static void hvx_syz_emit(struct hvx_syz_asm* s, const void* b, size_t n)
{
	if (s->p + n > s->end)
		return;
	memcpy(s->p, b, n);
	s->p += n;
}
static void hvx_syz_b(struct hvx_syz_asm* s, uint8_t b) { hvx_syz_emit(s, &b, 1); }
static void hvx_syz_op_imm32(struct hvx_syz_asm* s, uint8_t op, uint32_t v)
{
	uint8_t b[5] = {op, (uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16), (uint8_t)(v >> 24)};
	hvx_syz_emit(s, b, 5);
}
// movabs r64, imm64 (REX.W B8+r)
static void hvx_syz_movabs(struct hvx_syz_asm* s, int reg, uint64_t v)
{
	uint8_t b[10];
	b[0] = 0x48;
	b[1] = 0xb8 + reg;
	memcpy(b + 2, &v, 8);
	hvx_syz_emit(s, b, 10);
}
#define HVX_RAX 0
#define HVX_RCX 1
#define HVX_RDX 2
#define HVX_RBX 3
#define HVX_RSI 6

static void hvx_syz_port_io(struct hvx_syz_asm* s, uint32_t port, uint64_t val, uint64_t size, int in)
{
	hvx_syz_op_imm32(s, 0xba, port & 0xffff); // mov edx, port
	if (!in)
		hvx_syz_op_imm32(s, 0xb8, (uint32_t)val); // mov eax, val
	switch (size) {
	case 1:
		hvx_syz_b(s, in ? 0xec : 0xee);
		break;
	case 2:
		hvx_syz_b(s, 0x66);
		hvx_syz_b(s, in ? 0xed : 0xef);
		break;
	default:
		hvx_syz_b(s, in ? 0xed : 0xef);
		break;
	}
}

static void hvx_syz_assemble(struct hvx_syz_asm* s, const struct hvx_syz_op* op)
{
	uint64_t kind = op->kind & 0xff;
	uint64_t a = op->a, b = op->b, c = op->c;

	switch (kind) {
	case 0: // OUT port=a val=b size=c
	case 1: // IN  port=a size=c
		hvx_syz_port_io(s, (uint32_t)a, b, c, kind == 1);
		break;
	case 2: // MMIO write: addr = lo32(a) + hi32(a), val=b, size=c
	case 3: { // MMIO read
		uint64_t addr = (a & 0xffffffffull) + (a >> 32);
		hvx_syz_movabs(s, HVX_RBX, addr);
		if (kind == 2)
			hvx_syz_movabs(s, HVX_RAX, b);
		switch (c) {
		case 1:
			if (kind == 2) {
				hvx_syz_b(s, 0x88); // mov [rbx], al
				hvx_syz_b(s, 0x03);
			} else {
				hvx_syz_b(s, 0x0f); // movzx eax, byte [rbx]
				hvx_syz_b(s, 0xb6);
				hvx_syz_b(s, 0x03);
			}
			break;
		case 2:
			hvx_syz_b(s, 0x66);
			hvx_syz_b(s, kind == 2 ? 0x89 : 0x8b);
			hvx_syz_b(s, 0x03);
			break;
		case 8:
			hvx_syz_b(s, 0x48);
			hvx_syz_b(s, kind == 2 ? 0x89 : 0x8b);
			hvx_syz_b(s, 0x03);
			break;
		default:
			hvx_syz_b(s, kind == 2 ? 0x89 : 0x8b);
			hvx_syz_b(s, 0x03);
			break;
		}
		break;
	}
	case 4: // WRMSR msr=a val=b
		hvx_syz_op_imm32(s, 0xb9, (uint32_t)a);
		hvx_syz_op_imm32(s, 0xb8, (uint32_t)b);
		hvx_syz_op_imm32(s, 0xba, (uint32_t)(b >> 32));
		hvx_syz_b(s, 0x0f);
		hvx_syz_b(s, 0x30);
		break;
	case 5: // RDMSR msr=a
		hvx_syz_op_imm32(s, 0xb9, (uint32_t)a);
		hvx_syz_b(s, 0x0f);
		hvx_syz_b(s, 0x32);
		break;
	case 6: // CPUID leaf=a sub=b
		hvx_syz_op_imm32(s, 0xb8, (uint32_t)a);
		hvx_syz_op_imm32(s, 0xb9, (uint32_t)b);
		hvx_syz_b(s, 0x0f);
		hvx_syz_b(s, 0xa2);
		break;
	case 7: { // single instruction a, argument b
		static const uint8_t insn[][4] = {
		    {1, 0xf4}, // 0 hlt
		    {2, 0x0f, 0x31}, // 1 rdtsc
		    {3, 0x0f, 0x01, 0xf9}, // 2 rdtscp
		    {2, 0xf3, 0x90}, // 3 pause
		    {1, 0xfb}, // 4 sti
		    {1, 0xfa}, // 5 cli
		    {3, 0x0f, 0x01, 0xc1}, // 6 vmcall
		    {2, 0x0f, 0x0b}, // 7 ud2
		    {1, 0xcc}, // 8 int3
		    {1, 0x90}, // 9 nop
		    {2, 0x0f, 0x09}, // 10 wbinvd
		    {2, 0x0f, 0x08}, // 11 invd
		    {2, 0x0f, 0x33}, // 12 rdpmc (ecx = b)
		    {3, 0x0f, 0x01, 0xd1}, // 13 xsetbv (ecx = b>>32, edx:eax = b)
		    {3, 0x0f, 0x01, 0xc8}, // 14 monitor
		    {3, 0x0f, 0x01, 0xc9}, // 15 mwait
		    {3, 0x0f, 0xc7, 0xf0}, // 16 rdrand eax
		    {2, 0xfb, 0xf4}, // 17 sti; hlt
		    {2, 0xcd, 0x00}, // 18 int imm8 (patched with b)
		    {3, 0x0f, 0x01, 0xd0}, // 19 xgetbv
		    {3, 0x0f, 0xc7, 0xf8}, // 20 rdseed eax
		    {3, 0x0f, 0x01, 0xca}, // 21 clac
		    {3, 0x0f, 0x01, 0xf8}, // 22 swapgs
		    {2, 0x0f, 0x05}, // 23 syscall
		    {2, 0x48, 0xcf}, // 24 iretq
		};
		uint64_t i = a % (sizeof(insn) / sizeof(insn[0]));
		uint8_t buf[4];
		if (i == 12 || i == 13) {
			hvx_syz_op_imm32(s, 0xb9, (uint32_t)(i == 12 ? b : b >> 32));
			hvx_syz_op_imm32(s, 0xb8, (uint32_t)b);
			hvx_syz_op_imm32(s, 0xba, 0);
		}
		memcpy(buf, insn[i] + 1, 3);
		if (i == 18)
			buf[1] = (uint8_t)b;
		hvx_syz_emit(s, buf, insn[i][0]);
		break;
	}
	case 8: { // write a control register: a = 0 cr0, 1 cr3, 2 cr4, 3 cr8, 4 dr7; value b
		static const uint8_t modrm[] = {0xc0, 0xd8, 0xe0, 0xc0, 0xf8};
		uint64_t i = a % 5;
		hvx_syz_movabs(s, HVX_RAX, b);
		if (i == 3)
			hvx_syz_b(s, 0x44); // REX.R: cr8
		hvx_syz_b(s, 0x0f);
		hvx_syz_b(s, i == 4 ? 0x23 : 0x22);
		hvx_syz_b(s, modrm[i]);
		break;
	}
	case 9: // raw bytes: a = pointer, b = length
		if (a && b)
			hvx_syz_emit(s, (const void*)(uintptr_t)a, b > 128 ? 128 : b);
		break;
	case 10: { // PCI config cycle: a = config address (bus/dev/fn/reg), b = value, c = size | is_read << 8
		uint32_t addr = 0x80000000u | ((uint32_t)a & 0x00fffffcu);
		uint64_t size = c & 0xff;
		hvx_syz_port_io(s, 0xcf8, addr, 4, 0);
		hvx_syz_port_io(s, 0xcfc + ((uint32_t)a & 3), b, size, (c >> 8) & 1);
		break;
	}
	case 11: { // string I/O: port = a, rsi/rdi = gpa b, rcx = count c (rep when c != 1)
		hvx_syz_op_imm32(s, 0xba, (uint32_t)a & 0xffff);
		hvx_syz_movabs(s, HVX_RSI, b);
		hvx_syz_b(s, 0x48); // mov rdi, rsi
		hvx_syz_b(s, 0x89);
		hvx_syz_b(s, 0xf7);
		hvx_syz_movabs(s, HVX_RCX, c);
		if (c != 1)
			hvx_syz_b(s, 0xf3);
		hvx_syz_b(s, (a >> 32) & 1 ? 0x6c : 0x6e); // insb / outsb
		break;
	}
	}
}

static long syz_ext_hvx_guest(volatile long a0, volatile long a1, volatile long a2, volatile long a3,
			      volatile long a4, volatile long a5)
{
	int fd = (int)a0;
	uint64_t flags = (uint64_t)a1;
	const struct hvx_syz_op* ops = (const struct hvx_syz_op*)a2;
	uint64_t nops = (uint64_t)a3;
	const struct hvx_syz_mem* mem = (const struct hvx_syz_mem*)a4;
	uint64_t nmem = (uint64_t)a5;
	static const size_t lens[] = {1 << 20, 256 << 10, 64 << 10, 32 << 10};
	uint8_t code[HVX_SYZ_CODE_MAX];
	struct hvx_syz_asm s = {code, code + sizeof(code) - 3};
	uint8_t* ram = (uint8_t*)MAP_FAILED;
	size_t ramlen = 0;
	uint64_t i;

	// Map as much guest RAM as exists, from 1 MiB down to the module's 32 KiB minimum.
	for (i = 0; i < sizeof(lens) / sizeof(lens[0]); i++) {
		ram = (uint8_t*)mmap(NULL, lens[i], PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (ram != (uint8_t*)MAP_FAILED) {
			ramlen = lens[i];
			break;
		}
	}
	if (ram == (uint8_t*)MAP_FAILED)
		return -1;
	for (i = 0; i < nops && i < 64; i++)
		NONFAILING(hvx_syz_assemble(&s, &ops[i]));
	if (!(flags & HVX_SYZ_F_NO_HLT)) {
		s.end += 3;
		hvx_syz_b(&s, 0xf4); // hlt
		hvx_syz_b(&s, 0xeb); // jmp back to the hlt
		hvx_syz_b(&s, 0xfd);
	}
	memcpy(ram + HVX_SYZ_CODE, code, s.p - code);
	if (flags & HVX_SYZ_F_IDMAP_MMIO) {
		uint64_t pte = (3ull << 30) | 0x83 | 0x60; // 1 GiB identity of the 4th GiB (as HVX_SET_DISK does)
		memcpy(ram + 0x1000 + 3 * 8, &pte, 8);
	}
	for (i = 0; i < nmem && i < 16; i++) {
		uint64_t gpa = 0, len = 0, data = 0;
		NONFAILING(gpa = mem[i].gpa; len = mem[i].len; data = mem[i].data);
		if (!data || gpa >= ramlen || len > ramlen - gpa)
			continue;
		NONFAILING(memcpy(ram + gpa, (const void*)(uintptr_t)data, len));
	}
	munmap(ram, ramlen);
	return 0;
}
#endif

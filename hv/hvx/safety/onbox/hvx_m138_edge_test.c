// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_m138_edge_test - the edges of Milestone 138's module code, for a debug kernel to watch
 * (Milestone 140). syzkaller never saw the six new ioctls, and the explorer only takes the
 * port's normal path; this drives every refusal and boundary once, with a hand-assembled guest,
 * so KASAN, UBSAN, lockdep and kmemleak see them:
 *
 *   the host-call port: the magic, a record of 4+1 bytes, a commit whose length leaves 4 bytes
 *   over (an error), a record that overflows the 64 KiB buffer (an error), a 2-byte record;
 *   HVX_HC_GET in two pieces; HVX_HC_PUT of exactly the buffer and of one byte more (ENOSPC);
 *   the guest reading the count and 4+2+1 staged bytes; HVX_HC_GET_STATE with a short and a
 *   long buffer; HVX_HC_SET_STATE with each invalid length (EINVAL), an unmapped address
 *   (EFAULT) and a valid partial record, read back;
 *   the dirty-page log: HVX_GET_DIRTY with a short buffer (ENOSPC), NULL addresses, an unmapped
 *   address (EFAULT), the right buffers (the guest's two written pages are set), CLEAR;
 *   HVX_RESET_VM, then the whole pass again: every value must be the first pass's;
 *   refusals: each new ioctl with no VM and on a VM without the flags (EINVAL), the guest on a
 *   VM without the port (IN reads all ones); and 50 VM lifecycles with both flags and a disk.
 *
 *   cc -O2 -Wall -I../.. -o hvx_m138_edge_test hvx_m138_edge_test.c
 *   ~/bin/hvx-run sudo [HVX_DEV=/dev/hvxgc] ./hvx_m138_edge_test
 * Prints "hvx_m138_edge_test: PASS (<n> checks)" or the first failures; exit status 0 = pass.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "hvx_abi.h"

#define EXIT_HLT	12
#define MEM		(64UL * 4096)
#define DISK		(16UL * 4096)

static int checks, fails;
#define CHECK(c, ...) do { checks++; if (!(c)) { fails++; printf("  FAIL %s:%d: ", __func__, __LINE__); \
	printf(__VA_ARGS__); printf("\n"); } } while (0)

/* The guest (64-bit, flat identity paging of the module's default tables, code at 0x3000). */
static const unsigned char guest[] = {
	0xba, 0xf4, 0x05, 0x00, 0x00,		/* mov edx,0x5f4                  */
	0xed,					/* in eax,dx       magic          */
	0x89, 0xc3,				/* mov ebx,eax                    */
	0xba, 0xf0, 0x05, 0x00, 0x00,		/* mov edx,0x5f0                  */
	0xb8, 'H', 'E', 'L', 'L',		/* mov eax,"HELL"                 */
	0xef,					/* out dx,eax                     */
	0xb0, 'O',				/* mov al,'O'                     */
	0xee,					/* out dx,al                      */
	0xba, 0xf4, 0x05, 0x00, 0x00,		/* mov edx,0x5f4                  */
	0xb8, 0x05, 0x00, 0x00, 0x00,		/* mov eax,5                      */
	0xef,					/* out dx,eax      commit: exit 1 */
	0xba, 0xf0, 0x05, 0x00, 0x00,		/* mov edx,0x5f0                  */
	0xef, 0xef,				/* out dx,eax x2   8 bytes        */
	0xba, 0xf4, 0x05, 0x00, 0x00,		/* mov edx,0x5f4                  */
	0xb8, 0x04, 0x00, 0x00, 0x00,		/* mov eax,4                      */
	0xef,					/* out dx,eax      bad commit: 2  */
	0xba, 0xf0, 0x05, 0x00, 0x00,		/* mov edx,0x5f0                  */
	0xb9, 0x5c, 0x44, 0x00, 0x00,		/* mov ecx,17500   70000 bytes    */
	0xef,					/* L: out dx,eax                  */
	0xff, 0xc9,				/* dec ecx                        */
	0x75, 0xfb,				/* jnz L                          */
	0xba, 0xf4, 0x05, 0x00, 0x00,		/* mov edx,0x5f4                  */
	0xb8, 0x00, 0x00, 0x01, 0x00,		/* mov eax,0x10000                */
	0xef,					/* out dx,eax      overflow: 3    */
	0xba, 0xf0, 0x05, 0x00, 0x00,		/* mov edx,0x5f0                  */
	0xb8, 'A', 'B', 0x00, 0x00,		/* mov eax,"AB"                   */
	0x66, 0xef,				/* out dx,ax                      */
	0xba, 0xf4, 0x05, 0x00, 0x00,		/* mov edx,0x5f4                  */
	0xb8, 0x02, 0x00, 0x00, 0x00,		/* mov eax,2                      */
	0xef,					/* out dx,eax      commit: exit 4 */
	0xba, 0xf8, 0x05, 0x00, 0x00,		/* mov edx,0x5f8                  */
	0xed,					/* in eax,dx       staged count   */
	0x89, 0xc6,				/* mov esi,eax                    */
	0xba, 0xf0, 0x05, 0x00, 0x00,		/* mov edx,0x5f0                  */
	0xed,					/* in eax,dx       4 bytes        */
	0x89, 0xc7,				/* mov edi,eax                    */
	0x66, 0xed,				/* in ax,dx        2 bytes        */
	0x89, 0xc5,				/* mov ebp,eax                    */
	0xec,					/* in al,dx        1 byte         */
	0x41, 0x89, 0xc0,			/* mov r8d,eax                    */
	0xb8, 0xef, 0xbe, 0xad, 0xde,		/* mov eax,0xdeadbeef             */
	0x89, 0x04, 0x25, 0x00, 0x00, 0x02, 0x00,	/* mov [0x20000],eax      */
	0x89, 0x04, 0x25, 0x00, 0x50, 0x02, 0x00,	/* mov [0x25000],eax      */
	0xf4,					/* hlt                            */
};

static int vm_open(const char *dev, uint32_t flags, void **mem, int disk)
{
	struct hvx_config cfg = { .mem_size = MEM, .flags = flags };
	int fd = open(dev, O_RDWR);

	if (fd < 0) { perror(dev); exit(2); }
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); exit(2); }
	*mem = mmap(NULL, MEM, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (*mem == MAP_FAILED) { perror("mmap"); exit(2); }
	if (disk) {
		static unsigned char img[DISK];
		struct hvx_disk d = { .len = DISK, .data = (uintptr_t)img };

		memset(img, 0x5a, sizeof(img));
		if (ioctl(fd, HVX_SET_DISK, &d) < 0) { perror("HVX_SET_DISK"); exit(2); }
	}
	return fd;
}

static void start(int fd, void *mem)
{
	struct hvx_regs r = { .rip = HVX_GUEST_CODE_BASE, .rsp = MEM - 16, .rflags = 2 };

	memcpy((char *)mem + HVX_GUEST_CODE_BASE, guest, sizeof(guest));
	if (ioctl(fd, HVX_SET_REGS, &r) < 0) { perror("HVX_SET_REGS"); exit(2); }
}

/* Run to the next host-call return or HLT; returns the exit reason. */
static uint32_t run(int fd)
{
	struct hvx_exit ex;
	int i;

	for (i = 0; i < 1000000; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); exit(2); }
		if (ex.reason != HVX_EXIT_THROTTLE)
			return ex.reason;
	}
	return HVX_EXIT_THROTTLE;
}

static unsigned char staged[HVX_HC_SZ + 1];

struct pass { uint64_t v[24]; };

/* One pass of the port and dirty-log edges on a VM with both flags and a disk; the values
 * that must repeat after HVX_RESET_VM go into *p. */
static void port_pass(int fd, void *mem, struct pass *p)
{
	unsigned char buf[2 * HVX_HC_SZ];
	struct hvx_hc_io io;
	struct hvx_hc_state st;
	struct hvx_dirty dd;
	struct hvx_regs r;
	uint64_t ram[(64 + 63) / 64], disk[(16 + 63) / 64];
	int k = 0, i;
	uint32_t why;

	start(fd, mem);
	why = run(fd);
	CHECK(why == HVX_EXIT_HOSTCALL, "exit 1 reason %#x", why);
	memset(&io, 0, sizeof(io)); io.addr = (uintptr_t)buf; io.len = 2;
	CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 2 && !memcmp(buf, "HE", 2), "GET 2: n=%u", io.n);
	io.len = sizeof(buf);
	CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 3 && !memcmp(buf, "LLO", 3) &&
	      io.records == 1 && io.errors == 0, "GET rest: n=%u rec=%llu err=%llu", io.n,
	      (unsigned long long)io.records, (unsigned long long)io.errors);
	p->v[k++] = io.records;

	why = run(fd);
	CHECK(why == HVX_EXIT_HOSTCALL, "exit 2 reason %#x", why);
	CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 0 && io.errors == 1, "bad commit: n=%u err=%llu",
	      io.n, (unsigned long long)io.errors);
	why = run(fd);
	CHECK(why == HVX_EXIT_HOSTCALL, "exit 3 reason %#x", why);
	CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 0 && io.errors == 2, "overflow: n=%u err=%llu",
	      io.n, (unsigned long long)io.errors);
	why = run(fd);
	CHECK(why == HVX_EXIT_HOSTCALL, "exit 4 reason %#x", why);
	CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 2 && !memcmp(buf, "AB", 2) && io.records == 2,
	      "AB: n=%u rec=%llu", io.n, (unsigned long long)io.records);
	p->v[k++] = io.errors;

	for (i = 0; i <= HVX_HC_SZ; i++)
		staged[i] = (unsigned char)(i * 7 + 3);
	memset(&io, 0, sizeof(io)); io.addr = (uintptr_t)staged; io.len = HVX_HC_SZ;
	CHECK(ioctl(fd, HVX_HC_PUT, &io) == 0, "PUT full: %s", strerror(errno));
	io.len = 1;
	CHECK(ioctl(fd, HVX_HC_PUT, &io) < 0 && errno == ENOSPC, "PUT one more: errno %d", errno);
	io.len = 1; io.addr = 16;
	CHECK(ioctl(fd, HVX_HC_PUT, &io) < 0 && errno == ENOSPC, "PUT bad addr full: errno %d", errno);

	why = run(fd);
	CHECK(why == EXIT_HLT, "HLT reason %#x", why);
	CHECK(ioctl(fd, HVX_GET_REGS, &r) == 0, "GET_REGS");
	CHECK(r.rbx == HVX_HC_MAGIC, "magic %#llx", (unsigned long long)r.rbx);
	CHECK(r.rsi == HVX_HC_SZ, "count %llu", (unsigned long long)r.rsi);
	CHECK((uint32_t)r.rdi == (uint32_t)(staged[0] | staged[1] << 8 | staged[2] << 16 | (uint32_t)staged[3] << 24),
	      "4 bytes %#llx", (unsigned long long)r.rdi);
	CHECK((r.rbp & 0xffff) == (uint64_t)(staged[4] | staged[5] << 8), "2 bytes %#llx", (unsigned long long)r.rbp);
	CHECK((r.r8 & 0xff) == staged[6], "1 byte %#llx", (unsigned long long)r.r8);
	p->v[k++] = r.rbx; p->v[k++] = r.rsi; p->v[k++] = r.rdi; p->v[k++] = r.rbp & 0xffff; p->v[k++] = r.r8 & 0xff;

	memset(&st, 0, sizeof(st)); st.addr = (uintptr_t)buf; st.cap = 10;
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &st) < 0 && errno == ENOSPC, "GET_STATE short: errno %d", errno);
	st.cap = sizeof(buf);
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &st) == 0 && st.out_len == 0 && st.in_len == HVX_HC_SZ - 7 &&
	      buf[0] == staged[7] && buf[st.in_len - 1] == staged[HVX_HC_SZ - 1],
	      "GET_STATE: out=%u in=%u", st.out_len, st.in_len);
	p->v[k++] = st.in_len;
	st.addr = 16; st.cap = sizeof(buf);
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &st) < 0 && errno == EFAULT, "GET_STATE bad addr: errno %d", errno);

	memset(&st, 0, sizeof(st)); st.addr = (uintptr_t)buf;
	st.out_len = HVX_HC_SZ + 1;
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EINVAL, "SET_STATE out_len: errno %d", errno);
	st.out_len = 4; st.out_done = 5;
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EINVAL, "SET_STATE out_done: errno %d", errno);
	st.out_done = 0; st.in_len = HVX_HC_SZ + 1;
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EINVAL, "SET_STATE in_len: errno %d", errno);
	st.out_len = 10; st.out_done = 4; st.in_len = 100; st.addr = 16;
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EFAULT, "SET_STATE bad addr: errno %d", errno);
	memset(&st, 0, sizeof(st));
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &(struct hvx_hc_state){ .addr = (uintptr_t)buf, .cap = sizeof(buf) }) == 0,
	      "GET_STATE after EFAULT");
	for (i = 0; i < 110; i++)
		buf[i] = (unsigned char)(0x80 + i);
	st.addr = (uintptr_t)buf; st.out_len = 10; st.out_done = 4; st.in_len = 100; st.flags = HVX_HC_STATE_OVF;
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) == 0, "SET_STATE valid: %s", strerror(errno));
	{
		unsigned char back[2 * HVX_HC_SZ];
		struct hvx_hc_state g = { .addr = (uintptr_t)back, .cap = sizeof(back) };

		CHECK(ioctl(fd, HVX_HC_GET_STATE, &g) == 0 && g.out_len == 10 && g.out_done == 4 &&
		      g.in_len == 100 && g.flags == HVX_HC_STATE_OVF && !memcmp(back, buf, 110),
		      "state back: out=%u done=%u in=%u flags=%u", g.out_len, g.out_done, g.in_len, g.flags);
		memset(&io, 0, sizeof(io)); io.addr = (uintptr_t)back; io.len = sizeof(back);
		CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.n == 4 && !memcmp(back, buf, 4), "GET after SET: n=%u", io.n);
		p->v[k++] = io.n;
	}

	memset(&dd, 0, sizeof(dd)); dd.ram_addr = (uintptr_t)ram; dd.ram_bytes = 4;
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) < 0 && errno == ENOSPC, "DIRTY short ram: errno %d", errno);
	memset(&dd, 0, sizeof(dd)); dd.disk_addr = (uintptr_t)disk; dd.disk_bytes = 0;
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) < 0 && errno == ENOSPC, "DIRTY short disk: errno %d", errno);
	memset(&dd, 0, sizeof(dd));
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) == 0 && dd.ram_dirty >= 2 && dd.ram_bytes == sizeof(ram) &&
	      dd.disk_bytes == sizeof(disk) && dd.disk_dirty == 0,
	      "DIRTY NULL: ram=%llu/%llu disk=%llu/%llu", (unsigned long long)dd.ram_dirty,
	      (unsigned long long)dd.ram_bytes, (unsigned long long)dd.disk_dirty, (unsigned long long)dd.disk_bytes);
	memset(&dd, 0, sizeof(dd)); dd.ram_addr = 16; dd.ram_bytes = sizeof(ram);
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) < 0 && errno == EFAULT, "DIRTY bad addr: errno %d", errno);
	memset(&dd, 0, sizeof(dd)); memset(ram, 0, sizeof(ram)); memset(disk, 0xff, sizeof(disk));
	dd.ram_addr = (uintptr_t)ram; dd.ram_bytes = sizeof(ram);
	dd.disk_addr = (uintptr_t)disk; dd.disk_bytes = sizeof(disk); dd.flags = HVX_DIRTY_CLEAR;
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) == 0 && (ram[0] >> 32 & 1) && (ram[0] >> 37 & 1) && disk[0] == 0,
	      "DIRTY bits: ram %#llx disk %#llx", (unsigned long long)ram[0], (unsigned long long)disk[0]);
	p->v[k++] = (ram[0] >> 32 & 1) | (ram[0] >> 37 & 1) << 1;
	memset(&dd, 0, sizeof(dd));
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) == 0 && dd.ram_dirty == 0 && dd.disk_dirty == 0,
	      "after CLEAR: ram=%llu disk=%llu", (unsigned long long)dd.ram_dirty, (unsigned long long)dd.disk_dirty);
}

int main(void)
{
	const char *dev = getenv("HVX_DEV") ? getenv("HVX_DEV") : "/dev/hvx";
	uint32_t both = HVX_VM_HOSTCALL | HVX_VM_DIRTY_LOG;
	struct pass a = { { 0 } }, b = { { 0 } };
	struct hvx_hc_io io = { 0 };
	struct hvx_hc_state st = { 0 };
	struct hvx_dirty dd = { 0 };
	struct hvx_regs r;
	void *mem;
	int fd, i;

	/* no VM: every new ioctl refuses */
	fd = open(dev, O_RDWR);
	if (fd < 0) { perror(dev); return 2; }
	CHECK(ioctl(fd, HVX_RESET_VM) < 0 && errno == EINVAL, "no VM RESET: errno %d", errno);
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) < 0 && errno == EINVAL, "no VM DIRTY: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_GET, &io) < 0 && errno == EINVAL, "no VM HC_GET: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_PUT, &io) < 0 && errno == EINVAL, "no VM HC_PUT: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &st) < 0 && errno == EINVAL, "no VM GET_STATE: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EINVAL, "no VM SET_STATE: errno %d", errno);
	close(fd);

	/* a VM without the flags: the port is unhandled, the HC and dirty ioctls refuse */
	fd = vm_open(dev, 0, &mem, 1);
	CHECK(ioctl(fd, HVX_GET_DIRTY, &dd) < 0 && errno == EINVAL, "plain DIRTY: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_GET, &io) < 0 && errno == EINVAL, "plain HC_GET: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_PUT, &io) < 0 && errno == EINVAL, "plain HC_PUT: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_GET_STATE, &st) < 0 && errno == EINVAL, "plain GET_STATE: errno %d", errno);
	CHECK(ioctl(fd, HVX_HC_SET_STATE, &st) < 0 && errno == EINVAL, "plain SET_STATE: errno %d", errno);
	start(fd, mem);
	CHECK(run(fd) == EXIT_HLT, "plain guest did not HLT");
	CHECK(ioctl(fd, HVX_GET_REGS, &r) == 0 && (uint32_t)r.rbx == 0xffffffffu && (uint32_t)r.rsi == 0xffffffffu,
	      "plain port: rbx %#llx rsi %#llx", (unsigned long long)r.rbx, (unsigned long long)r.rsi);
	CHECK(ioctl(fd, HVX_RESET_VM) == 0, "plain RESET: %s", strerror(errno));
	close(fd);

	/* both flags and a disk: two passes with HVX_RESET_VM between, the same values */
	fd = vm_open(dev, both, &mem, 1);
	port_pass(fd, mem, &a);
	CHECK(ioctl(fd, HVX_RESET_VM) == 0, "RESET: %s", strerror(errno));
	{
		unsigned char buf[16];
		struct hvx_hc_state g = { .addr = (uintptr_t)buf, .cap = sizeof(buf) };

		CHECK(ioctl(fd, HVX_HC_GET_STATE, &g) == 0 && g.out_len == 0 && g.in_len == 0 && g.flags == 0,
		      "after RESET: out=%u in=%u flags=%u", g.out_len, g.in_len, g.flags);
		memset(&io, 0, sizeof(io)); io.addr = (uintptr_t)buf; io.len = sizeof(buf);
		CHECK(ioctl(fd, HVX_HC_GET, &io) == 0 && io.records == 0 && io.errors == 0,
		      "after RESET: rec=%llu err=%llu", (unsigned long long)io.records, (unsigned long long)io.errors);
	}
	port_pass(fd, mem, &b);
	CHECK(!memcmp(&a, &b, sizeof(a)), "second pass after RESET differs");
	close(fd);

	/* lifecycles: kmemleak and the page counts see whether they give everything back */
	for (i = 0; i < 50; i++) {
		fd = vm_open(dev, both, &mem, 1);
		start(fd, mem);
		if (run(fd) != HVX_EXIT_HOSTCALL) { CHECK(0, "lifecycle %d", i); close(fd); break; }
		if (i % 2)
			ioctl(fd, HVX_RESET_VM);
		munmap(mem, MEM);
		close(fd);
	}

	if (fails)
		printf("hvx_m138_edge_test: FAIL (%d of %d checks)\n", fails, checks);
	else
		printf("hvx_m138_edge_test: PASS (%d checks)\n", checks);
	return fails ? 1 : 0;
}

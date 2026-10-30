// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_test - userspace driver for the hvx mini-KVM device (milestone 3).
 *
 * Proves the whole guest now comes from userspace: this program opens /dev/hvx,
 * sizes guest RAM, mmap()s it, writes a tiny 64-bit guest into it, sets the
 * guest registers, and runs it. The guest writes a magic value to GPA 0x5000,
 * leaves the magic and its address in registers, then HLTs. We read the magic
 * back through the shared mapping and check the registers round-tripped.
 *
 * (Milestone 3 originally executed a CPUID here purely to demonstrate a VM-exit
 * back to userspace. As of milestone 12 CPUID is emulated IN-KERNEL -- it no
 * longer exits and would clobber RAX/RBX/RCX/RDX with synthetic vendor values --
 * so the demo instruction is gone; the register round-trip below still proves
 * the same M3 capability. See hvx_cpuid_test.c for the CPUID-emulation test.)
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_test hvx_test.c
 * Run   (inside L1):  sudo ./hvx_test
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

#include "hvx_abi.h"

#define EXIT_HLT   12

#define GUEST_MAGIC   0x6879646f2d657074ULL	/* "hydo-ept" */
#define GUEST_MAGIC_GPA 0x5000UL
#define GUEST_MEM_BYTES (64UL * 4096)

/*
 * Guest machine code, loaded at HVX_GUEST_CODE_BASE (0x3000):
 *   movabs $0x6879646f2d657074, %rax   ; the magic
 *   movabs $0x5000, %rbx                ; magic address (GPA == GVA)
 *   mov    %rax, (%rbx)                 ; store magic -> guest paging -> EPT
 *   hlt                                 ; VM-exit we stop at
 */
static const unsigned char guest_code[] = {
	0x48, 0xb8, 0x74, 0x70, 0x65, 0x2d, 0x6f, 0x64, 0x79, 0x68,
	0x48, 0xbb, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0x48, 0x89, 0x03,
	0xf4,
};

int main(void)
{
	int fd, i, rc = 1;
	void *mem;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_regs regs;
	struct hvx_exit ex;
	uint64_t got;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	printf("hvx_test: created VM with %lu bytes guest RAM\n", GUEST_MEM_BYTES);

	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }

	/* load the guest program at 0x3000 */
	memcpy((unsigned char *)mem + HVX_GUEST_CODE_BASE, guest_code, sizeof(guest_code));

	/* set the entry point and stack; the module defaults would also work */
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x2;
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); goto out; }

	/* run: one VM-exit per HVX_RUN; drive the loop from userspace */
	for (i = 0; i < 16; i++) {
		if (ioctl(fd, HVX_RUN, &ex) < 0) { perror("HVX_RUN"); goto out; }

		printf("hvx_test: exit #%d reason=%u insn_len=%u qual=0x%llx gpa=0x%llx\n",
		       i, ex.reason, ex.insn_len,
		       (unsigned long long)ex.qualification,
		       (unsigned long long)ex.guest_phys);

		if (ex.reason == HVX_EXIT_VMFAIL) {
			fprintf(stderr, "hvx_test: VMLAUNCH failed, vm-instruction-error=%llu\n",
				(unsigned long long)ex.qualification);
			goto out;
		}
		if (ex.reason == EXIT_HLT) {
			printf("hvx_test: guest executed HLT\n");
			break;
		}
		fprintf(stderr, "hvx_test: unexpected exit reason %u\n", ex.reason);
		goto out;
	}

	/* proof #1: the guest's GPRs round-tripped back through userspace.
	 * The guest left rax = magic and rbx = magic-address; the module's
	 * trampoline must have saved them from the guest into the register file. */
	if (ioctl(fd, HVX_GET_REGS, &regs) < 0) { perror("HVX_GET_REGS"); goto out; }
	printf("hvx_test: final regs rax=0x%llx rbx=0x%llx rip=0x%llx\n",
	       (unsigned long long)regs.rax, (unsigned long long)regs.rbx,
	       (unsigned long long)regs.rip);
	if (regs.rax != GUEST_MAGIC || regs.rbx != GUEST_MAGIC_GPA) {
		fprintf(stderr, "hvx_test: register round-trip FAIL (rax/rbx not preserved)\n");
		goto out;
	}

	/* proof #2: did the guest's store land in the page we can see via mmap? */
	got = *(volatile uint64_t *)((unsigned char *)mem + GUEST_MAGIC_GPA);
	if (got == GUEST_MAGIC) {
		printf("hvx_test: MILESTONE 3 PASS: userspace-loaded guest wrote 0x%llx via mmap+EPT\n",
		       (unsigned long long)got);
		rc = 0;
	} else {
		fprintf(stderr, "hvx_test: MILESTONE 3 FAIL: GPA 0x%lx holds 0x%llx, want 0x%llx\n",
			GUEST_MAGIC_GPA, (unsigned long long)got, (unsigned long long)GUEST_MAGIC);
	}

out:
	close(fd);
	return rc;
}

// Probe: can KVM run several vCPUs one at a time from one thread, with the local APIC in user
// space, deterministically? Stage 1 of docs/design/kvm-vcpus.md. No root, no in-kernel irqchip,
// hand-written 32-bit guests; every test ends by itself (each run has a limit of exits).
//
//   cc -O2 -Wall -o smpprobe smpprobe.c
//   smpprobe all | x2apic | sipi | ipi | race | cost
//
// Tests (the design's "Stage 1"):
//   x2apic  x2APIC MSR reads and writes exit to user space with their index; CPUID leaf 1 shows
//           APIC and x2APIC; an access to the xAPIC page is an MMIO exit
//   sipi    vCPU 0 writes INIT and two STARTUPs to the ICR; the probe starts vCPU 1 in real mode
//           at the vector's page; vCPU 1 reaches 32-bit protected mode; the second STARTUP is
//           ignored
//   ipi     an ICR write of vCPU 0 is injected into vCPU 1 with KVM_INTERRUPT at its next turn;
//           vCPU 1's handler runs once
//   race    two vCPUs increment a shared counter with an exit inside the read-modify-write, in
//           turns of q exits, q = 1..200, each q twice; the run equals a model of the guest exit
//           by exit; snapshots at every seventh turn, restored into a fresh VM, give the
//           straight run's hash
//   cost    nanoseconds per exit: one vCPU; two vCPUs switched at every exit on one thread; two
//           threads handing over at every exit; one vCPU run alternately by two threads
// Output: one PASS or FAIL line per check, then "smpprobe: <passed> of <checks> passed".
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/kvm.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define MEM (2u << 20)
#define NV 2
#define COUNTER 0x5000
#define HCOUNT 0x5004

enum { RUN, DONE, WAIT_SIPI };

struct vc {
  int fd;
  struct kvm_run *run;
  int st;
  int pending; // a vector for the next entry, or -1
  uint64_t exits;
};

static int kvm = -1, vm = -1, runsz;
static uint8_t *mem;
static struct vc v[NV];
static int nv;
static int checks, passed;

static void die(const char *m) {
  perror(m);
  exit(1);
}
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static void check(int ok, const char *name, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void check(int ok, const char *name, const char *fmt, ...) {
  va_list ap;
  checks++;
  passed += ok != 0;
  printf("%s %s: ", ok ? "PASS" : "FAIL", name);
  va_start(ap, fmt);
  vprintf(fmt, ap);
  va_end(ap);
  printf("\n");
  fflush(stdout);
}

static uint64_t fnv(uint64_t h, uint64_t x) {
  for (int i = 0; i < 8; i++) {
    h ^= (x >> (8 * i)) & 0xff;
    h *= 0x100000001b3ull;
  }
  return h;
}

static uint64_t apic_base(int i) {
  // the base, EN, EXTD (x2APIC mode), and the boot-CPU bit on vCPU 0 only
  return 0xfee00000ull | (1ull << 11) | (1ull << 10) | (i == 0 ? 1ull << 8 : 0);
}

static void set_cpuid(int fd) {
  int n = 256;
  struct kvm_cpuid2 *c = calloc(1, sizeof *c + n * sizeof(struct kvm_cpuid_entry2));
  c->nent = n;
  if (ioctl(kvm, KVM_GET_SUPPORTED_CPUID, c) < 0) die("KVM_GET_SUPPORTED_CPUID");
  for (unsigned i = 0; i < c->nent; i++)
    if (c->entries[i].function == 1) c->entries[i].ecx |= 1u << 21; // x2APIC (KVM always offers it)
  if (ioctl(fd, KVM_SET_CPUID2, c) < 0) die("KVM_SET_CPUID2");
  free(c);
}

static void vm_free(void) {
  for (int i = 0; i < nv; i++) {
    munmap(v[i].run, runsz);
    close(v[i].fd);
  }
  if (vm >= 0) close(vm);
  if (mem) munmap(mem, MEM);
  vm = -1;
  mem = 0;
  nv = 0;
}

// A VM with n vCPUs, no in-kernel irqchip, every unhandled MSR access to user space.
static void vm_new(int n) {
  vm_free();
  if (kvm < 0) kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
  if (kvm < 0) die("/dev/kvm");
  vm = ioctl(kvm, KVM_CREATE_VM, 0);
  if (vm < 0) die("KVM_CREATE_VM");
  struct kvm_enable_cap cap = {.cap = KVM_CAP_X86_USER_SPACE_MSR};
  cap.args[0] = KVM_MSR_EXIT_REASON_INVAL | KVM_MSR_EXIT_REASON_UNKNOWN | KVM_MSR_EXIT_REASON_FILTER;
  if (ioctl(vm, KVM_ENABLE_CAP, &cap) < 0) die("KVM_CAP_X86_USER_SPACE_MSR");
  mem = mmap(0, MEM, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (mem == MAP_FAILED) die("mmap");
  struct kvm_userspace_memory_region r = {.slot = 0, .guest_phys_addr = 0, .memory_size = MEM,
                                          .userspace_addr = (uint64_t)mem};
  if (ioctl(vm, KVM_SET_USER_MEMORY_REGION, &r) < 0) die("KVM_SET_USER_MEMORY_REGION");
  if (ioctl(vm, KVM_SET_TSS_ADDR, 0xfffbd000) < 0) die("KVM_SET_TSS_ADDR");
  runsz = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
  for (int i = 0; i < n; i++) {
    v[i].fd = ioctl(vm, KVM_CREATE_VCPU, i);
    if (v[i].fd < 0) die("KVM_CREATE_VCPU");
    v[i].run = mmap(0, runsz, PROT_READ | PROT_WRITE, MAP_SHARED, v[i].fd, 0);
    if (v[i].run == MAP_FAILED) die("mmap kvm_run");
    v[i].st = RUN;
    v[i].pending = -1;
    v[i].exits = 0;
    set_cpuid(v[i].fd);
  }
  nv = n;
  // a GDT at 0x6008: null, flat 32-bit code (0x08), flat data (0x10); its descriptor at 0x6000
  uint64_t gdt[3] = {0, 0x00cf9a000000ffffull, 0x00cf92000000ffffull};
  memcpy(mem + 0x6008, gdt, sizeof gdt);
  uint16_t lim = 0x17;
  uint32_t base = 0x6008;
  memcpy(mem + 0x6000, &lim, 2);
  memcpy(mem + 0x6002, &base, 4);
}

// vCPU i in flat 32-bit protected mode at CPL 0, paging off.
static void pm32(int i, uint32_t eip, uint32_t esp, uint32_t ecx) {
  struct kvm_sregs s;
  if (ioctl(v[i].fd, KVM_GET_SREGS, &s) < 0) die("KVM_GET_SREGS");
  struct kvm_segment c = {.base = 0, .limit = 0xffffffff, .selector = 0x08, .type = 0xb, .present = 1,
                          .dpl = 0, .db = 1, .s = 1, .l = 0, .g = 1};
  struct kvm_segment d = c;
  d.selector = 0x10;
  d.type = 3;
  s.cs = c;
  s.ds = s.es = s.fs = s.gs = s.ss = d;
  s.cr0 = 0x11;
  s.gdt.base = 0x6008;
  s.gdt.limit = 0x17;
  s.idt.base = 0x3000;
  s.idt.limit = 0x7ff;
  s.apic_base = apic_base(i);
  if (ioctl(v[i].fd, KVM_SET_SREGS, &s) < 0) die("KVM_SET_SREGS");
  struct kvm_regs g = {0};
  g.rip = eip;
  g.rsp = esp;
  g.rcx = ecx;
  g.rflags = 0x2;
  if (ioctl(v[i].fd, KVM_SET_REGS, &g) < 0) die("KVM_SET_REGS");
}

// What a vCPU's run ended with.
struct ex {
  int kind; // KVM_EXIT_*
  uint16_t port;
  uint32_t msr;
  uint64_t data;
  uint64_t addr;
};

// Finish the instruction of an I/O, MMIO or MSR exit now (KVM does it at the next KVM_RUN): a run
// with immediate_exit, as the engine's Cpu::complete.
static void complete(int i) {
  v[i].run->immediate_exit = 1;
  int r = ioctl(v[i].fd, KVM_RUN, 0);
  v[i].run->immediate_exit = 0;
  if (!(r < 0 && errno == EINTR)) {
    fprintf(stderr, "smpprobe: completion of vCPU %d gave %d (exit %u)\n", i, r, v[i].run->exit_reason);
    exit(1);
  }
}

// Run vCPU i to its next exit; I/O and MSR exits are answered (reads give `reply`) and completed.
static struct ex step(int i, uint64_t reply) {
  struct ex e = {0};
  struct kvm_run *r = v[i].run;
  for (;;) {
    int rc = ioctl(v[i].fd, KVM_RUN, 0);
    if (rc < 0 && errno == EINTR) continue;
    if (rc < 0) die("KVM_RUN");
    break;
  }
  e.kind = r->exit_reason;
  switch (r->exit_reason) {
  case KVM_EXIT_IO:
    e.port = r->io.port;
    if (r->io.direction == KVM_EXIT_IO_IN) memset((uint8_t *)r + r->io.data_offset, 0, r->io.size * r->io.count);
    complete(i);
    break;
  case KVM_EXIT_X86_RDMSR:
    e.msr = r->msr.index;
    r->msr.data = reply;
    r->msr.error = 0;
    complete(i);
    break;
  case KVM_EXIT_X86_WRMSR:
    e.msr = r->msr.index;
    e.data = r->msr.data;
    r->msr.error = 0;
    complete(i);
    break;
  case KVM_EXIT_MMIO:
    e.addr = r->mmio.phys_addr;
    if (!r->mmio.is_write) memset(r->mmio.data, 0, 8);
    complete(i);
    break;
  case KVM_EXIT_HLT:
  case KVM_EXIT_IRQ_WINDOW_OPEN:
    break;
  default:
    fprintf(stderr, "smpprobe: vCPU %d: unexpected exit %u\n", i, r->exit_reason);
    exit(1);
  }
  v[i].exits++;
  return e;
}

static struct kvm_regs regs(int i) {
  struct kvm_regs g;
  if (ioctl(v[i].fd, KVM_GET_REGS, &g) < 0) die("KVM_GET_REGS");
  return g;
}

// ---------------------------------------------------------------- x2apic
static void t_x2apic(void) {
  vm_new(1);
  static const uint8_t code[] = {
      0xb8, 0x01, 0, 0, 0,          // mov eax,1
      0x31, 0xc9,                   // xor ecx,ecx
      0x0f, 0xa2,                   // cpuid
      0xe6, 0x10,                   // out 0x10,al
      0xb9, 0x02, 0x08, 0, 0,       // mov ecx,0x802 (the x2APIC ID)
      0x0f, 0x32,                   // rdmsr
      0xe6, 0x12,                   // out 0x12,al
      0xb9, 0x30, 0x08, 0, 0,       // mov ecx,0x830 (the ICR)
      0xba, 0x01, 0, 0, 0,          // mov edx,1
      0xb8, 0xfd, 0, 0, 0,          // mov eax,0xfd
      0x0f, 0x30,                   // wrmsr
      0xb9, 0x0b, 0x08, 0, 0,       // mov ecx,0x80b (EOI)
      0x31, 0xc0,                   // xor eax,eax
      0x31, 0xd2,                   // xor edx,edx
      0x0f, 0x30,                   // wrmsr
      0xa1, 0x30, 0x00, 0xe0, 0xfe, // mov eax,[0xfee00030] (the xAPIC page)
      0xe6, 0x13,                   // out 0x13,al
      0xf4,                         // hlt
  };
  memcpy(mem + 0x1000, code, sizeof code);
  pm32(0, 0x1000, 0x7ff0, 0);
  struct ex e = step(0, 0);
  struct kvm_regs g = regs(0);
  check(e.kind == KVM_EXIT_IO && e.port == 0x10 && (g.rcx >> 21 & 1) && (g.rdx >> 9 & 1), "x2apic.cpuid",
        "leaf 1 ecx %#llx (x2APIC bit %llu) edx %#llx (APIC bit %llu)", (unsigned long long)g.rcx,
        (unsigned long long)(g.rcx >> 21 & 1), (unsigned long long)g.rdx, (unsigned long long)(g.rdx >> 9 & 1));
  e = step(0, 0x12345678);
  check(e.kind == KVM_EXIT_X86_RDMSR && e.msr == 0x802, "x2apic.rdmsr", "exit %d index %#x", e.kind, e.msr);
  e = step(0, 0);
  g = regs(0);
  check(e.kind == KVM_EXIT_IO && e.port == 0x12 && (uint32_t)g.rax == 0x12345678, "x2apic.rdmsr-value",
        "eax %#x after the read", (uint32_t)g.rax);
  e = step(0, 0);
  check(e.kind == KVM_EXIT_X86_WRMSR && e.msr == 0x830 && e.data == 0x1000000fdull, "x2apic.icr",
        "exit %d index %#x data %#llx", e.kind, e.msr, (unsigned long long)e.data);
  e = step(0, 0);
  check(e.kind == KVM_EXIT_X86_WRMSR && e.msr == 0x80b && e.data == 0, "x2apic.eoi", "exit %d index %#x",
        e.kind, e.msr);
  e = step(0, 0);
  check(e.kind == KVM_EXIT_MMIO && e.addr == 0xfee00030, "x2apic.xapic-page", "exit %d address %#llx", e.kind,
        (unsigned long long)e.addr);
  e = step(0, 0);
  struct ex h = step(0, 0);
  check(e.kind == KVM_EXIT_IO && e.port == 0x13 && h.kind == KVM_EXIT_HLT, "x2apic.end", "exits %d %d",
        e.kind, h.kind);
}

// ---------------------------------------------------------------- sipi
static void sipi_start(int t, uint32_t vector) {
  // the state after INIT and STARTUP: KVM's reset state (real mode) with CS = vector << 8
  struct kvm_sregs s;
  if (ioctl(v[t].fd, KVM_GET_SREGS, &s) < 0) die("KVM_GET_SREGS");
  s.cs.selector = (uint16_t)(vector << 8);
  s.cs.base = (uint64_t)vector << 12;
  s.cs.limit = 0xffff;
  s.apic_base = apic_base(t);
  if (ioctl(v[t].fd, KVM_SET_SREGS, &s) < 0) die("KVM_SET_SREGS sipi");
  struct kvm_regs g = {0};
  g.rip = 0;
  g.rflags = 0x2;
  if (ioctl(v[t].fd, KVM_SET_REGS, &g) < 0) die("KVM_SET_REGS sipi");
}

static void t_sipi(void) {
  vm_new(2);
  static const uint8_t bsp[] = {
      0xb9, 0x30, 0x08, 0, 0, // mov ecx,0x830
      0xba, 0x01, 0, 0, 0,    // mov edx,1 (destination: APIC ID 1)
      0xb8, 0x00, 0x45, 0, 0, // mov eax,0x4500 (INIT, level assert)
      0x0f, 0x30,             // wrmsr
      0xb8, 0x08, 0x06, 0, 0, // mov eax,0x0608 (STARTUP, vector 8: page 0x8000)
      0x0f, 0x30,             // wrmsr
      0x0f, 0x30,             // wrmsr (the second STARTUP)
      0xe6, 0x11,             // out 0x11,al
      0xf4,                   // hlt
  };
  static const uint8_t rm[] = {
      0xb0, 0x01,                                     // mov al,1
      0xe6, 0x20,                                     // out 0x20,al (real mode)
      0x0f, 0x01, 0x16, 0x00, 0x60,                   // lgdt [0x6000]
      0x0f, 0x20, 0xc0,                               // mov eax,cr0
      0x0c, 0x01,                                     // or al,1
      0x0f, 0x22, 0xc0,                               // mov cr0,eax
      0x66, 0xea, 0x00, 0x81, 0x00, 0x00, 0x08, 0x00, // jmp far 0x08:0x8100
  };
  static const uint8_t pm[] = {
      0x66, 0xb8, 0x10, 0x00, // mov ax,0x10
      0x8e, 0xd8,             // mov ds,ax
      0xb0, 0x02,             // mov al,2
      0xe6, 0x21,             // out 0x21,al (protected mode)
      0xf4,                   // hlt
  };
  memcpy(mem + 0x1000, bsp, sizeof bsp);
  memcpy(mem + 0x8000, rm, sizeof rm);
  memcpy(mem + 0x8100, pm, sizeof pm);
  pm32(0, 0x1000, 0x7ff0, 0);
  v[1].st = WAIT_SIPI;
  int inits = 0, sipis = 0, ignored = 0, n = 0;
  struct ex e;
  do {
    e = step(0, 0);
    if (e.kind == KVM_EXIT_X86_WRMSR && e.msr == 0x830 && (e.data >> 32) == 1) {
      unsigned dm = (e.data >> 8) & 7;
      if (dm == 5) {
        inits++;
        v[1].st = WAIT_SIPI;
      } else if (dm == 6) {
        if (v[1].st == WAIT_SIPI) {
          sipi_start(1, e.data & 0xff);
          v[1].st = RUN;
          sipis++;
        } else {
          ignored++;
        }
      }
    }
  } while (e.kind != KVM_EXIT_HLT && ++n < 100);
  check(inits == 1 && sipis == 1 && ignored == 1, "sipi.icr", "INIT %d, STARTUP taken %d, ignored %d", inits,
        sipis, ignored);
  uint16_t ports[4];
  int np = 0;
  n = 0;
  do {
    e = step(1, 0);
    if (e.kind == KVM_EXIT_IO && np < 4) ports[np++] = e.port;
  } while (e.kind != KVM_EXIT_HLT && ++n < 100);
  struct kvm_sregs s;
  if (ioctl(v[1].fd, KVM_GET_SREGS, &s) < 0) die("KVM_GET_SREGS");
  check(np == 2 && ports[0] == 0x20 && ports[1] == 0x21, "sipi.real-mode", "ports %#x %#x (%d I/O exits)",
        np > 0 ? ports[0] : 0, np > 1 ? ports[1] : 0, np);
  check(e.kind == KVM_EXIT_HLT && s.cs.selector == 0x08 && s.cs.db == 1 && (s.cr0 & 1), "sipi.protected-mode",
        "cs %#x db %u cr0 %#llx", s.cs.selector, s.cs.db, (unsigned long long)s.cr0);
}

// ---------------------------------------------------------------- ipi
// Before vCPU i runs: its pending vector is injected when KVM says it can take it, else the
// interrupt window is asked for (the engine's delivery rule, per vCPU).
static int deliver(int i) {
  struct kvm_run *r = v[i].run;
  if (v[i].pending < 0) {
    r->request_interrupt_window = 0;
    return 0;
  }
  if (r->ready_for_interrupt_injection && r->if_flag) {
    struct kvm_interrupt irq = {.irq = (uint32_t)v[i].pending};
    if (ioctl(v[i].fd, KVM_INTERRUPT, &irq) < 0) die("KVM_INTERRUPT");
    v[i].pending = -1;
    r->request_interrupt_window = 0;
    return 1;
  }
  r->request_interrupt_window = 1;
  return 0;
}

static uint64_t ipi_once(int *handled, int *injected, int *windows, int *last_port) {
  vm_new(2);
  static const uint8_t bsp[] = {
      0xb9, 0x30, 0x08, 0, 0, // mov ecx,0x830
      0xba, 0x01, 0, 0, 0,    // mov edx,1
      0xb8, 0x40, 0, 0, 0,    // mov eax,0x40 (fixed, vector 0x40)
      0x0f, 0x30,             // wrmsr
      0xe6, 0x11,             // out 0x11,al
      0xf4,                   // hlt
  };
  static const uint8_t ap[] = {
      0xfb,                         // sti
      0xe6, 0x31,                   // L: out 0x31,al
      0xa1, 0x04, 0x50, 0x00, 0x00, // mov eax,[0x5004]
      0x83, 0xf8, 0x01,             // cmp eax,1
      0x75, 0xf4,                   // jne L
      0xe6, 0x32,                   // out 0x32,al
      0xf4,                         // hlt
  };
  static const uint8_t handler[] = {
      0xff, 0x05, 0x04, 0x50, 0x00, 0x00, // inc dword [0x5004]
      0xcf,                               // iretd
  };
  memcpy(mem + 0x1000, bsp, sizeof bsp);
  memcpy(mem + 0x2000, ap, sizeof ap);
  memcpy(mem + 0x4000, handler, sizeof handler);
  uint8_t gate[8] = {0x00, 0x40, 0x08, 0x00, 0x00, 0x8e, 0x00, 0x00}; // 32-bit interrupt gate to 0x4000
  memcpy(mem + 0x3000 + 0x40 * 8, gate, 8);
  pm32(0, 0x1000, 0x7000, 0);
  pm32(1, 0x2000, 0x7ff0, 0);
  uint64_t h = 0xcbf29ce484222325ull;
  struct ex e;
  // vCPU 1: a turn of 3 exits; vCPU 0: to its HLT; vCPU 1: to its HLT
  for (int k = 0; k < 3; k++) {
    e = step(1, 0);
    h = fnv(h, (uint64_t)e.kind << 16 | e.port);
  }
  int n = 0;
  do {
    e = step(0, 0);
    h = fnv(h, (uint64_t)e.kind << 16 | e.port);
    if (e.kind == KVM_EXIT_X86_WRMSR && e.msr == 0x830 && ((e.data >> 8) & 7) == 0)
      v[e.data >> 32 & 1].pending = e.data & 0xff;
  } while (e.kind != KVM_EXIT_HLT && ++n < 100);
  *injected = *windows = 0;
  *last_port = 0;
  n = 0;
  do {
    *injected += deliver(1);
    e = step(1, 0);
    h = fnv(h, (uint64_t)e.kind << 16 | e.port);
    if (e.kind == KVM_EXIT_IRQ_WINDOW_OPEN) (*windows)++;
    if (e.kind == KVM_EXIT_IO) *last_port = e.port;
  } while (e.kind != KVM_EXIT_HLT && ++n < 1000);
  uint32_t c;
  memcpy(&c, mem + HCOUNT, 4);
  *handled = (int)c;
  struct kvm_regs g = regs(1);
  h = fnv(h, g.rip);
  h = fnv(h, g.rsp);
  h = fnv(h, c);
  return h;
}

static void t_ipi(void) {
  int handled, injected, windows, port;
  uint64_t h1 = ipi_once(&handled, &injected, &windows, &port);
  check(handled == 1 && injected == 1 && port == 0x32, "ipi.delivered",
        "handler ran %d time(s), %d injection(s), %d window exit(s), last port %#x", handled, injected, windows,
        port);
  uint64_t h2 = ipi_once(&handled, &injected, &windows, &port);
  check(h1 == h2, "ipi.repeat", "%016llx and %016llx", (unsigned long long)h1, (unsigned long long)h2);
}

// ---------------------------------------------------------------- race
// The guest of both vCPUs (vCPU 0 at 0x1000, vCPU 1 at 0x2000), ecx iterations:
//   L: mov eax,[COUNTER]; out 0x40,al; inc eax; mov [COUNTER],eax; out 0x41,al; dec ecx; jnz L
//      out 0x42,al; hlt
static const uint8_t race_code[] = {
    0xa1, 0x00, 0x50, 0x00, 0x00, // mov eax,[0x5000]
    0xe6, 0x40,                   // out 0x40,al
    0x40,                         // inc eax
    0xa3, 0x00, 0x50, 0x00, 0x00, // mov [0x5000],eax
    0xe6, 0x41,                   // out 0x41,al
    0x49,                         // dec ecx
    0x75, 0xee,                   // jnz L
    0xe6, 0x42,                   // out 0x42,al
    0xf4,                         // hlt
};
#define ITERS 200

// A model of the guest, exit by exit: its expected next exit and the counter.
struct model {
  int stage[NV];
  uint32_t eax[NV], ecx[NV];
  uint32_t cnt;
};
static int model_exit(struct model *m, int i) { // the port of the next exit, or 0 for HLT
  switch (m->stage[i]) {
  case 0:
    m->eax[i] = m->cnt;
    m->stage[i] = 1;
    return 0x40;
  case 1:
    m->cnt = ++m->eax[i];
    m->stage[i] = --m->ecx[i] ? 0 : 2;
    return 0x41;
  case 2:
    m->stage[i] = 3;
    return 0x42;
  default:
    m->stage[i] = 4;
    return 0;
  }
}

// The scheduler's state and a snapshot of the whole machine at a turn boundary.
struct sched {
  int cur, left, turn;
  int st[NV];
  uint64_t exits[NV], total;
  struct model m;
};
struct snap {
  struct sched s;
  struct kvm_regs r[NV];
  struct kvm_sregs sr[NV];
  struct kvm_vcpu_events ev[NV];
  struct kvm_fpu fpu[NV];
  uint8_t *mem;
};
#define MAXSNAP 64
static struct snap snaps[MAXSNAP];
static int nsnap;

static void race_vm(void) {
  vm_new(2);
  memcpy(mem + 0x1000, race_code, sizeof race_code);
  memcpy(mem + 0x2000, race_code, sizeof race_code);
  pm32(0, 0x1000, 0x7000, ITERS);
  pm32(1, 0x2000, 0x7ff0, ITERS);
}

static void take(struct sched *s) {
  if (nsnap >= MAXSNAP) return;
  struct snap *p = &snaps[nsnap++];
  p->s = *s;
  for (int i = 0; i < NV; i++) {
    if (ioctl(v[i].fd, KVM_GET_REGS, &p->r[i]) < 0) die("KVM_GET_REGS");
    if (ioctl(v[i].fd, KVM_GET_SREGS, &p->sr[i]) < 0) die("KVM_GET_SREGS");
    if (ioctl(v[i].fd, KVM_GET_VCPU_EVENTS, &p->ev[i]) < 0) die("KVM_GET_VCPU_EVENTS");
    if (ioctl(v[i].fd, KVM_GET_FPU, &p->fpu[i]) < 0) die("KVM_GET_FPU");
  }
  p->mem = malloc(MEM);
  memcpy(p->mem, mem, MEM);
}

static void restore(const struct snap *p, struct sched *s) {
  vm_new(2);
  memcpy(mem, p->mem, MEM);
  for (int i = 0; i < NV; i++) {
    if (ioctl(v[i].fd, KVM_SET_SREGS, &p->sr[i]) < 0) die("KVM_SET_SREGS");
    if (ioctl(v[i].fd, KVM_SET_REGS, &p->r[i]) < 0) die("KVM_SET_REGS");
    if (ioctl(v[i].fd, KVM_SET_FPU, &p->fpu[i]) < 0) die("KVM_SET_FPU");
    if (ioctl(v[i].fd, KVM_SET_VCPU_EVENTS, &p->ev[i]) < 0) die("KVM_SET_VCPU_EVENTS");
  }
  *s = p->s;
}

// Run the scheduler to the end: turns of q exits, round-robin, HLT ends a vCPU. Returns the hash of
// the end state; *ok is cleared when an exit differs from the model's.
static uint64_t race_go(struct sched *s, int q, int snap_every, int *ok) {
  while (s->st[0] == RUN || s->st[1] == RUN) {
    if (s->total > 100000) {
      *ok = 0;
      break;
    }
    int i = s->cur;
    if (s->st[i] != RUN) {
      s->cur = (i + 1) % NV;
      s->left = q;
      continue;
    }
    struct ex e = step(i, 0);
    s->exits[i]++;
    s->total++;
    int want = model_exit(&s->m, i);
    int got = e.kind == KVM_EXIT_HLT ? 0 : e.kind == KVM_EXIT_IO ? e.port : -1;
    if (got != want) *ok = 0;
    int end = e.kind == KVM_EXIT_HLT;
    if (end) s->st[i] = DONE;
    if (end || --s->left == 0) {
      s->cur = (i + 1) % NV;
      s->left = q;
      s->turn++;
      if (snap_every && s->turn % snap_every == 0) take(s);
    }
  }
  uint32_t c;
  memcpy(&c, mem + COUNTER, 4);
  if (c != s->m.cnt) *ok = 0;
  uint64_t h = 0xcbf29ce484222325ull;
  h = fnv(h, c);
  h = fnv(h, s->turn);
  for (int i = 0; i < NV; i++) {
    struct kvm_regs g = regs(i);
    h = fnv(h, g.rax);
    h = fnv(h, g.rcx);
    h = fnv(h, g.rip);
    h = fnv(h, g.rflags);
    h = fnv(h, s->exits[i]);
  }
  return h;
}

static uint64_t race_once(int q, int snap_every, int *ok, uint32_t *count) {
  race_vm();
  struct sched s = {.cur = 0, .left = q};
  for (int i = 0; i < NV; i++) s.m.ecx[i] = ITERS;
  uint64_t h = race_go(&s, q, snap_every, ok);
  *count = s.m.cnt;
  return h;
}

static void t_race(void) {
  int ok = 1, repeat = 1, differ = 0;
  uint64_t grid = 0xcbf29ce484222325ull;
  char lost[2048];
  int lp = 0;
  for (int q = 1; q <= 200; q++) {
    uint32_t c1, c2;
    uint64_t h1 = race_once(q, 0, &ok, &c1);
    uint64_t h2 = race_once(q, 0, &ok, &c2);
    if (h1 != h2 || c1 != c2) repeat = 0;
    grid = fnv(grid, h1);
    if (c1 != 2 * ITERS) differ++;
    if (q <= 12 || q % 50 == 0)
      lp += snprintf(lost + lp, sizeof lost - lp, " q=%d:%u", q, 2 * ITERS - c1);
  }
  check(ok, "race.model", "every exit of every run equals the model's");
  check(repeat, "race.repeat", "each q from 1 to 200 twice with one hash; grid hash %016llx",
        (unsigned long long)grid);
  check(differ > 0 && differ < 200, "race.lost", "%d of 200 turn lengths lose updates; lost by q:%s", differ,
        lost);
  // snapshots at every seventh turn, each restored into a fresh VM and run to the end
  static const int qs[] = {1, 2, 3, 5, 8, 13, 50, 199};
  int all = 1, n = 0;
  for (unsigned k = 0; k < sizeof qs / sizeof qs[0]; k++) {
    for (int j = 0; j < nsnap; j++) free(snaps[j].mem);
    nsnap = 0;
    uint32_t c;
    int ok2 = 1;
    uint64_t h = race_once(qs[k], 7, &ok2, &c);
    int taken = nsnap;
    for (int j = 0; j < taken; j++) {
      struct sched s;
      restore(&snaps[j], &s);
      int ok3 = 1;
      uint64_t hr = race_go(&s, qs[k], 0, &ok3);
      n++;
      if (hr != h || !ok3) {
        all = 0;
        printf("  q=%d snapshot at turn %d: %016llx against %016llx\n", qs[k], snaps[j].s.turn,
               (unsigned long long)hr, (unsigned long long)h);
      }
    }
    for (int j = 0; j < taken; j++) free(snaps[j].mem);
    nsnap = 0;
  }
  check(all && n > 0, "race.snapshots", "%d runs from snapshots, each equal to its straight run", n);
}

// ---------------------------------------------------------------- cost
static const uint8_t loop_code[] = {0xe6, 0x50, 0xeb, 0xfc}; // L: out 0x50,al; jmp L

static double cost_one(int n) {
  vm_new(1);
  memcpy(mem + 0x1000, loop_code, sizeof loop_code);
  pm32(0, 0x1000, 0x7000, 0);
  double t = now();
  for (int k = 0; k < n; k++) step(0, 0);
  return (now() - t) / n * 1e9;
}

static double cost_switch(int n) {
  vm_new(2);
  memcpy(mem + 0x1000, loop_code, sizeof loop_code);
  pm32(0, 0x1000, 0x7000, 0);
  pm32(1, 0x1000, 0x7800, 0);
  step(0, 0);
  step(1, 0);
  double t = now();
  for (int k = 0; k < n; k++) step(k & 1, 0);
  return (now() - t) / n * 1e9;
}

// two threads hand a baton over at every exit
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int baton, remaining;
struct worker {
  int me, vcpu;
};
static void *work(void *a) {
  struct worker *w = a;
  for (;;) {
    pthread_mutex_lock(&mu);
    while (baton != w->me && remaining > 0) pthread_cond_wait(&cv, &mu);
    if (remaining <= 0) {
      pthread_mutex_unlock(&mu);
      return 0;
    }
    pthread_mutex_unlock(&mu);
    step(w->vcpu, 0);
    pthread_mutex_lock(&mu);
    remaining--;
    baton = 1 - w->me;
    pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&mu);
  }
}
static double cost_threads(int n, int same_vcpu) {
  vm_new(2);
  memcpy(mem + 0x1000, loop_code, sizeof loop_code);
  pm32(0, 0x1000, 0x7000, 0);
  pm32(1, 0x1000, 0x7800, 0);
  struct worker w0 = {0, 0}, w1 = {1, same_vcpu ? 0 : 1};
  baton = 0;
  remaining = n;
  pthread_t a, b;
  double t = now();
  pthread_create(&a, 0, work, &w0);
  pthread_create(&b, 0, work, &w1);
  pthread_join(a, 0);
  pthread_join(b, 0);
  return (now() - t) / n * 1e9;
}

static void t_cost(void) {
  double one = cost_one(200000), sw = cost_switch(200000);
  double th = cost_threads(20000, 0), pid = cost_threads(200, 1);
  printf("COST ns per exit: one vCPU %.0f; two vCPUs switched every exit, one thread %.0f; "
         "two threads, a handover every exit %.0f; one vCPU run alternately by two threads %.0f\n",
         one, sw, th, pid);
  check(sw - one < 20000, "cost.switch", "a switch costs %.0f ns over an exit (go: under 20000)", sw - one);
}

int main(int argc, char **argv) {
  const char *t = argc > 1 ? argv[1] : "all";
  int all = !strcmp(t, "all");
  if (all || !strcmp(t, "x2apic")) t_x2apic();
  if (all || !strcmp(t, "sipi")) t_sipi();
  if (all || !strcmp(t, "ipi")) t_ipi();
  if (all || !strcmp(t, "race")) t_race();
  if (all || !strcmp(t, "cost")) t_cost();
  vm_free();
  printf("smpprobe: %d of %d passed\n", passed, checks);
  return passed == checks && checks > 0 ? 0 : 1;
}

// Probe: can a host perf_event count a KVM guest's retired conditional branches exactly
// (guest only, user mode only), interrupt KVM_RUN near a target, and can single steps reach
// the exact count? No root: perf_event_paranoid 2 allows a thread's own user-mode events.
//
//   cc -O2 -Wall -o pmuprobe pmuprobe.c
//   pmuprobe <test> <cpu> <pmu type> <raw config> [args]
//   tests: exact N reps | skid T reps | step T margin reps | exits reps with(0|1) | kernel
//
// <pmu type> is /sys/bus/event_source/devices/<pmu>/type (cpu, or cpu_core and cpu_atom on a
// hybrid Intel CPU), <raw config> the event: Intel's conditional branches retired, 0x11c4 on a
// performance core and 0x7ec4 on an efficiency core of a 13th-generation Core; AMD's ex_ret_cond
// is 0xd1. Measurements and what they mean: hydo/explore/RESEARCH-kvm-exit-free.md, option 1.
// One VM at a time, no root; every run ends by itself.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/kvm.h>
#include <linux/perf_event.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static int kvm, vm, vcpu, pfd = -1;
static struct kvm_run *run;
static uint8_t *mem;
static int ptype;
static uint64_t pconfig;

static void die(const char *m) { perror(m); exit(1); }
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static void on_sig(int s) { (void)s; }

static uint64_t count(void) {
  uint64_t v;
  if (read(pfd, &v, 8) != 8) die("read perf");
  return v;
}

// 32-bit protected mode, flat, CPL 3, IOPL 3, code at 0x1000:
//   start: mov ecx, N ; l: dec ecx ; jnz l ; out 0x10, al ; jmp start
static void setup(uint32_t n) {
  kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
  if (kvm < 0) die("/dev/kvm");
  vm = ioctl(kvm, KVM_CREATE_VM, 0);
  if (vm < 0) die("create vm");
  mem = mmap(0, 1 << 21, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
  struct kvm_userspace_memory_region r = {.slot = 0, .guest_phys_addr = 0, .memory_size = 1 << 21,
                                          .userspace_addr = (uint64_t)mem};
  if (ioctl(vm, KVM_SET_USER_MEMORY_REGION, &r) < 0) die("mem");
  if (ioctl(vm, KVM_SET_TSS_ADDR, 0xfffbd000) < 0) die("tss");
  vcpu = ioctl(vm, KVM_CREATE_VCPU, 0);
  if (vcpu < 0) die("vcpu");
  int sz = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
  run = mmap(0, sz, PROT_READ | PROT_WRITE, MAP_SHARED, vcpu, 0);
  uint8_t code[] = {0xb9, 0, 0, 0, 0, 0x49, 0x75, 0xfd, 0xe6, 0x10, 0xeb, 0xf4};
  memcpy(code + 1, &n, 4);
  memcpy(mem + 0x1000, code, sizeof code);
  struct kvm_sregs s;
  if (ioctl(vcpu, KVM_GET_SREGS, &s) < 0) die("get sregs");
  struct kvm_segment c = {.base = 0, .limit = 0xffffffff, .selector = 0x1b, .type = 0xb, .present = 1,
                          .dpl = 3, .db = 1, .s = 1, .l = 0, .g = 1};
  struct kvm_segment d = c;
  d.selector = 0x23;
  d.type = 3;
  s.cs = c;
  s.ds = s.es = s.fs = s.gs = s.ss = d;
  s.cr0 = 0x11;
  if (ioctl(vcpu, KVM_SET_SREGS, &s) < 0) die("set sregs");
  struct kvm_regs g = {0};
  g.rip = 0x1000;
  g.rsp = 0x8000;
  g.rflags = 0x2 | (3 << 12);
  if (ioctl(vcpu, KVM_SET_REGS, &g) < 0) die("set regs");
}

static void open_counter(uint64_t period, int user_only) {
  struct perf_event_attr a;
  memset(&a, 0, sizeof a);
  a.size = sizeof a;
  a.type = ptype;
  a.config = pconfig;
  a.exclude_host = 1;
  a.exclude_kernel = user_only;
  a.exclude_hv = 1;
  a.pinned = 1;
  if (period) {
    a.sample_period = period;
    a.wakeup_events = 1;
  }
  pfd = syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
  if (pfd < 0) die("perf_event_open");
  if (period) {
    struct f_owner_ex o = {.type = F_OWNER_TID, .pid = syscall(SYS_gettid)};
    if (fcntl(pfd, F_SETOWN_EX, &o) < 0) die("setown");
    if (fcntl(pfd, F_SETSIG, SIGUSR1) < 0) die("setsig");
    if (fcntl(pfd, F_SETFL, O_ASYNC) < 0) die("async");
  }
}

static uint32_t ecx(void) {
  struct kvm_regs g;
  if (ioctl(vcpu, KVM_GET_REGS, &g) < 0) die("get regs");
  return (uint32_t)g.rcx;
}

static int kvm_run(void) { return ioctl(vcpu, KVM_RUN, 0); }

int main(int argc, char **argv) {
  if (argc < 5) {
    fprintf(stderr, "usage\n");
    return 2;
  }
  const char *test = argv[1];
  int cpu = atoi(argv[2]);
  ptype = atoi(argv[3]);
  pconfig = strtoull(argv[4], 0, 0);
  cpu_set_t set;
  CPU_ZERO(&set);
  CPU_SET(cpu, &set);
  if (sched_setaffinity(0, sizeof set, &set) < 0) die("affinity");
  struct sigaction sa = {0};
  sa.sa_handler = on_sig;
  sigaction(SIGUSR1, &sa, 0);

  if (!strcmp(test, "kernel")) { // can the guest kernel be counted without CAP_PERFMON?
    setup(1000);
    struct perf_event_attr a;
    memset(&a, 0, sizeof a);
    a.size = sizeof a;
    a.type = ptype;
    a.config = pconfig;
    a.exclude_host = 1;
    a.exclude_hv = 1;
    int fd = syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
    printf("guest kernel+user counting: %s\n", fd < 0 ? strerror(errno) : "allowed");
    return 0;
  }
  if (!strcmp(test, "exact")) { // N branches per OUT exit: is the count exactly N each time?
    uint32_t n = strtoul(argv[5], 0, 0);
    int reps = atoi(argv[6]);
    setup(n);
    open_counter(0, 1);
    uint64_t prev = count();
    int bad = 0;
    double t0 = now();
    for (int i = 0; i < reps; i++) {
      if (kvm_run() < 0) die("run");
      if (run->exit_reason != KVM_EXIT_IO) {
        printf("exit %d\n", run->exit_reason);
        return 1;
      }
      uint64_t c = count();
      if (c - prev != n) {
        bad++;
        printf("rep %d: %llu (diff %lld)\n", i, (unsigned long long)(c - prev), (long long)(c - prev - n));
      }
      prev = c;
    }
    printf("exact: n=%u reps=%d wrong=%d time=%.3fs\n", n, reps, bad, now() - t0);
    return 0;
  }
  if (!strcmp(test, "exits")) { // cost of an exit with and without the counter
    int reps = atoi(argv[5]);
    int with = atoi(argv[6]);
    setup(1);
    if (with) open_counter(0, 1);
    double t0 = now();
    for (int i = 0; i < reps; i++)
      if (kvm_run() < 0) die("run");
    double t = now() - t0;
    printf("exits: %d in %.3fs, %.0f ns each, counter %s\n", reps, t, t / reps * 1e9, with ? "on" : "off");
    return 0;
  }
  if (!strcmp(test, "skid") || !strcmp(test, "step")) {
    uint64_t target = strtoull(argv[5], 0, 0);
    uint64_t margin = !strcmp(test, "step") ? strtoull(argv[6], 0, 0) : 0;
    int reps = atoi(argv[!strcmp(test, "step") ? 7 : 6]);
    uint32_t n = 0xffffffffu;
    long long mins = 1LL << 60, maxs = -(1LL << 60);
    double tstep = 0, tint = 0;
    long steps_total = 0, steps_max = 0;
    int bad = 0, over = 0;
    for (int i = 0; i < reps; i++) {
      setup(n);
      open_counter(target - margin, 1);
      double t0 = now();
      int r = kvm_run();
      // first run: exit at the OUT? no: the loop is 4G branches long; expect EINTR
      if (!(r < 0 && errno == EINTR)) {
        printf("rep %d: run returned %d exit %d\n", i, r, run->exit_reason);
        return 1;
      }
      double t1 = now();
      tint += t1 - t0;
      uint64_t c = count();
      long long sk = (long long)c - (long long)(target - margin);
      if (sk < mins) mins = sk;
      if (sk > maxs) maxs = sk;
      uint32_t e = ecx();
      if ((uint64_t)(n - e) != c && (uint64_t)(n - e) + 0 != c) {
        // ecx counts jnz executed: after k jnz, ecx = n - k (dec runs before jnz)
        // between dec and jnz ecx is one ahead; accept both
        if ((uint64_t)(n - e) != c + 1) bad++;
      }
      if (!strcmp(test, "step")) {
        if (c > target) {
          over++;
        } else {
          struct kvm_guest_debug dbg = {.control = KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP};
          if (ioctl(vcpu, KVM_SET_GUEST_DEBUG, &dbg) < 0) die("debug");
          long k = 0;
          // disarm the overflow while stepping
          ioctl(pfd, PERF_EVENT_IOC_PERIOD, &(uint64_t){1ULL << 62});
          while (c < target) {
            if (kvm_run() < 0) die("step");
            if (run->exit_reason != KVM_EXIT_DEBUG) {
              printf("step exit %d\n", run->exit_reason);
              return 1;
            }
            c = count();
            k++;
          }
          struct kvm_guest_debug off = {0};
          ioctl(vcpu, KVM_SET_GUEST_DEBUG, &off);
          tstep += now() - t1;
          steps_total += k;
          if (k > steps_max) steps_max = k;
          struct kvm_regs g;
          ioctl(vcpu, KVM_GET_REGS, &g);
          // the deterministic point: right after the target-th jnz: rip at dec (0x1005), ecx = n - target
          if (c != target || (uint32_t)g.rcx != (uint32_t)(n - target) || g.rip != 0x1005) {
            printf("rep %d: c=%llu ecx=%u want %u rip=%llx\n", i, (unsigned long long)c, (uint32_t)g.rcx,
                   (uint32_t)(n - target), (unsigned long long)g.rip);
            bad++;
          }
        }
      }
      close(pfd);
      munmap(mem, 1 << 21);
      close(vcpu);
      close(vm);
      close(kvm);
    }
    printf("%s: target=%llu margin=%llu reps=%d skid(branches past overflow) min=%lld max=%lld "
           "inconsistent=%d overshoot=%d avg time to interrupt %.3f ms",
           test, (unsigned long long)target, (unsigned long long)margin, reps, mins, maxs, bad, over,
           tint / reps * 1e3);
    if (!strcmp(test, "step"))
      printf(" steps avg %.1f max %ld, stepping %.3f ms avg (%.0f ns per step)", (double)steps_total / reps,
             steps_max, tstep / reps * 1e3, steps_total ? tstep / steps_total * 1e9 : 0);
    printf("\n");
    return 0;
  }
  fprintf(stderr, "unknown test\n");
  return 2;
}

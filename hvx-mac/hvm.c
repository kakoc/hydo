/*
 * hvm.c -- a small deterministic VMM for Apple Silicon (Hypervisor.framework).
 *
 * Boots an arm64 Linux `Image` + initramfs on one vCPU at EL1 with a generated
 * device tree, and makes the run a pure function of its inputs, the way hvx
 * does on Intel (CLAUDE.md M4/M14-M16):
 *
 *  - Time is an EXIT-COUNT CLOCK. Every VM exit advances virtual time by a fixed
 *    step; the guest only sees time through trapped reads of CNTPCT_EL0 and the
 *    trapped physical-timer registers CNTP_{CTL,CVAL,TVAL}_EL0, which we emulate.
 *    The virtual counter CNTVCT_EL0 cannot be trapped on this hardware, so the
 *    guest kernel is built with CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT (linux/apply-cntpct.py),
 *    which makes it use the physical counter and trap EL0 virtual-counter reads.
 *  - Interrupts reach the guest only at exits: we emulate a GICv2 (distributor +
 *    CPU interface, all MMIO) and drive the vCPU's IRQ line with
 *    hv_vcpu_set_pending_interrupt before every hv_vcpu_run. Host interrupts never
 *    surface (probe 1b), so every exit happens at a deterministic instruction.
 *  - Devices: a 16550 UART (console capture) and PSCI over HVC.
 *  - WFI exits; an idle guest's clock jumps to the timer deadline (hvx's HLT rule).
 *
 * Usage:
 *   hvm run    <Image> <initrd> [end-marker]           boot, print console, stop at marker
 *   hvm print  <Image> <initrd> <end-marker>           boot to marker, print the fingerprint
 *   hvm branch <Image> <initrd> <mid-marker> <end-marker>
 *              boot to mid, snapshot, finish (A); restore into a fresh vCPU, finish (B)
 * Devices: virtio-mmio block + network with hvx's fault injection (virtio.h: HVM_DISK, HVM_FAULT, HVM_NET, ...).
 * Env: HVM_MEM_MB (default 256), HVM_STEP (ticks per exit, default 64),
 *      HVM_ECHO=1 (echo the console live), HVM_MAX_EXITS (default 2e8),
 *      HVM_TRACE_SYSREG=1 (log unknown sysreg traps).
 */
#include <Hypervisor/Hypervisor.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <pthread.h>
#include "virtio.h"

#define CHK(x) do { hv_return_t _r = (x); if (_r) { fprintf(stderr, "FAIL %s = 0x%x\n", #x, _r); exit(1); } } while (0)

/* ---------------- guest physical layout ---------------- */
#define RAM_BASE   0x40000000ULL
#define KERNEL_IPA (RAM_BASE + 0x200000)
#define DTB_IPA    (RAM_BASE + 0x08000000)
#define INITRD_IPA (RAM_BASE + 0x08200000)
#define GICD_BASE  0x08000000ULL
#define GICC_BASE  0x08010000ULL
#define UART_BASE  0x09000000ULL
#define TIMER_PPI  14              /* non-secure physical timer: INTID 30 */
#define TIMER_INTID (16 + TIMER_PPI)
#define UART_SPI   1               /* INTID 33 */
#define UART_INTID (32 + UART_SPI)
#define NIRQ       64
#define CNTFRQ     24000000ULL     /* what the guest reads from CNTFRQ_EL0 (host value, constant) */

static uint8_t *ram;
extern FILE *trace_f;
static uint64_t ram_size;
static hv_vcpu_t vcpu;
static hv_vcpu_exit_t *ex;

/* ---------------- deterministic machine state (everything here is in the snapshot) ---------------- */
struct gic {
	uint32_t dctlr, cctlr, pmr, bpr;
	uint32_t enabled[NIRQ / 32], pending[NIRQ / 32], active[NIRQ / 32], cfg[NIRQ / 16], group[NIRQ / 32];
	uint8_t prio[NIRQ], targets[NIRQ];
};
struct uart { uint8_t ier, lcr, mcr, scr, dll, dlm, fcr; };
struct mach {
	uint64_t vtime;              /* virtual counter, CNTFRQ ticks */
	uint64_t exits;              /* all exits taken */
	uint64_t cntp_ctl, cntp_cval;
	struct gic gic;
	struct uart uart;
	uint64_t console_len;
	uint32_t halted;             /* PSCI SYSTEM_OFF/RESET */
	uint32_t stepping;           /* single-stepping to the instruction where the guest unmasks IRQs */
	uint64_t mdscr_saved;
	uint64_t n_wfi, n_wfi_jump, n_cntpct, n_mmio, n_hvc, n_sysreg_other, n_irq_line, n_steps, n_step_runs;
};
static struct mach M;
static uint64_t STEP = 64;

#define CONSOLE_MAX (1 << 22)
static char console[CONSOLE_MAX];
static int echo;

/* ---------------- helpers ---------------- */
static uint64_t rd(hv_reg_t r) { uint64_t v; CHK(hv_vcpu_get_reg(vcpu, r, &v)); return v; }
static void wr(hv_reg_t r, uint64_t v) { CHK(hv_vcpu_set_reg(vcpu, r, v)); }
static uint64_t xget(int n) { return n == 31 ? 0 : rd(HV_REG_X0 + n); }
static void xset(int n, uint64_t v) { if (n != 31) wr(HV_REG_X0 + n, v); }
static void skip(void) { wr(HV_REG_PC, rd(HV_REG_PC) + 4); }
static double now_s(void) { static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb);
	return (double)mach_absolute_time() * tb.numer / tb.denom / 1e9; }

static void *load_file(const char *p, size_t *n)
{
	int fd = open(p, O_RDONLY); if (fd < 0) { perror(p); exit(1); }
	struct stat st; fstat(fd, &st); *n = st.st_size;
	void *b = malloc(*n); if (read(fd, b, *n) != (ssize_t)*n) { perror("read"); exit(1); }
	close(fd); return b;
}

/* ---------------- flattened device tree writer ---------------- */
static uint8_t fdt_struct[16384], fdt_str[4096];
static size_t fs_n, fstr_n;
static void be32p(uint8_t *p, uint32_t v) { p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v; }
static void s32(uint32_t v) { be32p(fdt_struct + fs_n, v); fs_n += 4; }
static void sbytes(const void *b, size_t n) { memcpy(fdt_struct + fs_n, b, n); fs_n += n; while (fs_n & 3) fdt_struct[fs_n++] = 0; }
static uint32_t stroff(const char *s)
{
	for (size_t i = 0; i < fstr_n; i += strlen((char *)fdt_str + i) + 1)
		if (!strcmp((char *)fdt_str + i, s)) return i;
	uint32_t o = fstr_n; memcpy(fdt_str + fstr_n, s, strlen(s) + 1); fstr_n += strlen(s) + 1; return o;
}
static void node(const char *n) { s32(1); sbytes(n, strlen(n) + 1); }
static void endnode(void) { s32(2); }
static void prop(const char *n, const void *v, size_t len) { s32(3); s32(len); s32(stroff(n)); sbytes(v, len); }
static void prop_str(const char *n, const char *v) { prop(n, v, strlen(v) + 1); }
static void prop_u32s(const char *n, const uint32_t *v, int k)
{ uint8_t b[256]; for (int i = 0; i < k; i++) be32p(b + 4 * i, v[i]); prop(n, b, 4 * k); }
static void prop_u32(const char *n, uint32_t v) { prop_u32s(n, &v, 1); }
static void prop_u64(const char *n, uint64_t v) { uint32_t w[2] = { v >> 32, (uint32_t)v }; prop_u32s(n, w, 2); }
static void prop_empty(const char *n) { prop(n, NULL, 0); }

static size_t build_fdt(uint8_t *out, const char *bootargs, uint64_t initrd_start, uint64_t initrd_end)
{
	fs_n = fstr_n = 0;
	node("");
	prop_u32("#address-cells", 2); prop_u32("#size-cells", 2);
	prop_str("compatible", "linux,dummy-virt"); prop_str("model", "hydo-hvm");
	prop_u32("interrupt-parent", 1);
	node("chosen");
	prop_str("bootargs", bootargs);
	prop_str("stdout-path", "/uart@9000000");
	prop_u64("linux,initrd-start", initrd_start); prop_u64("linux,initrd-end", initrd_end);
	{ uint8_t seed[64]; for (int i = 0; i < 64; i++) seed[i] = (uint8_t)(0x5a + 7 * i); prop("rng-seed", seed, 64); }
	endnode();
	node("memory@40000000");
	prop_str("device_type", "memory");
	{ uint32_t r[4] = { 0, (uint32_t)RAM_BASE, (uint32_t)(ram_size >> 32), (uint32_t)ram_size }; prop_u32s("reg", r, 4); }
	endnode();
	node("cpus");
	prop_u32("#address-cells", 1); prop_u32("#size-cells", 0);
	node("cpu@0");
	prop_str("device_type", "cpu"); prop_str("compatible", "arm,arm-v8"); prop_u32("reg", 0); prop_str("enable-method", "psci");
	endnode();
	endnode();
	node("psci"); prop_str("compatible", "arm,psci-0.2"); prop_str("method", "hvc"); endnode();
	node("intc@8000000");
	prop_str("compatible", "arm,cortex-a15-gic"); prop_u32("#interrupt-cells", 3); prop_empty("interrupt-controller");
	{ uint32_t r[8] = { 0, (uint32_t)GICD_BASE, 0, 0x1000, 0, (uint32_t)GICC_BASE, 0, 0x2000 }; prop_u32s("reg", r, 8); }
	prop_u32("phandle", 1);
	endnode();
	node("timer");
	prop_str("compatible", "arm,armv8-timer");
	/* only the secure and non-secure PHYSICAL timer PPIs: no virtual PPI, so Linux picks the physical timer */
	{ uint32_t i[6] = { 1, 13, 0x104, 1, TIMER_PPI, 0x104 }; prop_u32s("interrupts", i, 6); }
	prop_empty("always-on");
	endnode();
	node("uart@9000000");
	prop_str("compatible", "ns16550a");
	{ uint32_t r[4] = { 0, (uint32_t)UART_BASE, 0, 0x100 }; prop_u32s("reg", r, 4); }
	prop_u32("clock-frequency", 1843200);
	{ uint32_t i[3] = { 0, UART_SPI, 4 }; prop_u32s("interrupts", i, 3); }
	endnode();
	for (int d = 0; d < virtio_ndev(); d++) {          /* virtio-mmio devices (none unless attached) */
		uint64_t b; int spi; char nm[40];
		virtio_dev_info(d, &b, &spi);
		snprintf(nm, sizeof nm, "virtio_mmio@%llx", (unsigned long long)b);
		node(nm);
		prop_str("compatible", "virtio,mmio");
		{ uint32_t r[4] = { 0, (uint32_t)b, 0, VIRTIO_MMIO_SIZE }; prop_u32s("reg", r, 4); }
		{ uint32_t i[3] = { 0, (uint32_t)spi, 4 }; prop_u32s("interrupts", i, 3); }
		prop_empty("dma-coherent");
		endnode();
	}
	endnode();
	s32(9);

	size_t off_rsv = 40, off_struct = off_rsv + 16, off_str = off_struct + fs_n, total = off_str + fstr_n;
	memset(out, 0, total);
	be32p(out + 0, 0xd00dfeed); be32p(out + 4, total); be32p(out + 8, off_struct); be32p(out + 12, off_str);
	be32p(out + 16, off_rsv); be32p(out + 20, 17); be32p(out + 24, 16); be32p(out + 28, 0);
	be32p(out + 32, fstr_n); be32p(out + 36, fs_n);
	memcpy(out + off_struct, fdt_struct, fs_n); memcpy(out + off_str, fdt_str, fstr_n);
	return total;
}

/* ---------------- GICv2 ---------------- */
static int bit(const uint32_t *a, int i) { return (a[i / 32] >> (i % 32)) & 1; }
static void setb(uint32_t *a, int i, int v) { if (v) a[i / 32] |= 1u << (i % 32); else a[i / 32] &= ~(1u << (i % 32)); }

/*
 * Seeded tick jitter (hvx's HVX_SET_TJITTER, CLAUDE.md M43), counted in exits. The guest
 * switches threads at timer interrupts, and they can only be taken at an exit, so the knob is
 * which exit: the timer interrupt of a deadline in tick period k (k = cval / TJ_PERIOD, the
 * guest's 4 ms tick) is raised d(k) exits LATER than the deadline, never earlier; the counter
 * the guest reads is untouched. d(k) is hvx's formula (explore.py tj_delays replicates it):
 * h = mix(seed + k*GOLDEN), delayed if h % 1000 < permille, d = 1 + (h >> 16) % max_delay,
 * optionally only for ticks in [lo, hi), or an explicit list of (tick, delay). The policy is
 * host-side, outside struct mach, so it is in neither the snapshot nor the fingerprint: set it
 * after a restore, as on hvx. Off by default, and then nothing below changes.
 */
#define TJ_PERIOD (CNTFRQ / 250)            /* CONFIG_HZ_250 */
#define TJ_MAX_EXPLICIT 32
struct tjit {
	int on; uint64_t seed; uint32_t permille, max_delay; uint64_t lo, hi;
	int n_explicit; uint64_t ek[TJ_MAX_EXPLICIT]; uint32_t ed[TJ_MAX_EXPLICIT];
	uint64_t applied, last_applied_k;         /* statistics: delays that were actually taken */
};
static struct tjit TJ;
static uint64_t tj_mix(uint64_t x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}
static uint64_t tj_delay(uint64_t k)         /* in exits */
{
	uint64_t d = 0;
	if (!TJ.on || k <= 1) return 0;
	if (TJ.n_explicit) {
		for (int i = 0; i < TJ.n_explicit; i++) if (TJ.ek[i] == k) d = TJ.ed[i];
	} else {
		if (k < TJ.lo || (TJ.hi && k >= TJ.hi)) return 0;
		uint64_t h = tj_mix(TJ.seed + k * 0x9e3779b97f4a7c15ULL);
		if (h % 1000 >= TJ.permille) return 0;
		d = 1 + (h >> 16) % TJ.max_delay;
	}
	uint64_t cap = TJ_PERIOD / STEP - 1;        /* stay inside the tick period */
	return d < cap ? d : cap;
}
/* the virtual time at which the timer interrupt is raised: the deadline, plus the delay */
static uint64_t timer_due(void)
{
	if (!TJ.on) return M.cntp_cval;
	return M.cntp_cval + tj_delay(M.cntp_cval / TJ_PERIOD) * STEP;
}
static int timer_level(void)
{
	int en = M.cntp_ctl & 1, mask = (M.cntp_ctl >> 1) & 1;
	if (!TJ.on) return en && !mask && M.vtime >= M.cntp_cval;
	int lv = en && !mask && M.vtime >= timer_due();
	if (lv && M.vtime >= M.cntp_cval && tj_delay(M.cntp_cval / TJ_PERIOD)) {
		uint64_t k = M.cntp_cval / TJ_PERIOD;
		if (k != TJ.last_applied_k) { TJ.applied++; TJ.last_applied_k = k; }
	}
	return lv;
}
static int uart_level(void);
/* level-triggered sources feed the pending bit continuously */
static void gic_update_levels(void)
{
	setb(M.gic.pending, TIMER_INTID, timer_level());
	setb(M.gic.pending, UART_INTID, uart_level());
	virtio_poll(M.vtime);                              /* completions that are due become visible */
	for (int d = 0; d < virtio_ndev(); d++) { uint64_t b; int spi; virtio_dev_info(d, &b, &spi); setb(M.gic.pending, 32 + spi, virtio_level(d)); }
}
/* the highest-priority pending, enabled, inactive interrupt, or 1023 */
static int gic_best(void)
{
	int best = 1023, bp = 256;
	for (int i = 0; i < NIRQ; i++)
		if (bit(M.gic.pending, i) && bit(M.gic.enabled, i) && !bit(M.gic.active, i) && M.gic.prio[i] < bp) { best = i; bp = M.gic.prio[i]; }
	if (best != 1023 && bp >= (int)(M.gic.pmr & 0xff)) return 1023;
	return best;
}
static int gic_any_active(void) { for (int i = 0; i < NIRQ / 32; i++) if (M.gic.active[i]) return 1; return 0; }
static int irq_line(void)
{
	gic_update_levels();
	if (!(M.gic.dctlr & 1) || !(M.gic.cctlr & 1)) return 0;
	if (gic_any_active()) return 0;     /* Linux handles one IRQ at a time (non-split EOI) */
	return gic_best() != 1023;
}

static uint32_t gicd_read(uint64_t off, int size)
{
	(void)size;
	if (off == 0x000) return M.gic.dctlr;
	if (off == 0x004) return (NIRQ / 32) - 1;          /* ITLinesNumber; one CPU */
	if (off == 0x008) return 0x0200143b;
	if (off >= 0x080 && off < 0x080 + NIRQ / 8) return M.gic.group[(off - 0x080) / 4];
	if (off >= 0x100 && off < 0x100 + NIRQ / 8) return M.gic.enabled[(off - 0x100) / 4];
	if (off >= 0x180 && off < 0x180 + NIRQ / 8) return M.gic.enabled[(off - 0x180) / 4];
	if (off >= 0x200 && off < 0x200 + NIRQ / 8) { gic_update_levels(); return M.gic.pending[(off - 0x200) / 4]; }
	if (off >= 0x280 && off < 0x280 + NIRQ / 8) { gic_update_levels(); return M.gic.pending[(off - 0x280) / 4]; }
	if (off >= 0x300 && off < 0x300 + NIRQ / 8) return M.gic.active[(off - 0x300) / 4];
	if (off >= 0x380 && off < 0x380 + NIRQ / 8) return M.gic.active[(off - 0x380) / 4];
	if (off >= 0x400 && off < 0x400 + NIRQ) { uint32_t v = 0; for (int k = 0; k < size; k++) v |= (uint32_t)M.gic.prio[off - 0x400 + k] << (8 * k); return v; }
	if (off >= 0x800 && off < 0x800 + NIRQ) {
		uint32_t v = 0;
		for (int k = 0; k < size; k++) { int i = off - 0x800 + k; v |= (uint32_t)(i < 32 ? 1 : M.gic.targets[i]) << (8 * k); }
		return v;
	}
	if (off >= 0xc00 && off < 0xc00 + NIRQ / 4) return M.gic.cfg[(off - 0xc00) / 4];
	return 0;
}
static void gicd_write(uint64_t off, uint32_t v, int size)
{
	if (off == 0x000) { M.gic.dctlr = v & 1; return; }
	if (off >= 0x080 && off < 0x080 + NIRQ / 8) { M.gic.group[(off - 0x080) / 4] = v; return; }
	if (off >= 0x100 && off < 0x100 + NIRQ / 8) { M.gic.enabled[(off - 0x100) / 4] |= v; return; }
	if (off >= 0x180 && off < 0x180 + NIRQ / 8) { M.gic.enabled[(off - 0x180) / 4] &= ~v; return; }
	if (off >= 0x200 && off < 0x200 + NIRQ / 8) { M.gic.pending[(off - 0x200) / 4] |= v; return; }
	if (off >= 0x280 && off < 0x280 + NIRQ / 8) { M.gic.pending[(off - 0x280) / 4] &= ~v; return; }
	if (off >= 0x300 && off < 0x300 + NIRQ / 8) { M.gic.active[(off - 0x300) / 4] |= v; return; }
	if (off >= 0x380 && off < 0x380 + NIRQ / 8) { M.gic.active[(off - 0x380) / 4] &= ~v; return; }
	if (off >= 0x400 && off < 0x400 + NIRQ) { for (int k = 0; k < size; k++) M.gic.prio[off - 0x400 + k] = v >> (8 * k); return; }
	if (off >= 0x800 && off < 0x800 + NIRQ) { for (int k = 0; k < size; k++) M.gic.targets[off - 0x800 + k] = v >> (8 * k); return; }
	if (off >= 0xc00 && off < 0xc00 + NIRQ / 4) { M.gic.cfg[(off - 0xc00) / 4] = v; return; }
	if (off == 0xf00) {                                /* GICD_SGIR: one CPU, so any target that includes us */
		int filter = (v >> 24) & 3, list = (v >> 16) & 0xff, id = v & 0xf;
		if (filter == 2 || (filter == 0 && (list & 1))) setb(M.gic.pending, id, 1);
		return;
	}
}
static uint32_t gicc_read(uint64_t off)
{
	switch (off) {
	case 0x00: return M.gic.cctlr;
	case 0x04: return M.gic.pmr;
	case 0x08: return M.gic.bpr;
	case 0x0c: {                                      /* IAR: acknowledge */
		gic_update_levels();
		int i = gic_best();
		if (i == 1023) return 1023;
		if (trace_f) { uint64_t elr = 0, spsr = 0; hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_ELR_EL1, &elr); hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SPSR_EL1, &spsr);
			fprintf(trace_f, "IAR %d at exit %llu: interrupted pc %llx spsr %llx\n", i, M.exits, elr, spsr); }
		setb(M.gic.active, i, 1);
		setb(M.gic.pending, i, 0);                     /* level sources re-assert from gic_update_levels */
		return i;
	}
	case 0x14: return 0xff;                           /* RPR: idle */
	case 0x18: gic_update_levels(); return gic_best();
	case 0xfc: return 0x0202143b;
	default: return 0;
	}
}
static void gicc_write(uint64_t off, uint32_t v)
{
	switch (off) {
	case 0x00: M.gic.cctlr = v; break;
	case 0x04: M.gic.pmr = v & 0xff; break;
	case 0x08: M.gic.bpr = v; break;
	case 0x10: if ((v & 0x3ff) < NIRQ) setb(M.gic.active, v & 0x3ff, 0); break;  /* EOIR */
	case 0x1000: if ((v & 0x3ff) < NIRQ) setb(M.gic.active, v & 0x3ff, 0); break; /* DIR */
	}
}

/* ---------------- 16550 UART ---------------- */
/* Receive queue: bytes the host types on the console (hvm_put_input). Not in struct mach: it
 * is empty at every snapshot point, and with it empty the UART behaves exactly as before. */
static uint8_t rxq[4096];
static uint32_t rx_head, rx_tail;
static int rx_ready(void) { return rx_head != rx_tail; }
static __attribute__((unused)) void hvm_put_input(const char *s)
{
	for (; *s && rx_tail - rx_head < sizeof rxq; s++) rxq[rx_tail++ % sizeof rxq] = (uint8_t)*s;
}
static int uart_level(void)
{
	return (M.uart.ier & 0x02) != 0 ||                /* THR-empty interrupt while THRI is enabled (TX never full) */
	       ((M.uart.ier & 0x01) && rx_ready());       /* received data available */
}
static uint8_t uart_read(uint64_t off)
{
	int dlab = M.uart.lcr & 0x80;
	switch (off) {
	case 0:                                           /* RBR: the next queued input byte */
		if (dlab) return M.uart.dll;
		return rx_ready() ? rxq[rx_head++ % sizeof rxq] : 0;
	case 1: return dlab ? M.uart.dlm : M.uart.ier;
	case 2: { uint8_t fifo = (M.uart.fcr & 1) ? 0xc0 : 0;
		if ((M.uart.ier & 0x01) && rx_ready()) return fifo | 0x04;   /* received data available */
		return fifo | ((M.uart.ier & 0x02) ? 0x02 : 0x01); }
	case 3: return M.uart.lcr;
	case 4: return M.uart.mcr;
	case 5: return 0x60 | (rx_ready() ? 0x01 : 0);    /* THRE | TEMT, DR while input is queued */
	case 6:
		if (M.uart.mcr & 0x10) {                       /* loopback: modem outputs reflected */
			uint8_t m = M.uart.mcr;
			return ((m & 2) << 3) | ((m & 1) << 5) | ((m & 4) << 4) | ((m & 8) << 4);
		}
		return 0xb0;                                   /* DCD | DSR | CTS */
	case 7: return M.uart.scr;
	}
	return 0;
}
static void uart_write(uint64_t off, uint8_t v)
{
	int dlab = M.uart.lcr & 0x80;
	switch (off) {
	case 0:
		if (dlab) { M.uart.dll = v; break; }
		if (M.console_len < CONSOLE_MAX) console[M.console_len++] = v;
		if (echo) { fputc(v, stdout); if (v == '\n') fflush(stdout); }
		break;
	case 1: if (dlab) M.uart.dlm = v; else M.uart.ier = v & 0x0f; break;
	case 2: M.uart.fcr = v; break;
	case 3: M.uart.lcr = v; break;
	case 4: M.uart.mcr = v; break;
	case 7: M.uart.scr = v; break;
	}
}

/* ---------------- exit handlers ---------------- */
static uint64_t esr(void) { return ex->exception.syndrome; }

static void handle_mmio(void)
{
	uint64_t s = esr(), ipa = ex->exception.physical_address;
	if (!((s >> 24) & 1)) { fprintf(stderr, "hvm: MMIO abort without ISV at ipa 0x%llx pc 0x%llx\n", ipa, rd(HV_REG_PC)); exit(3); }
	int size = 1 << ((s >> 22) & 3), rt = (s >> 16) & 31, write = (s >> 6) & 1;
	M.n_mmio++;
	if (ipa >= GICD_BASE && ipa < GICD_BASE + 0x1000) {
		if (write) gicd_write(ipa - GICD_BASE, (uint32_t)xget(rt), size); else xset(rt, gicd_read(ipa - GICD_BASE, size));
	} else if (ipa >= GICC_BASE && ipa < GICC_BASE + 0x2000) {
		if (write) gicc_write(ipa - GICC_BASE, (uint32_t)xget(rt)); else xset(rt, gicc_read(ipa - GICC_BASE));
	} else if (ipa >= UART_BASE && ipa < UART_BASE + 0x100) {
		if (write) uart_write(ipa - UART_BASE, (uint8_t)xget(rt)); else xset(rt, uart_read(ipa - UART_BASE));
	} else {
		uint64_t v = write ? xget(rt) : 0;
		if (virtio_mmio(ipa, write, size, &v, M.vtime)) { if (!write) xset(rt, v); }
		else if (!write) xset(rt, 0);                 /* unmapped: reads as zero, writes ignored */
	}
	skip();
}

static void handle_sysreg(void)
{
	uint64_t s = esr();
	int op0 = (s >> 20) & 3, op2 = (s >> 17) & 7, op1 = (s >> 14) & 7, crn = (s >> 10) & 15, rt = (s >> 5) & 31, crm = (s >> 1) & 15, rdir = s & 1;
	if (op0 == 3 && op1 == 3 && crn == 14) {
		if (crm == 0 && op2 == 1 && rdir) { M.n_cntpct++; xset(rt, M.vtime); skip(); return; }       /* CNTPCT_EL0 */
		if (crm == 0 && op2 == 0 && rdir) { xset(rt, CNTFRQ); skip(); return; }                      /* CNTFRQ_EL0 */
		if (crm == 2) {
			if (op2 == 1) {                                                                       /* CNTP_CTL_EL0 */
				if (rdir) xset(rt, (M.cntp_ctl & 3) | ((M.vtime >= M.cntp_cval && (M.cntp_ctl & 1)) ? 4 : 0));
				else M.cntp_ctl = xget(rt) & 3;
				skip(); return;
			}
			if (op2 == 2) { if (rdir) xset(rt, M.cntp_cval); else M.cntp_cval = xget(rt); skip(); return; }   /* CVAL */
			if (op2 == 0) {                                                                       /* TVAL */
				if (rdir) xset(rt, (uint32_t)(M.cntp_cval - M.vtime));
				else M.cntp_cval = M.vtime + (int64_t)(int32_t)xget(rt);
				skip(); return;
			}
		}
	}
	M.n_sysreg_other++;
	if (getenv("HVM_TRACE_SYSREG"))
		fprintf(stderr, "hvm: sysreg %s op0=%d op1=%d CRn=%d CRm=%d op2=%d x%d pc=0x%llx -> RAZ/WI\n",
		        rdir ? "read" : "write", op0, op1, crn, crm, op2, rt, rd(HV_REG_PC));
	if (rdir) xset(rt, 0);
	skip();
}

static int idle_wait(void);
/* Returns 1 when the guest idles with nothing armed (like handle_wfi). */
static int handle_hvc(void)
{
	uint64_t fn = rd(HV_REG_X0);
	M.n_hvc++;
	if ((uint32_t)fn == 0xc6000020) {                 /* hydo idle (CONFIG_ARM64_HVM_IDLE_HVC): a WFI that always exits */
		wr(HV_REG_X0, 0);                         /* PC is already past the HVC */
		return idle_wait();
	}
	uint64_t ret = (uint64_t)-1;                      /* NOT_SUPPORTED */
	switch ((uint32_t)fn) {
	case 0x84000000: ret = 0x00000002; break;          /* PSCI_VERSION 0.2 */
	case 0x84000006: ret = 2; break;                   /* MIGRATE_INFO_TYPE: not present */
	case 0x84000008: case 0x84000009: M.halted = (uint32_t)fn; ret = 0; break;   /* SYSTEM_OFF / SYSTEM_RESET */
	case 0x84000001: ret = 0; break;                   /* CPU_SUSPEND: return at once */
	}
	wr(HV_REG_X0, ret);                               /* PC is already past the HVC */
	return 0;
}

/*
 * Interrupt delivery rule. Hypervisor.framework delivers a pending IRQ that the
 * guest has MASKED at entry at a host-timing-dependent instruction after the guest
 * unmasks it (measured: the interrupted PC differed between runs while every exit
 * matched). So the line is raised only at an entry where PSTATE.I is clear, where
 * the IRQ is taken at the entry instruction. An idle WFI is executed with IRQs
 * masked (arm64 cpu_do_idle); there we single-step -- exact, deterministic -- to the
 * instruction that unmasks, and raise the line there.
 */
static void start_stepping(void)
{
	if (M.stepping) return;
	M.stepping = 1; M.n_step_runs++;
	hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, &M.mdscr_saved);
}
static void stop_stepping(void)
{
	M.stepping = 0;
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, M.mdscr_saved));
	wr(HV_REG_CPSR, rd(HV_REG_CPSR) & ~(1ULL << 21));
}
static int trap_dbg_applied = -1;                    /* host-side, re-applied after a vCPU is (re)created */
static void apply_stepping(void)
{
	if (trap_dbg_applied != (int)M.stepping) { CHK(hv_vcpu_set_trap_debug_exceptions(vcpu, M.stepping)); trap_dbg_applied = M.stepping; }
	if (M.stepping) {
		CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, M.mdscr_saved | 1 | (1 << 13)));   /* SS | KDE */
		wr(HV_REG_CPSR, rd(HV_REG_CPSR) | (1ULL << 21));                                         /* PSTATE.SS */
	}
}

/* WFI: if nothing can be delivered, jump time to the timer deadline (or stop: idle forever) */
static int handle_wfi(void)
{
	skip();
	return idle_wait();
}
/*
 * The guest waits for an interrupt (a trapped WFI, or the idle HVC). A WFI only traps when it
 * would enter low power: with a host wake-up event pending it completes inside the guest with no
 * exit, so a guest idling by WFI can run extra idle iterations at host-timed points (README.md).
 * A kernel built with CONFIG_ARM64_HVM_IDLE_HVC idles through the HVC instead, which always exits.
 */
static int idle_wait(void)
{
	M.n_wfi++;
	if (irq_line()) { if (rd(HV_REG_CPSR) & 0x80) start_stepping(); return 0; }
	uint64_t next = (M.cntp_ctl & 3) == 1 ? timer_due() : 0;   /* the timer deadline (may be already due) */
	uint64_t vd = virtio_next_due(M.vtime);                 /* a device completion or paced frame */
	if (vd && (!next || vd < next)) next = vd;
	if (next > M.vtime) { M.vtime = next; M.n_wfi_jump++; return 0; }
	if (next) return 0;                                /* already due */
	return 1;                                          /* idle with nothing armed */
}

/* ---------------- vCPU / VM setup ---------------- */
static size_t kernel_len, initrd_len;
static uint8_t *kernel_img, *initrd_img;

static void vm_create(const char *bootargs)
{
	CHK(hv_vm_create(NULL));
	ram = mmap(NULL, ram_size, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
	if (ram == MAP_FAILED) { perror("mmap"); exit(1); }
	CHK(hv_vm_map(ram, RAM_BASE, ram_size, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC));
	memcpy(ram + (KERNEL_IPA - RAM_BASE), kernel_img, kernel_len);
	memcpy(ram + (INITRD_IPA - RAM_BASE), initrd_img, initrd_len);
	build_fdt(ram + (DTB_IPA - RAM_BASE), bootargs, INITRD_IPA, INITRD_IPA + initrd_len);
}
static void vcpu_boot(void)
{
	CHK(hv_vcpu_create(&vcpu, &ex, NULL));
	wr(HV_REG_PC, KERNEL_IPA);
	wr(HV_REG_CPSR, 0x3c5);                            /* EL1h, DAIF masked */
	wr(HV_REG_X0, DTB_IPA); wr(HV_REG_X1, 0); wr(HV_REG_X2, 0); wr(HV_REG_X3, 0);
	memset(&M, 0, sizeof M);
	M.gic.pmr = 0;
}

/*
 * Spin watchdog (diagnostic only). A guest that loops without any exit never
 * returns from hv_vcpu_run: on this hardware there is no counter to interrupt it
 * at a deterministic instruction (PROBE.md). If no exit happens for HVM_SPIN_SECS
 * of host time (default 10), the watchdog kicks the vCPU out with hv_vcpus_exit and
 * the run stops with STOP_SPIN. The stop point is host-timed, so such a run is
 * reported as invalid, never fingerprinted as a result.
 */
static volatile uint64_t wd_exits;
static volatile int wd_armed;
static double spin_secs = 10;
static void *watchdog(void *arg)
{
	(void)arg;
	uint64_t last = ~0ULL; double since = now_s();
	for (;;) {
		usleep(200000);
		if (!wd_armed) { last = ~0ULL; since = now_s(); continue; }
		if (wd_exits != last) { last = wd_exits; since = now_s(); continue; }
		if (now_s() - since > spin_secs) { hv_vcpus_exit(&vcpu, 1); since = now_s(); }
	}
	return NULL;
}

/* ---------------- run loop ---------------- */
enum { STOP_MARKER = 1, STOP_HALT, STOP_IDLE, STOP_LIMIT, STOP_SPIN };
static uint64_t max_exits = 200000000ULL;
FILE *trace_f;      /* HVM_TRACE=<file>: one line per exit (index, EC, PC, CPSR, IRQ line at entry) */

static int (*hvm_stop_hook)(void);                  /* hvx-mac/explore/serve.c: extra stop conditions */

/*
 * HVM_SOFT_IRQ=1: deliver an IRQ by emulating the exception entry in software (like KVM's
 * enter_exception64) instead of raising the vCPU's virtual IRQ line, so the IRQ is taken
 * exactly at the entry PC by construction rather than by the hardware's promptness. The
 * M48 boot gives the same fingerprint either way (81711ada76a4a236), i.e. the hardware
 * line was already taken at the entry there. Off by default; hvx-mac/explore/hvmx turns it on.
 */
static int soft_irq = -1;
static int inject_irq_soft(void)
{
	uint64_t cpsr = rd(HV_REG_CPSR), pc = rd(HV_REG_PC), vbar, sctlr, off;
	if (cpsr & 0x10) return 0;                           /* AArch32: not modelled, use the line */
	switch (cpsr & 0xf) {
	case 0x0: off = 0x480; break;                        /* from EL0 (AArch64): lower EL, IRQ */
	case 0x4: off = 0x080; break;                        /* EL1t: current EL with SP_EL0, IRQ */
	case 0x5: off = 0x280; break;                        /* EL1h: current EL with SP_ELx, IRQ */
	default: return 0;
	}
	CHK(hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_VBAR_EL1, &vbar));
	CHK(hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SCTLR_EL1, &sctlr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_SPSR_EL1, cpsr));
	CHK(hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_ELR_EL1, pc));
	uint64_t n = 0;
	n |= cpsr & (1ULL << 24);                            /* DIT kept */
	n |= cpsr & (1ULL << 22);                            /* PAN kept ... */
	if (!(sctlr & (1ULL << 23))) n |= 1ULL << 22;        /* ... and set when SCTLR_EL1.SPAN is 0 */
	if (sctlr & (1ULL << 44)) n |= 1ULL << 12;           /* SSBS from SCTLR_EL1.DSSBS */
	n |= 0x3c0 | 0x5;                                    /* D A I F masked, EL1h */
	wr(HV_REG_CPSR, n);
	wr(HV_REG_PC, vbar + off);
	return 1;
}
static int run_until(const char *marker)
{
	size_t mlen = marker ? strlen(marker) : 0;
	size_t scan_from = 0;
	for (;;) {
		if (M.halted) return STOP_HALT;
		if (M.exits >= max_exits) return STOP_LIMIT;
		if (hvm_stop_hook && hvm_stop_hook()) return STOP_MARKER;
		if (marker && M.console_len >= mlen) {          /* stop at the first exit after the marker appeared */
			size_t start = scan_from > mlen ? scan_from - mlen : 0;
			if (memmem(console + start, M.console_len - start, marker, mlen)) return STOP_MARKER;
			scan_from = M.console_len;
		}
		int line = irq_line() && !(rd(HV_REG_CPSR) & 0x80);   /* only where the guest takes it at once */
		M.n_irq_line += line;
		if (soft_irq < 0) soft_irq = getenv("HVM_SOFT_IRQ") && atoi(getenv("HVM_SOFT_IRQ"));
		if (line && soft_irq && !M.stepping && inject_irq_soft()) line = 0;
		apply_stepping();
		CHK(hv_vcpu_set_pending_interrupt(vcpu, HV_INTERRUPT_TYPE_IRQ, line));
		wd_armed = 1;
		CHK(hv_vcpu_run(vcpu));
		wd_armed = 0; wd_exits++;
		if (ex->reason == HV_EXIT_REASON_CANCELED) {
			fprintf(stderr, "hvm: SPIN: no VM exit for %.0f s of host time at exit %llu, pc 0x%llx cpsr 0x%llx -- "
			        "the guest loops without exits; this run is not deterministic from here and is invalid\n",
			        spin_secs, M.exits, rd(HV_REG_PC), rd(HV_REG_CPSR));
			return STOP_SPIN;
		}
		M.exits++;
		M.vtime += STEP;
		if (ex->reason != HV_EXIT_REASON_EXCEPTION) {
			fprintf(stderr, "hvm: unexpected exit reason %d after %llu exits (pc 0x%llx)\n", ex->reason, M.exits, rd(HV_REG_PC));
			exit(3);
		}
		uint64_t ec = (esr() >> 26) & 0x3f;
		if (trace_f) {
			static long watch = -2;   /* HVM_TRACE_WATCH=<RAM offset>: log the guest u64 there at every exit */
			if (watch == -2) watch = getenv("HVM_TRACE_WATCH") ? strtol(getenv("HVM_TRACE_WATCH"), 0, 0) : -1;
			if (watch >= 0) fprintf(trace_f, "%llu %llx %llx %llx %d w=%llx\n", M.exits, ec, rd(HV_REG_PC), rd(HV_REG_CPSR), line, *(uint64_t *)(ram + watch));
			else fprintf(trace_f, "%llu %llx %llx %llx %d\n", M.exits, ec, rd(HV_REG_PC), rd(HV_REG_CPSR), line);
		}
		switch (ec) {
		case 0x24: handle_mmio(); break;
		case 0x18: handle_sysreg(); break;
		case 0x16: if (handle_hvc()) return STOP_IDLE; break;
		case 0x01: if (handle_wfi()) return STOP_IDLE; break;
		case 0x32: case 0x33:                          /* software step completed */
			M.n_steps++;
			{ uint32_t insn = 0; uint64_t pc = rd(HV_REG_PC);   /* an exclusive load never completes under single-step */
			  if (pc >= 0xffffffc080000000ULL && pc - 0xffffffc080000000ULL + (KERNEL_IPA - RAM_BASE) + 4 <= ram_size)
				memcpy(&insn, ram + (pc - 0xffffffc080000000ULL) + (KERNEL_IPA - RAM_BASE), 4);
			  if ((insn & 0x3fc00000) == 0x08400000) { fprintf(stderr, "hvm: single-step reached an exclusive load at pc 0x%llx (build the guest with LSE atomics)\n", pc); exit(3); } }
			if (!(rd(HV_REG_CPSR) & 0x80)) stop_stepping();
			break;
		default:
			fprintf(stderr, "hvm: unhandled EC 0x%llx ESR 0x%llx pc 0x%llx far 0x%llx after %llu exits\n",
			        ec, esr(), rd(HV_REG_PC), ex->exception.virtual_address, M.exits);
			exit(3);
		}
	}
}

/* ---------------- snapshot + fingerprint ---------------- */
static const hv_sys_reg_t SR[] = {
	HV_SYS_REG_SCTLR_EL1, HV_SYS_REG_CPACR_EL1, HV_SYS_REG_TTBR0_EL1, HV_SYS_REG_TTBR1_EL1, HV_SYS_REG_TCR_EL1,
	HV_SYS_REG_SPSR_EL1, HV_SYS_REG_ELR_EL1, HV_SYS_REG_SP_EL0, HV_SYS_REG_SP_EL1, HV_SYS_REG_AFSR0_EL1,
	HV_SYS_REG_AFSR1_EL1, HV_SYS_REG_ESR_EL1, HV_SYS_REG_FAR_EL1, HV_SYS_REG_PAR_EL1, HV_SYS_REG_MAIR_EL1,
	HV_SYS_REG_AMAIR_EL1, HV_SYS_REG_VBAR_EL1, HV_SYS_REG_CONTEXTIDR_EL1, HV_SYS_REG_TPIDR_EL1, HV_SYS_REG_CNTKCTL_EL1,
	HV_SYS_REG_CSSELR_EL1, HV_SYS_REG_TPIDR_EL0, HV_SYS_REG_TPIDRRO_EL0, HV_SYS_REG_CNTV_CTL_EL0, HV_SYS_REG_CNTV_CVAL_EL0,
	HV_SYS_REG_MDSCR_EL1, HV_SYS_REG_MDCCINT_EL1, HV_SYS_REG_ACTLR_EL1, HV_SYS_REG_SCXTNUM_EL0, HV_SYS_REG_SCXTNUM_EL1,
	HV_SYS_REG_TPIDR2_EL0, HV_SYS_REG_APIAKEYLO_EL1, HV_SYS_REG_APIAKEYHI_EL1, HV_SYS_REG_APIBKEYLO_EL1,
	HV_SYS_REG_APIBKEYHI_EL1, HV_SYS_REG_APDAKEYLO_EL1, HV_SYS_REG_APDAKEYHI_EL1, HV_SYS_REG_APDBKEYLO_EL1,
	HV_SYS_REG_APDBKEYHI_EL1, HV_SYS_REG_APGAKEYLO_EL1, HV_SYS_REG_APGAKEYHI_EL1,
};
#define NSR (sizeof SR / sizeof SR[0])
struct vsnap { uint64_t x[31], pc, cpsr, fpcr, fpsr, sr[NSR]; hv_simd_fp_uchar16_t q[32]; uint8_t ok[NSR]; };
static void vtake(struct vsnap *s)
{
	memset(s, 0, sizeof *s);
	for (int i = 0; i < 31; i++) s->x[i] = rd(HV_REG_X0 + i);
	s->pc = rd(HV_REG_PC); s->cpsr = rd(HV_REG_CPSR); s->fpcr = rd(HV_REG_FPCR); s->fpsr = rd(HV_REG_FPSR);
	for (int i = 0; i < 32; i++) CHK(hv_vcpu_get_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, &s->q[i]));
	for (size_t i = 0; i < NSR; i++) s->ok[i] = hv_vcpu_get_sys_reg(vcpu, SR[i], &s->sr[i]) == 0;
}
static void vput(const struct vsnap *s)
{
	for (int i = 0; i < 31; i++) wr(HV_REG_X0 + i, s->x[i]);
	wr(HV_REG_PC, s->pc); wr(HV_REG_CPSR, s->cpsr); wr(HV_REG_FPCR, s->fpcr); wr(HV_REG_FPSR, s->fpsr);
	for (int i = 0; i < 32; i++) CHK(hv_vcpu_set_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, s->q[i]));
	for (size_t i = 0; i < NSR; i++) if (s->ok[i]) hv_vcpu_set_sys_reg(vcpu, SR[i], s->sr[i]);
}
static uint64_t fnv(const void *p, size_t n, uint64_t h)
{ const uint8_t *b = p; for (size_t i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; } return h; }

struct parts { uint64_t console, regs, mach, ram, all; };
static struct parts fingerprint(void)
{
	struct parts p;
	struct vsnap s; vtake(&s);
	p.console = fnv(console, M.console_len, 0xcbf29ce484222325ULL);
	uint64_t h = 0xcbf29ce484222325ULL;
	h = fnv(s.x, sizeof s.x, h); h = fnv(&s.pc, 8, h); h = fnv(&s.cpsr, 8, h); h = fnv(&s.fpcr, 8, h); h = fnv(&s.fpsr, 8, h);
	h = fnv(s.q, sizeof s.q, h); h = fnv(s.sr, sizeof s.sr, h); h = fnv(s.ok, sizeof s.ok, h);
	p.regs = h;
	/* machine state minus the pure statistics counters */
	struct mach m = M; m.n_wfi = m.n_wfi_jump = m.n_cntpct = m.n_mmio = m.n_hvc = m.n_sysreg_other = m.n_irq_line = m.n_steps = m.n_step_runs = 0;
	p.mach = fnv(&m, sizeof m, 0xcbf29ce484222325ULL);
	if (virtio_attached()) p.mach = virtio_hash(p.mach);   /* device state, disk image, volatile cache, captured TX */
	p.ram = fnv(ram, ram_size, 0xcbf29ce484222325ULL);
	h = fnv(&p.console, 8, 0xcbf29ce484222325ULL); h = fnv(&p.regs, 8, h); h = fnv(&p.mach, 8, h); h = fnv(&p.ram, 8, h);
	p.all = h;
	return p;
}
/* HVM_DUMP=<prefix>: write <prefix>.regs (text) and <prefix>.ram (raw) to find a divergence */
static void dump_state(const char *prefix)
{
	char path[512];
	struct vsnap s; vtake(&s);
	snprintf(path, sizeof path, "%s.regs", prefix);
	FILE *f = fopen(path, "w");
	for (int i = 0; i < 31; i++) fprintf(f, "x%d %016llx\n", i, s.x[i]);
	fprintf(f, "pc %016llx\ncpsr %016llx\nfpcr %016llx\nfpsr %016llx\n", s.pc, s.cpsr, s.fpcr, s.fpsr);
	for (int i = 0; i < 32; i++) { fprintf(f, "q%d ", i); for (int k = 0; k < 16; k++) fprintf(f, "%02x", ((uint8_t *)&s.q[i])[k]); fprintf(f, "\n"); }
	for (size_t i = 0; i < NSR; i++) fprintf(f, "sr%zu(0x%x) %d %016llx\n", i, SR[i], s.ok[i], s.sr[i]);
	fclose(f);
	snprintf(path, sizeof path, "%s.ram", prefix);
	f = fopen(path, "w"); fwrite(ram, 1, ram_size, f); fclose(f);
}
static void print_fp(const char *tag, struct parts p, int stop, double secs)
{
	if (getenv("HVM_DUMP")) dump_state(getenv("HVM_DUMP"));
	printf("%s: stop=%d exits=%llu vtime=%llu (%.3f s virtual) console=%llu B wall=%.2f s | fp=%016llx (console %016llx regs %016llx mach %016llx ram %016llx)\n",
	       tag, stop, M.exits, M.vtime, (double)M.vtime / CNTFRQ, M.console_len, secs, p.all, p.console, p.regs, p.mach, p.ram);
	printf("%s: cntpct reads %llu, mmio %llu, hvc %llu, wfi %llu (clock jumps %llu), other sysreg traps %llu, entries with IRQ line up %llu, single steps %llu in %llu runs\n",
	       tag, M.n_cntpct, M.n_mmio, M.n_hvc, M.n_wfi, M.n_wfi_jump, M.n_sysreg_other, M.n_irq_line, M.n_steps, M.n_step_runs);
	virtio_stats(tag);
	if (TJ.on) printf("%s: tick jitter: %llu timer interrupts delayed, last at tick %llu\n", tag,
	                  (unsigned long long)TJ.applied, (unsigned long long)TJ.last_applied_k);
	fflush(stdout);
}

static const char *BOOTARGS =
	"console=ttyS0 earlycon=uart8250,mmio,0x09000000 clocksource.arm_arch_timer.evtstrm=0 "
	"nokaslr rdinit=/init loglevel=7 random.trust_bootloader=on";

#ifndef HVM_NO_MAIN
int main(int argc, char **argv)
{
	if (argc < 4) { fprintf(stderr, "usage: hvm run|print|branch <Image> <initrd> [markers...]\n"); return 2; }
	const char *mode = argv[1];
	kernel_img = load_file(argv[2], &kernel_len);
	initrd_img = load_file(argv[3], &initrd_len);
	ram_size = (uint64_t)(getenv("HVM_MEM_MB") ? atoi(getenv("HVM_MEM_MB")) : 256) << 20;
	if (getenv("HVM_STEP")) STEP = strtoull(getenv("HVM_STEP"), 0, 0);
	if (getenv("HVM_MAX_EXITS")) max_exits = strtoull(getenv("HVM_MAX_EXITS"), 0, 0);
	echo = getenv("HVM_ECHO") != NULL;
	if (getenv("HVM_TRACE")) trace_f = fopen(getenv("HVM_TRACE"), "w");
	const char *bootargs = getenv("HVM_BOOTARGS") ? getenv("HVM_BOOTARGS") : BOOTARGS;

	if (getenv("HVM_SPIN_SECS")) spin_secs = atof(getenv("HVM_SPIN_SECS"));
	if (getenv("HVM_TJ")) {   /* HVM_TJ=<hex seed>,<permille>,<max delay in exits>[,<lo>,<hi>]: tick jitter for the whole run */
		unsigned long long sd = 0, lo = 0, hi = 0; unsigned pm = 0, mx = 0;
		sscanf(getenv("HVM_TJ"), "%llx,%u,%u,%llu,%llu", &sd, &pm, &mx, &lo, &hi);
		TJ.seed = sd; TJ.permille = pm; TJ.max_delay = mx; TJ.lo = lo; TJ.hi = hi; TJ.on = pm && mx;
	}
	{ pthread_t t; pthread_create(&t, NULL, watchdog, NULL); }
	virtio_setup();
	vm_create(bootargs);
	virtio_bind(ram, RAM_BASE, ram_size, STEP);
	vcpu_boot();
	double t0 = now_s();

	if (!strcmp(mode, "run") || !strcmp(mode, "print")) {
		const char *end = argc > 4 ? argv[4] : NULL;
		int stop = run_until(end);
		double dt = now_s() - t0;
		if (!strcmp(mode, "run") && !echo) fwrite(console, 1, M.console_len, stdout);
		print_fp(mode, fingerprint(), stop, dt);
		virtio_finish();
		return stop == STOP_MARKER || (!end && stop != STOP_LIMIT) ? 0 : 1;
	}
	if (!strcmp(mode, "branch") && argc >= 6) {
		const char *mid = argv[4], *end = argv[5];
		int s1 = run_until(mid);
		if (s1 != STOP_MARKER) { fprintf(stderr, "branch: mid marker not reached (stop %d)\n", s1); return 1; }
		/* snapshot: vCPU registers, all of RAM, the device/clock state and the console length */
		struct vsnap vs; vtake(&vs);
		uint8_t *ram_copy = malloc(ram_size); memcpy(ram_copy, ram, ram_size);
		struct mach msave = M;
		void *dsave = virtio_save();
		printf("branch: snapshot at exit %llu (vtime %llu, console %llu B)\n", M.exits, M.vtime, M.console_len);
		int sa = run_until(end);
		struct parts a = fingerprint(); print_fp("branch-A (continue)", a, sa, now_s() - t0);
		/* restore into a FRESH vCPU after trashing RAM */
		CHK(hv_vcpu_destroy(vcpu));
		memset(ram, 0xee, ram_size);
		CHK(hv_vcpu_create(&vcpu, &ex, NULL)); trap_dbg_applied = -1;
		memcpy(ram, ram_copy, ram_size); vput(&vs); M = msave; virtio_restore(dsave);
		int sb = run_until(end);
		struct parts b = fingerprint(); print_fp("branch-B (restored)", b, sb, now_s() - t0);
		printf("branch: %s\n", a.all == b.all ? "IDENTICAL" : "DIFFERENT");
		return a.all == b.all ? 0 : 1;
	}
	fprintf(stderr, "bad mode\n");
	return 2;
}
#endif /* HVM_NO_MAIN */

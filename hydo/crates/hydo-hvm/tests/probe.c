/* The C side of tests/c_layout.rs: hv/hvm/hvm.c and virtio.c themselves (no main), printing the
 * Hypervisor.framework constants the port hard-codes, the hash of struct mach and struct vstate
 * filled with known values, and the device tree for two configurations. */
#define HVM_NO_MAIN
#include "hvm.c"
#include "virtio_int.h"

static void hex(const char *tag, const uint8_t *b, size_t n)
{
	printf("%s ", tag);
	for (size_t i = 0; i < n; i++) printf("%02x", b[i]);
	printf("\n");
}
#define P(x) printf("%s %llu\n", #x, (unsigned long long)(x))
int main(void)
{
	P(HV_REG_X0); P(HV_REG_PC); P(HV_REG_FPCR); P(HV_REG_FPSR); P(HV_REG_CPSR);
	P(HV_SIMD_FP_REG_Q0); P(HV_INTERRUPT_TYPE_IRQ); P(HV_EXIT_REASON_CANCELED); P(HV_EXIT_REASON_EXCEPTION);
	printf("exit_size %zu\nexit_exception %zu\n", sizeof(hv_vcpu_exit_t), offsetof(hv_vcpu_exit_t, exception));
	for (size_t i = 0; i < NSR; i++) printf("SR%zu %u\n", i, (unsigned)SR[i]);
	for (size_t i = 0; i < NDR; i++) printf("DR%zu %u\n", i, (unsigned)DR[i]);
	printf("mach_size %zu\nvstate_size %zu\n", sizeof(struct mach), sizeof(struct vstate));

	struct mach m; memset(&m, 0, sizeof m);
	m.vtime = 1; m.exits = 2; m.cntp_ctl = 3; m.cntp_cval = 4;
	m.gic.dctlr = 5; m.gic.cctlr = 6; m.gic.pmr = 7; m.gic.bpr = 8;
	for (int i = 0; i < 2; i++) { m.gic.enabled[i] = 10 + i; m.gic.pending[i] = 20 + i; m.gic.active[i] = 30 + i; m.gic.group[i] = 40 + i; }
	for (int i = 0; i < 4; i++) m.gic.cfg[i] = 50 + i;
	for (int i = 0; i < NIRQ; i++) { m.gic.prio[i] = i; m.gic.targets[i] = 200 - i; }
	m.uart.ier = 1; m.uart.lcr = 2; m.uart.mcr = 3; m.uart.scr = 4; m.uart.dll = 5; m.uart.dlm = 6; m.uart.fcr = 7;
	m.console_len = 99; m.halted = 98; m.stepping = 97; m.mdscr_saved = 96;
	m.n_wfi = 1; m.n_wfi_jump = 2; m.n_cntpct = 3; m.n_mmio = 4; m.n_hvc = 5; m.n_sysreg_other = 6; m.n_irq_line = 7; m.n_steps = 8; m.n_step_runs = 9;
	hex("mach", (const uint8_t *)&m, sizeof m);

	memset(&V, 0, sizeof V);
	V.ndev = 2; V.step = 64;
	for (int d = 0; d < 2; d++) {
		struct vdev *v = &V.dev[d];
		v->type = 1 + d; v->status = 2; v->dfsel = 3; v->drfsel = 4; v->qsel = 5; v->drv_features = 6;
		for (int q = 0; q < 2; q++) { v->q[q].num = 7; v->q[q].ready = 8; v->q[q].desc = 9; v->q[q].avail = 10; v->q[q].used = 11; v->q[q].last_avail = 12; }
		v->isr = 13; v->isr_shown = 14; v->armed = 15; v->due = 16; v->config_gen = 17;
	}
	V.blk.seed = 18; V.blk.classes = 19; V.blk.prob = 20; V.blk.max_delay = 21; V.blk.req_seq = 22; V.blk.kick_seq = 23;
	V.blk.fault_count = 24; V.blk.disk_size = 25; V.blk.n_unsync = 26;
	for (int i = 0; i < 6; i++) V.net.mac[i] = 27 + i;
	V.net.rx_n = 33; V.net.rx_head = 34; V.net.rx_next_at = 35; V.net.pace = 36; V.net.seed = 37; V.net.classes = 38;
	V.net.prob = 39; V.net.rx_seq = 40; V.net.tx_seq = 41; V.net.fault_count = 42; V.net.tx_len = 43;
	hex("vstate", (const uint8_t *)&V, sizeof V);

	static uint8_t buf[65536];
	ram_size = 1ULL << 30;
	memset(&V, 0, sizeof V);
	size_t n = build_fdt(buf, "console=ttyS0 hydo", INITRD_IPA, INITRD_IPA + 12345);
	hex("fdt0", buf, n);
	V.ndev = 2;
	ram_size = 3ULL << 30;
	n = build_fdt(buf, BOOTARGS, INITRD_IPA, INITRD_IPA + 999);
	hex("fdt2", buf, n);
	return 0;
}

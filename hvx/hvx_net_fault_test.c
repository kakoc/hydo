// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_net_fault_test - milestone 24: DETERMINISTIC network fault injection.
 * =============================================================================
 *
 * Fault injection for the network side, symmetric to the M22 disk fault
 * injection: a SEEDED policy perturbs packets on the virtio-net device (drop / delay /
 * reorder / corrupt / duplicate) so that any bug it surfaces replays bit-for-bit.
 * Every fault decision is a PURE FUNCTION of the seed + direction + the per-direction
 * packet index -- never wall-clock time or host state -- so the same seed yields the
 * same fault schedule yields the same result. The policy + PRNG counters ride in the
 * snapshot, so branch/replay reproduces the exact faults.
 *
 * A hand-written guest enumerates the virtio-net PCI function, negotiates VERSION_1 +
 * MAC, sets up the receiveq (0, bound to MSI-X entry 0) and transmitq (1), posts a
 * BATCH of receive buffers + a BATCH of transmit frames, and kicks both queues once.
 * The host pushes N_RX scripted inbound frames (delivered together at the one receiveq
 * kick, since faults are enabled on a non-paced guest) and captures the N_TX outbound
 * frames. This test replicates the module's PRNG EXACTLY, so it predicts the whole
 * schedule (which packets dropped / corrupted / reordered / delayed / duplicated) and
 * checks the observed device behavior + guest RAM against it.
 *
 * Proves:
 *   (a) reproducible under a seed -- the observed RX-delivery + TX-capture outcomes
 *       match the predicted deterministic schedule (corruption position/value, the
 *       used-ring reorder permutation, duplicate copies, dropped frames, the delayed
 *       completion icount), AND two runs with the same seed are byte-identical
 *       (fingerprint over regs + device state incl. the RX FIFO + captured TX +
 *       fault/PRNG state + all guest RAM);
 *   (b) the seed controls the schedule -- a different seed yields a DIFFERENT (but
 *       still reproducible) schedule and fingerprint;
 *   (c) snapshot/branch mid-flight reproduces the fault schedule bit-for-bit;
 *   (d) faults DISABLED (default) => the multi-packet batch is delivered/captured
 *       exactly like M23 (every frame delivered normally, all TX captured, used ring
 *       in submission order), and reproducible.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_net_fault_test hvx_net_fault_test.c
 * Run   (inside L1):  sudo ./hvx_net_fault_test
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
#define GUEST_MEM_BYTES (256UL * 4096)
#define INTR_CTR_GPA    0x2f00UL		/* the ISR increments this (GPA_INTR_COUNT) */
#define ISR_GPA         0x2800UL		/* GPA_ISR: built-in gate 0x20 targets here */
#define VECTOR          0x20			/* RX completion vector (built-in IDT gate) */

/* enumeration scratch */
#define VID_STORE       0x7000UL
#define BAR_STORE       0x7008UL
#define MAC_LO_STORE    0x7010UL
#define MAC_HI_STORE    0x7018UL

/* receiveq (0): descriptor/avail/used rings + a batch of writable receive buffers */
#define RX_DESC_GPA     0x10000UL
#define RX_AVAIL_GPA    0x10800UL
#define RX_USED_GPA     0x11000UL
#define RX_BUF_BASE     0x20000UL
#define RX_BUF_SIZE     0x800UL			/* 2 KiB per receive buffer */
#define RX_BUF(i)       (RX_BUF_BASE + (uint64_t)(i) * RX_BUF_SIZE)

/* transmitq (1): descriptor/avail/used rings + a batch of [hdr][frame] chains */
#define TX_DESC_GPA     0x30000UL
#define TX_AVAIL_GPA    0x30800UL
#define TX_USED_GPA     0x31000UL
#define TX_CH_BASE      0x40000UL
#define TX_CH_SIZE      0x400UL
#define TX_HDR(i)       (TX_CH_BASE + (uint64_t)(i) * TX_CH_SIZE)
#define TX_FRAME(i)     (TX_CH_BASE + (uint64_t)(i) * TX_CH_SIZE + 0x100UL)

#define QSIZE           16			/* driver-created ring size (>= all descriptors) */
#define N_RX            6			/* scripted inbound frames pushed per run */
#define N_TX            6			/* outbound frames posted per run */
#define N_RX_BUFS       12			/* receive buffers posted (2*N_RX, room for DUP) */
#define RX_FRAME_LEN    60
#define TX_FRAME_LEN    60
#define MSIX_ADDR       0xFEE00000U		/* LAPIC MMIO base -> deliver to the BSP */
#define COUNTDOWN       0x400			/* spin length: advance icount past completion */
#define FBATCH          32			/* local cap mirroring HVX_VNET_FAULT_BATCH */

#define HVX_VNET_TX_CAP_USER 16384

static const uint8_t g_mac[6] = { 0x52, 0x54, 0x00, 0x24, 0x24, 0x24 };

/* distinct per-frame content so corruption / duplication / reorder are detectable */
static uint8_t rx_byte(int i, int j) { return (uint8_t)(0x40 + i * 0x11 + (j & 0x1f)); }
static uint8_t tx_byte(int i, int j) { return (uint8_t)(0x90 + i * 0x13 + (j & 0x1f)); }

/* ---- REPLICA of the module's net-fault PRNG (must match hvx.c exactly) ---- */
enum { NF_NONE = 0, NF_DROP, NF_DELAY, NF_CORRUPT, NF_DUP };
#define NF_RX_SALT 0x0052000000000000ULL	/* 'R' << 48 */
#define NF_TX_SALT 0x0054000000000000ULL	/* 'T' << 48 */

static uint64_t fault_mix(uint64_t x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}
static uint64_t net_fault_roll(uint64_t seed, int dir, uint64_t index)
{
	uint64_t s = seed ^ (dir ? NF_TX_SALT : NF_RX_SALT);

	return fault_mix(s + (index + 1) * 0x9e3779b97f4a7c15ULL);
}
static uint64_t net_reorder_roll(uint64_t seed, int dir, uint64_t kick)
{
	uint64_t s = (seed ^ (dir ? NF_TX_SALT : NF_RX_SALT)) ^ 0xD15EA5E5C0FFEE11ULL;

	return fault_mix(s + (kick + 1) * 0x9e3779b97f4a7c15ULL);
}
/* Predict one packet's fault class (mirrors vnet_fault_decide). *cpos returned mod flen. */
static int predict_packet(uint64_t seed, int dir, uint64_t index, uint32_t classes,
			  uint32_t prob, uint32_t maxd, uint32_t flen,
			  uint32_t *extra, uint32_t *cpos, uint8_t *cxor)
{
	uint64_t h = net_fault_roll(seed, dir, index);
	uint32_t pct = (uint32_t)(h % 100);
	int cand[4], n = 0, sel;

	*extra = 0; *cpos = 0; *cxor = 0;
	if (classes == 0)
		return NF_NONE;
	if (pct >= prob)
		return NF_NONE;
	if (classes & HVX_NFAULT_DROP)
		cand[n++] = NF_DROP;
	if ((classes & HVX_NFAULT_DELAY) && dir == 0)
		cand[n++] = NF_DELAY;
	if (classes & HVX_NFAULT_CORRUPT)
		cand[n++] = NF_CORRUPT;
	if (classes & HVX_NFAULT_DUPLICATE)
		cand[n++] = NF_DUP;
	if (n == 0)
		return NF_NONE;
	sel = (int)((h >> 8) % (uint64_t)n);
	if (cand[sel] == NF_DELAY) {
		uint32_t md = maxd ? maxd : 1;

		*extra = 1 + (uint32_t)((h >> 16) % md);
	} else if (cand[sel] == NF_CORRUPT) {
		*cpos = (uint32_t)(h >> 16) % flen;
		*cxor = (uint8_t)((h >> 32) | 1);
	}
	return cand[sel];
}
static void predict_reorder(uint64_t seed, int dir, uint32_t kick, int n, int *order)
{
	uint64_t pr = net_reorder_roll(seed, dir, kick);
	int i;

	for (i = 0; i < n; i++)
		order[i] = i;
	for (i = n - 1; i > 0; i--) {
		int j, t;

		pr = fault_mix(pr);
		j = (int)(pr % (uint64_t)(i + 1));
		t = order[i]; order[i] = order[j]; order[j] = t;
	}
}

/* ---- machine-code emitters (guest is machine-checked, not hand-hexed) ---- */
static size_t movabs(uint8_t *c, size_t n, uint8_t reg_op, uint64_t v)
{
	c[n++] = 0x48; c[n++] = reg_op; memcpy(c + n, &v, 8); n += 8; return n;
}
static size_t mmio_w32(uint8_t *c, size_t n, uint64_t addr, uint32_t val)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0xbe; memcpy(c + n, &val, 4); n += 4;
	c[n++] = 0x89; c[n++] = 0x37;
	return n;
}
static size_t mmio_w16(uint8_t *c, size_t n, uint64_t addr, uint16_t val)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0x66; c[n++] = 0xbe; memcpy(c + n, &val, 2); n += 2;
	c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x37;
	return n;
}
static size_t mmio_r32_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0x8b; c[n++] = 0x37;
	return n;
}
static size_t mmio_wr_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0x89; c[n++] = 0x37;
	return n;
}
static size_t mmio_r32_store(uint8_t *c, size_t n, uint64_t addr, uint64_t store)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0x8b; c[n++] = 0x07;
	n = movabs(c, n, 0xbe, store);
	c[n++] = 0x89; c[n++] = 0x06;
	return n;
}
static size_t mmio_r16_store(uint8_t *c, size_t n, uint64_t addr, uint64_t store)
{
	n = movabs(c, n, 0xbf, addr);
	c[n++] = 0x66; c[n++] = 0x8b; c[n++] = 0x07;
	n = movabs(c, n, 0xbe, store);
	c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x06;
	return n;
}
static size_t pio_out32(uint8_t *c, size_t n, uint32_t val, uint32_t port)
{
	c[n++] = 0xb8; memcpy(c + n, &val, 4); n += 4;
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;
	c[n++] = 0xef;
	return n;
}
static uint32_t cfg_addr(unsigned reg) { return 0x80000000u | (HVX_VNET_PCI_DEV << 11) | (reg & 0xfc); }
static size_t pio_cfg_read_store(uint8_t *c, size_t n, unsigned reg, uint64_t store)
{
	uint32_t port = HVX_PCI_CONFIG_DATA;

	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;
	c[n++] = 0xed;
	n = movabs(c, n, 0xbf, store);
	c[n++] = 0x89; c[n++] = 0x07;
	return n;
}
static size_t pio_cfg_write(uint8_t *c, size_t n, unsigned reg, uint32_t val)
{
	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);
	n = pio_out32(c, n, val, HVX_PCI_CONFIG_DATA);
	return n;
}
static size_t setup_queue(uint8_t *c, size_t n, uint64_t B, unsigned qi,
			  uint64_t desc, uint64_t avail, uint64_t used, unsigned msix)
{
	n = mmio_w16(c, n, B + 0x16, qi);
	n = mmio_w16(c, n, B + 0x18, QSIZE);
	n = mmio_w16(c, n, B + 0x1a, msix);
	n = mmio_w32(c, n, B + 0x20, (uint32_t)desc);
	n = mmio_w32(c, n, B + 0x24, 0);
	n = mmio_w32(c, n, B + 0x28, (uint32_t)avail);
	n = mmio_w32(c, n, B + 0x2c, 0);
	n = mmio_w32(c, n, B + 0x30, (uint32_t)used);
	n = mmio_w32(c, n, B + 0x34, 0);
	n = mmio_w16(c, n, B + 0x1c, 1);
	return n;
}

static size_t build_guest(uint8_t *c)
{
	const uint64_t B = HVX_VNET_BAR;
	size_t n = 0;

	n = pio_cfg_read_store(c, n, 0x00, VID_STORE);
	n = pio_cfg_read_store(c, n, 0x10, BAR_STORE);
	n = pio_cfg_write(c, n, 0x04, 0x6);			/* memory + bus master */
	n = mmio_w32(c, n, B + 0x14, 0);			/* reset device_status */
	n = pio_cfg_write(c, n, HVX_VNET_MSIX_CAP, 0x80000000u);	/* enable MSI-X */
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x0, MSIX_ADDR);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x4, 0);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x8, VECTOR);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0xc, 0);
	n = mmio_w32(c, n, B + 0x14, 1);			/* ACKNOWLEDGE */
	n = mmio_w32(c, n, B + 0x14, 3);			/* |= DRIVER */
	n = mmio_w32(c, n, B + 0x00, 1);			/* dev_feature_select 1 */
	n = mmio_r32_esi(c, n, B + 0x04);
	n = mmio_w32(c, n, B + 0x08, 1);			/* drv_feature_select 1 */
	n = mmio_wr_esi(c, n, B + 0x0c);			/* accept VERSION_1 */
	n = mmio_w32(c, n, B + 0x00, 0);			/* dev_feature_select 0 */
	n = mmio_r32_esi(c, n, B + 0x04);
	n = mmio_w32(c, n, B + 0x08, 0);
	n = mmio_wr_esi(c, n, B + 0x0c);			/* accept MAC */
	n = mmio_w32(c, n, B + 0x14, 0xB);			/* FEATURES_OK */
	n = mmio_r32_store(c, n, B + HVX_VPCI_DEVICE + 0, MAC_LO_STORE);
	n = mmio_r16_store(c, n, B + HVX_VPCI_DEVICE + 4, MAC_HI_STORE);
	n = setup_queue(c, n, B, 0, RX_DESC_GPA, RX_AVAIL_GPA, RX_USED_GPA, 0);
	n = setup_queue(c, n, B, 1, TX_DESC_GPA, TX_AVAIL_GPA, TX_USED_GPA, HVX_MSIX_NO_VECTOR);
	n = mmio_w32(c, n, B + 0x14, 0xF);			/* DRIVER_OK */
	n = mmio_w16(c, n, B + HVX_VPCI_NOTIFY, 1);		/* kick transmitq (capture) */
	n = mmio_w16(c, n, B + HVX_VPCI_NOTIFY, 0);		/* kick receiveq (deliver batch) */
	/* countdown spin: advance icount past the RX completion (which fires the MSI-X
	 * during the spin), then HLT -- terminates regardless of how many frames dropped */
	c[n++] = 0xb9; { uint32_t v = COUNTDOWN; memcpy(c + n, &v, 4); n += 4; }	/* mov ecx, imm32 */
	c[n++] = 0xff; c[n++] = 0xc9;				/* dec ecx */
	c[n++] = 0x75; c[n++] = 0xfc;				/* jnz -4 */
	c[n++] = 0xf4;						/* hlt */
	return n;
}

/* built-in ISR is overwritten with one that EOIs the x2APIC (MSI-X is edge) */
static const uint8_t isr_blob[] = {
	0x50, 0x51, 0x52,				/* push rax; push rcx; push rdx */
	0x48, 0xff, 0x04, 0x25, 0x00, 0x2f, 0x00, 0x00,	/* inc qword [0x2f00] */
	0xb9, 0x0b, 0x08, 0x00, 0x00,			/* mov ecx, 0x80b (x2APIC EOI) */
	0x31, 0xc0, 0x31, 0xd2,				/* xor eax,eax; xor edx,edx */
	0x0f, 0x30,					/* wrmsr */
	0x5a, 0x59, 0x58,				/* pop rdx; pop rcx; pop rax */
	0x48, 0xcf,					/* iretq */
};

struct vqd { uint64_t addr; uint32_t len; uint16_t flags; uint16_t next; };
static void put_desc(void *mem, uint64_t base, unsigned i, uint64_t addr, uint32_t len,
		     uint16_t flags, uint16_t next)
{
	struct vqd d = { addr, len, flags, next };

	memcpy((uint8_t *)mem + base + i * 16, &d, sizeof(d));
}

static void build_queues(void *mem)
{
	uint16_t avail[2 + QSIZE];
	int i, j;

	/* RX: N_RX_BUFS writable receive buffers, avail ring[i]=i (head == descriptor) */
	for (i = 0; i < N_RX_BUFS; i++) {
		memset((uint8_t *)mem + RX_BUF(i), 0, RX_BUF_SIZE);
		put_desc(mem, RX_DESC_GPA, i, RX_BUF(i), (uint32_t)RX_BUF_SIZE, VIRTQ_DESC_F_WRITE, 0);
		avail[2 + i] = i;
	}
	avail[0] = 0; avail[1] = N_RX_BUFS;
	memcpy((uint8_t *)mem + RX_AVAIL_GPA, avail, sizeof(uint16_t) * (2 + N_RX_BUFS));
	memset((uint8_t *)mem + RX_USED_GPA, 0, 8 + 8 * QSIZE);

	/* TX: N_TX chains [12-byte hdr][frame], avail ring[i]=2*i (head of chain i) */
	for (i = 0; i < N_TX; i++) {
		memset((uint8_t *)mem + TX_HDR(i), 0, HVX_VNET_HDR_LEN);
		for (j = 0; j < TX_FRAME_LEN; j++)
			((uint8_t *)mem)[TX_FRAME(i) + j] = tx_byte(i, j);
		put_desc(mem, TX_DESC_GPA, 2 * i + 0, TX_HDR(i), HVX_VNET_HDR_LEN, VIRTQ_DESC_F_NEXT, 2 * i + 1);
		put_desc(mem, TX_DESC_GPA, 2 * i + 1, TX_FRAME(i), TX_FRAME_LEN, 0, 0);
		avail[2 + i] = 2 * i;
	}
	avail[0] = 0; avail[1] = N_TX;
	memcpy((uint8_t *)mem + TX_AVAIL_GPA, avail, sizeof(uint16_t) * (2 + N_TX));
	memset((uint8_t *)mem + TX_USED_GPA, 0, 8 + 8 * QSIZE);
}

/* ---- canonical fingerprint: regs + net device state + fault state + RAM + TX ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;

	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}
static uint64_t fingerprint(int fd, void *mem, uint8_t *txbuf)
{
	struct hvx_snapshot s;
	struct hvx_tx tx = { .len = HVX_VNET_TX_CAP_USER, .data = (uint64_t)(uintptr_t)txbuf };
	uint64_t ram_len = GUEST_MEM_BYTES;
	uint32_t rx_count;
	uint64_t h = 0xcbf29ce484222325ULL;
	int qi;

	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }
	if (ioctl(fd, HVX_GET_TX, &tx) < 0)       { perror("HVX_GET_TX"); return 0; }

	h = fnv(h, "hvx-nfault-v1", 13);
	h = fnv(h, &s.regs, sizeof(s.regs));
	h = fnv(h, &s.icount, sizeof(s.icount));
	h = fnv(h, &s.lapic_in_service, sizeof(s.lapic_in_service));
	h = fnv(h, &s.vnet_dev_status, sizeof(s.vnet_dev_status));
	h = fnv(h, &s.vnet_isr, sizeof(s.vnet_isr));
	for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) {
		h = fnv(h, &s.vnet_qready[qi], sizeof(s.vnet_qready[qi]));
		h = fnv(h, &s.vnet_last_avail[qi], sizeof(s.vnet_last_avail[qi]));
		h = fnv(h, &s.vnet_completion_armed[qi], sizeof(s.vnet_completion_armed[qi]));
		h = fnv(h, &s.vnet_completion_due[qi], sizeof(s.vnet_completion_due[qi]));
		h = fnv(h, &s.vnet_irq_pending[qi], sizeof(s.vnet_irq_pending[qi]));
	}
	rx_count = s.vnet_rx_tail - s.vnet_rx_head;
	h = fnv(h, &rx_count, sizeof(rx_count));
	h = fnv(h, &s.vnet_tx_len, sizeof(s.vnet_tx_len));
	h = fnv(h, &s.vnet_tx_count, sizeof(s.vnet_tx_count));
	/* the net fault-injection policy + PRNG state IS part of the deterministic state */
	h = fnv(h, &s.nfault_seed, sizeof(s.nfault_seed));
	h = fnv(h, &s.nfault_classes, sizeof(s.nfault_classes));
	h = fnv(h, &s.nfault_probability, sizeof(s.nfault_probability));
	h = fnv(h, &s.nfault_max_delay, sizeof(s.nfault_max_delay));
	h = fnv(h, &s.nfault_rx_kick_seq, sizeof(s.nfault_rx_kick_seq));
	h = fnv(h, &s.nfault_tx_kick_seq, sizeof(s.nfault_tx_kick_seq));
	h = fnv(h, &s.nfault_rx_seq, sizeof(s.nfault_rx_seq));
	h = fnv(h, &s.nfault_tx_seq, sizeof(s.nfault_tx_seq));
	h = fnv(h, &s.nfault_count, sizeof(s.nfault_count));
	h = fnv(h, &ram_len, sizeof(ram_len));
	h = fnv(h, mem, GUEST_MEM_BYTES);
	h = fnv(h, &tx.len, sizeof(tx.len));
	h = fnv(h, txbuf, tx.len < HVX_VNET_TX_CAP_USER ? tx.len : HVX_VNET_TX_CAP_USER);
	return h;
}

static int attach(int fd)
{
	struct hvx_net_config nc;

	memset(&nc, 0, sizeof(nc));
	memcpy(nc.mac, g_mac, 6);		/* NO_IDMAP off + PACED off: hand-written guest */
	return ioctl(fd, HVX_NET_ATTACH, &nc) < 0 ? -1 : 0;
}
static int set_fault(int fd, uint64_t seed, uint32_t classes, uint32_t prob, uint32_t maxd)
{
	struct hvx_net_fault f;

	memset(&f, 0, sizeof(f));
	f.seed = seed; f.classes = classes; f.probability = prob; f.max_delay = maxd;
	return ioctl(fd, HVX_SET_NET_FAULT, &f) < 0 ? -1 : 0;
}
static int push_rx_batch(int fd)
{
	struct hvx_rx rx;
	int i, j;

	for (i = 0; i < N_RX; i++) {
		memset(&rx, 0, sizeof(rx));
		rx.len = RX_FRAME_LEN;
		for (j = 0; j < RX_FRAME_LEN; j++)
			rx.buf[j] = rx_byte(i, j);
		if (ioctl(fd, HVX_PUT_RX, &rx) < 0) { perror("HVX_PUT_RX"); return -1; }
	}
	return 0;
}
static int setup(int fd, void *mem)
{
	struct hvx_regs regs;

	if (push_rx_batch(fd) < 0) return -1;
	build_queues(mem);
	memcpy((uint8_t *)mem + ISR_GPA, isr_blob, sizeof(isr_blob));
	*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) = 0;
	memset(&regs, 0, sizeof(regs));
	regs.rip    = HVX_GUEST_CODE_BASE;
	regs.rsp    = GUEST_MEM_BYTES - 16;
	regs.rflags = 0x202;			/* IF=1: required to deliver the MSI-X */
	return ioctl(fd, HVX_SET_REGS, &regs) < 0 ? -1 : 0;
}
static int run_to_halt(int fd, int reset, struct hvx_run_ctl *out)
{
	struct hvx_run_ctl ctl;
	int throttles = 0;

	for (;;) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (reset ? HVX_RUN_RESET_ICOUNT : 0);
		reset = 0;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE) {
			if (++throttles > 128) {
				fprintf(stderr, "hvx_net_fault_test: FAIL: guest never HLTed "
					"(icount=%" PRIu64 ")\n", (uint64_t)ctl.icount);
				*out = ctl;
				return -1;
			}
			continue;
		}
		*out = ctl;
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}

/* Verify the observed RX delivery against the predicted schedule. Returns 1 on match.
 * Fills *n_used (delivered copies) and *batch_extra (summed DELAY). */
static int verify_rx(void *mem, uint64_t seed, uint32_t classes, uint32_t prob, uint32_t maxd,
		     int *n_used_out, uint32_t *batch_extra_out)
{
	int ok = 1, i, next = 0, n_used = 0;
	int used_pre[FBATCH];
	int df[N_RX_BUFS], dpos[N_RX_BUFS];
	uint8_t dxor[N_RX_BUFS];
	uint32_t batch_extra = 0;
	int order[FBATCH];
	uint16_t uidx;

	for (i = 0; i < N_RX_BUFS; i++) { df[i] = -1; dpos[i] = -1; dxor[i] = 0; }

	for (i = 0; i < N_RX; i++) {
		uint32_t extra = 0, cpos = 0;
		uint8_t cxor = 0;
		int cls = predict_packet(seed, 0, i, classes, prob, maxd, RX_FRAME_LEN, &extra, &cpos, &cxor);
		int copies, c;

		if (cls == NF_DROP)
			continue;
		if (cls == NF_DELAY)
			batch_extra += extra;
		copies = (cls == NF_DUP) ? 2 : 1;
		for (c = 0; c < copies; c++) {
			if (next >= N_RX_BUFS)
				break;
			df[next] = i;
			dpos[next] = (cls == NF_CORRUPT) ? (int)cpos : -1;
			dxor[next] = cxor;
			used_pre[n_used++] = next;
			next++;
		}
	}
	if (classes & HVX_NFAULT_REORDER)
		predict_reorder(seed, 0, 0, n_used, order);
	else
		for (i = 0; i < n_used; i++) order[i] = i;

	/* delivered descriptor contents (frame bytes, corruption applied); un-consumed 0 */
	for (i = 0; i < N_RX_BUFS; i++) {
		uint8_t *buf = (uint8_t *)mem + RX_BUF(i);
		int j;

		if (df[i] >= 0) {
			if (buf[10] != 1) ok = 0;			/* num_buffers low byte */
			for (j = 0; j < RX_FRAME_LEN; j++) {
				uint8_t exp = rx_byte(df[i], j);

				if (dpos[i] >= 0 && j == dpos[i]) exp ^= dxor[i];
				if (buf[HVX_VNET_HDR_LEN + j] != exp) ok = 0;
			}
		} else {
			for (j = 0; j < HVX_VNET_HDR_LEN + RX_FRAME_LEN; j++)
				if (buf[j] != 0) { ok = 0; break; }
		}
	}
	/* used-ring completion order == predicted permutation of the descriptor ids */
	memcpy(&uidx, (uint8_t *)mem + RX_USED_GPA + 2, 2);
	if (uidx != (uint16_t)n_used) { ok = 0; printf("    rx used->idx=%u (expected %d)\n", uidx, n_used); }
	printf("    rx completion order (used-ring id): ");
	for (i = 0; i < n_used; i++) {
		uint32_t id;

		memcpy(&id, (uint8_t *)mem + RX_USED_GPA + 4 + 8 * i, 4);
		printf("%u ", id);
		if (id != (uint32_t)used_pre[order[i]]) ok = 0;
	}
	printf("(predicted: ");
	for (i = 0; i < n_used; i++) printf("%d ", used_pre[order[i]]);
	printf(")\n");

	*n_used_out = n_used;
	*batch_extra_out = batch_extra;
	return ok;
}

/* Verify the captured TX (submission order) + TX used-ring reorder. Returns 1 on match. */
static int verify_tx(uint8_t *txbuf, uint32_t txlen, uint32_t txcount, void *mem,
		     uint64_t seed, uint32_t classes, uint32_t prob, uint32_t maxd)
{
	int ok = 1, i, n_used = 0;
	uint8_t expbuf[N_TX * TX_FRAME_LEN * 2];
	uint32_t explen = 0, expcount = 0;
	int used_pre[FBATCH], order[FBATCH];
	uint16_t uidx;

	for (i = 0; i < N_TX; i++) {
		uint32_t extra = 0, cpos = 0;
		uint8_t cxor = 0;
		int cls = predict_packet(seed, 1, i, classes, prob, maxd, TX_FRAME_LEN, &extra, &cpos, &cxor);
		uint8_t frame[TX_FRAME_LEN];
		int copies, c, j;

		for (j = 0; j < TX_FRAME_LEN; j++) frame[j] = tx_byte(i, j);
		if (cls == NF_CORRUPT) frame[cpos] ^= cxor;
		copies = (cls == NF_DROP) ? 0 : (cls == NF_DUP ? 2 : 1);
		for (c = 0; c < copies; c++) {
			memcpy(expbuf + explen, frame, TX_FRAME_LEN);
			explen += TX_FRAME_LEN;
			expcount++;
		}
		used_pre[n_used++] = 2 * i;
	}
	if (classes & HVX_NFAULT_REORDER)
		predict_reorder(seed, 1, 0, n_used, order);
	else
		for (i = 0; i < n_used; i++) order[i] = i;

	if (txlen != explen) { ok = 0; printf("    tx len=%u (expected %u)\n", txlen, explen); }
	if (txcount != expcount) { ok = 0; printf("    tx count=%u (expected %u)\n", txcount, expcount); }
	if (explen && memcmp(txbuf, expbuf, explen) != 0) { ok = 0; printf("    tx bytes MISMATCH\n"); }

	memcpy(&uidx, (uint8_t *)mem + TX_USED_GPA + 2, 2);
	if (uidx != (uint16_t)n_used) { ok = 0; printf("    tx used->idx=%u (expected %d)\n", uidx, n_used); }
	printf("    tx completion order (used-ring id): ");
	for (i = 0; i < n_used; i++) {
		uint32_t id;

		memcpy(&id, (uint8_t *)mem + TX_USED_GPA + 4 + 8 * i, 4);
		printf("%u ", id);
		if (id != (uint32_t)used_pre[order[i]]) ok = 0;
	}
	printf("(predicted: ");
	for (i = 0; i < n_used; i++) printf("%d ", used_pre[order[i]]);
	printf(")\n");
	return ok;
}

/* Run one full scenario to HLT and verify the schedule. Returns fingerprint (0 on fail). */
static uint64_t run_scenario(int fd, void *mem, uint8_t *txbuf,
			     uint64_t seed, uint32_t classes, uint32_t prob, uint32_t maxd, int *ok_out)
{
	struct hvx_run_ctl r;
	struct hvx_tx tx = { .len = HVX_VNET_TX_CAP_USER, .data = (uint64_t)(uintptr_t)txbuf };
	int okr, okt, n_used = 0;
	uint32_t batch_extra = 0;
	uint64_t intr;

	*ok_out = 0;
	if (setup(fd, mem) < 0) return 0;
	if (set_fault(fd, seed, classes, prob, maxd) < 0) { perror("HVX_SET_NET_FAULT"); return 0; }
	if (run_to_halt(fd, 1, &r) < 0) return 0;
	if (ioctl(fd, HVX_GET_TX, &tx) < 0) { perror("HVX_GET_TX"); return 0; }

	okr = verify_rx(mem, seed, classes, prob, maxd, &n_used, &batch_extra);
	okt = verify_tx(txbuf, (uint32_t)tx.len, (uint32_t)tx.count, mem, seed, classes, prob, maxd);
	intr = *(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA);
	/* the RX completion arms iff at least one frame delivered -> exactly one MSI-X */
	if (intr != (uint64_t)(n_used > 0 ? 1 : 0)) {
		okr = 0;
		printf("    rx intr=%" PRIu64 " (expected %d)\n", intr, n_used > 0 ? 1 : 0);
	}
	printf("    -> rx %d copies delivered (intr=%" PRIu64 "), predicted +%u delay insns; "
	       "tx %" PRIu64 " frames captured; icount=%" PRIu64 "\n",
	       n_used, intr, batch_extra, (uint64_t)tx.count, (uint64_t)r.icount);
	*ok_out = okr && okt;
	return fingerprint(fd, mem, txbuf);
}

/* Read the RX completion_due at the armed midpoint (proves "delayed to which icount"). */
static int midpoint_due(int fd, void *mem, uint64_t seed, uint32_t classes,
			uint32_t prob, uint32_t maxd, uint64_t *due)
{
	struct hvx_run_ctl ctl;
	struct hvx_snapshot s;
	int k, qi;

	if (setup(fd, mem) < 0) return -1;
	if (set_fault(fd, seed, classes, prob, maxd) < 0) return -1;
	/* clear any completion left armed by a previous PAUSED midpoint run (the guest
	 * clears it on device reset, but we check armed from step 0 -- before the reset --
	 * so a stale value would false-trigger at icount 0 and skip the kick). */
	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); return -1; }
	for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) { s.vnet_completion_armed[qi] = 0; s.vnet_irq_pending[qi] = 0; }
	if (ioctl(fd, HVX_SET_SNAPSHOT, &s) < 0) { perror("SET_SNAPSHOT"); return -1; }
	for (k = 0; k < 8000; k++) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (k == 0 ? HVX_RUN_RESET_ICOUNT : 0);
		ctl.max_insns = 1;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE) { k--; continue; }
		if (ctl.reason == EXIT_HLT) return -1;		/* fired already -> too late */
		if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); return -1; }
		if (s.vnet_completion_armed[0]) { *due = s.vnet_completion_due[0]; return 0; }
	}
	return -1;
}

int main(void)
{
	int fd, rc = 1;
	void *mem;
	uint8_t *ramsave, *txbuf;
	struct hvx_config cfg = { .mem_size = GUEST_MEM_BYTES };
	struct hvx_run_ctl r;
	struct hvx_snapshot mid;
	const uint64_t S1 = 0x1234, S2 = 0x9abcdef;
	const uint32_t CLASSES = HVX_NFAULT_DROP | HVX_NFAULT_DELAY | HVX_NFAULT_CORRUPT |
				 HVX_NFAULT_DUPLICATE | HVX_NFAULT_REORDER;
	const uint32_t PROB = 70, MAXD = 8;
	uint64_t fp_a1, fp_a2, fp_s2a, fp_s2b, fp_dis1, fp_dis2, fp_orig, fp_branch;
	uint64_t due_off = 0, due_on = 0;
	int ok_a1, ok_a2, ok_s2a, ok_s2b, ok_dis1, ok_dis2;
	int ok_repro, ok_seed, ok_branch, ok_disabled, ok_delay;
	uint32_t td = 0;
	int i;

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	ramsave = malloc(GUEST_MEM_BYTES);
	txbuf   = malloc(HVX_VNET_TX_CAP_USER);
	if (!ramsave || !txbuf) { perror("malloc"); goto out; }

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	if (attach(fd) < 0) { perror("HVX_NET_ATTACH"); goto out; }
	{ uint8_t code[8192]; size_t clen = build_guest(code);
	  if (clen > sizeof(code)) { fprintf(stderr, "guest code overflow\n"); goto out; }
	  memcpy((uint8_t *)mem + HVX_GUEST_CODE_BASE, code, clen); }

	/* (a) reproducible under a seed: schedule matches the prediction, two runs equal */
	printf("hvx_net_fault_test: (a) seed=0x%" PRIx64 " classes=0x%x prob=%u maxd=%u -- run #1\n",
	       S1, CLASSES, PROB, MAXD);
	fp_a1 = run_scenario(fd, mem, txbuf, S1, CLASSES, PROB, MAXD, &ok_a1);
	printf("hvx_net_fault_test: (a) seed=0x%" PRIx64 " -- run #2 (replay)\n", S1);
	fp_a2 = run_scenario(fd, mem, txbuf, S1, CLASSES, PROB, MAXD, &ok_a2);
	ok_repro = ok_a1 && ok_a2 && (fp_a1 == fp_a2) && fp_a1;
	printf("hvx_net_fault_test: (a) schedule-match=%d/%d fp1=%016" PRIx64 " fp2=%016" PRIx64 " -> %s\n",
	       ok_a1, ok_a2, fp_a1, fp_a2, ok_repro ? "REPRODUCIBLE" : "FAIL");

	/* "delayed to which icount": with DELAY-only prob=100 every RX frame is delayed,
	 * pushing completion_due out by the predicted summed delay vs the disabled baseline
	 * (same kick icount). */
	if (midpoint_due(fd, mem, 0, 0, 0, 0, &due_off) < 0) {
		fprintf(stderr, "hvx_net_fault_test: (a) baseline midpoint failed\n"); goto out;
	}
	if (midpoint_due(fd, mem, S1, HVX_NFAULT_DELAY, 100, MAXD, &due_on) < 0) {
		fprintf(stderr, "hvx_net_fault_test: (a) delay midpoint failed\n"); goto out;
	}
	for (i = 0; i < N_RX; i++) {
		uint32_t ex = 0, cp = 0; uint8_t cx = 0;
		int cls = predict_packet(S1, 0, i, HVX_NFAULT_DELAY, 100, MAXD, RX_FRAME_LEN, &ex, &cp, &cx);

		if (cls == NF_DELAY) td += ex;
	}
	ok_delay = (due_on == due_off + td);
	printf("hvx_net_fault_test: (a) completion_due off=%" PRIu64 " on=%" PRIu64
	       " (predicted +%u) -> %s\n", due_off, due_on, td, ok_delay ? "OK" : "FAIL");

	/* (b) the seed controls the schedule: S2 differs from S1 but is itself reproducible */
	printf("hvx_net_fault_test: (b) seed=0x%" PRIx64 " -- run #1\n", S2);
	fp_s2a = run_scenario(fd, mem, txbuf, S2, CLASSES, PROB, MAXD, &ok_s2a);
	printf("hvx_net_fault_test: (b) seed=0x%" PRIx64 " -- run #2 (replay)\n", S2);
	fp_s2b = run_scenario(fd, mem, txbuf, S2, CLASSES, PROB, MAXD, &ok_s2b);
	{
		int diff = 0;

		for (i = 0; i < N_RX; i++) {
			uint32_t e1 = 0, e2 = 0, p1 = 0, p2 = 0; uint8_t x1 = 0, x2 = 0;
			int f1 = predict_packet(S1, 0, i, CLASSES, PROB, MAXD, RX_FRAME_LEN, &e1, &p1, &x1);
			int f2 = predict_packet(S2, 0, i, CLASSES, PROB, MAXD, RX_FRAME_LEN, &e2, &p2, &x2);

			if (f1 != f2 || e1 != e2 || p1 != p2 || x1 != x2) diff = 1;
		}
		ok_seed = ok_s2a && ok_s2b && (fp_s2a == fp_s2b) && (fp_s2a != fp_a1) && diff;
		printf("hvx_net_fault_test: (b) S2 reproducible=%d fp_s2=%016" PRIx64
		       " (!= S1 %016" PRIx64 "), schedule-differs=%d -> %s\n",
		       (fp_s2a == fp_s2b), fp_s2a, fp_a1, diff, ok_seed ? "OK" : "FAIL");
	}

	/* (c) snapshot/branch mid-flight (armed RX completion, before it fires) reproduces
	 * the finish bit-for-bit -- so pending faults + PRNG state are captured. Seed S1. */
	if (setup(fd, mem) < 0) goto out;
	if (set_fault(fd, S1, CLASSES, PROB, MAXD) < 0) goto out;
	{
		struct hvx_run_ctl ctl;
		struct hvx_snapshot s;
		int k, armed = 0, qi;

		/* clear any stale armed completion (as in midpoint_due) before stepping */
		if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); goto out; }
		for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) { s.vnet_completion_armed[qi] = 0; s.vnet_irq_pending[qi] = 0; }
		if (ioctl(fd, HVX_SET_SNAPSHOT, &s) < 0) { perror("SET_SNAPSHOT"); goto out; }
		for (k = 0; k < 8000; k++) {
			memset(&ctl, 0, sizeof(ctl));
			ctl.flags = HVX_RUN_SINGLE_STEP | (k == 0 ? HVX_RUN_RESET_ICOUNT : 0);
			ctl.max_insns = 1;
			if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); goto out; }
			if (ctl.reason == HVX_EXIT_THROTTLE) { k--; continue; }
			if (ctl.reason == EXIT_HLT) break;
			if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); goto out; }
			if (s.vnet_completion_armed[0]) { armed = 1; break; }
		}
		if (!armed) { fprintf(stderr, "hvx_net_fault_test: (c) never reached armed midpoint\n"); goto out; }
	}
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	if (mid.nfault_rx_seq != N_RX || mid.nfault_tx_seq != N_TX) {
		fprintf(stderr, "hvx_net_fault_test: (c) snapshot missed PRNG counters "
			"(rx_seq=%" PRIu64 " tx_seq=%" PRIu64 ")\n",
			(uint64_t)mid.nfault_rx_seq, (uint64_t)mid.nfault_tx_seq);
		goto out;
	}
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem, txbuf);
	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("SET_SNAPSHOT"); goto out; }
	memcpy(mem, ramsave, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem, txbuf);
	ok_branch = (fp_orig == fp_branch) && (fp_branch == fp_a1) && fp_orig;
	printf("hvx_net_fault_test: (c) branch fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a1, ok_branch ? "OK" : "FAIL");

	/* (d) faults DISABLED => the multi-packet batch is delivered/captured exactly like
	 * M23 (all normal, used ring in submission order); and reproducible. */
	printf("hvx_net_fault_test: (d) faults DISABLED (classes=0) -- run #1\n");
	fp_dis1 = run_scenario(fd, mem, txbuf, 0, 0, 0, 0, &ok_dis1);
	fp_dis2 = run_scenario(fd, mem, txbuf, 0, 0, 0, 0, &ok_dis2);
	ok_disabled = ok_dis1 && ok_dis2 && (fp_dis1 == fp_dis2) && fp_dis1;
	printf("hvx_net_fault_test: (d) all-normal=%d/%d fp=%016" PRIx64 " reproducible=%d -> %s\n",
	       ok_dis1, ok_dis2, fp_dis1, (fp_dis1 == fp_dis2), ok_disabled ? "OK" : "FAIL");

	if (ok_repro && ok_delay && ok_seed && ok_branch && ok_disabled) {
		printf("hvx_net_fault_test: MILESTONE 24 PASS: seeded deterministic network fault "
		       "injection -- per-packet drop/delay/corrupt/duplicate + used-ring reorder on "
		       "RX and TX; schedule matches the prediction; reproducible under a seed; the "
		       "seed controls the schedule; branch reproduces it; disabled == M23\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_net_fault_test: FAIL: repro=%d delay=%d seed=%d branch=%d disabled=%d\n",
			ok_repro, ok_delay, ok_seed, ok_branch, ok_disabled);
	}

out:
	free(ramsave); free(txbuf);
	close(fd);
	return rc;
}

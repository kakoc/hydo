// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_net_test - milestone 23: the deterministic virtio-net PCI device.
 * =============================================================================
 *
 * A hand-written guest drives the whole virtio-net function without needing full
 * Linux:
 *   1. enumerates PCI config space (0xCF8/0xCFC) -- finds vendor 0x1af4, device
 *      0x1041 (modern virtio-net) at bus 0/dev 4/fn 0, reads BAR0;
 *   2. enables the device + bus-mastering, resets it, enables MSI-X and programs
 *      table entry 0 (vector 0x20 -> LAPIC);
 *   3. negotiates features through the MODERN virtio-pci common-config MMIO
 *      (VIRTIO_F_VERSION_1 + VIRTIO_NET_F_MAC) and reads back the MAC from the
 *      device config;
 *   4. sets up the receiveq (0) and transmitq (1), binding the RX queue's MSI-X
 *      vector to table entry 0;
 *   5. posts a receive buffer + a transmit frame, kicks the transmitq (the frame
 *      is captured host-side) and the receiveq (a scripted inbound frame, pushed
 *      via HVX_PUT_RX, is delivered into the buffer), and takes the RX completion
 *      as an MSI-X interrupt at a deterministic instruction; its ISR EOIs; HLT.
 *
 * Asserts: the RX frame arrived intact (12-byte virtio_net_hdr + payload), the TX
 * frame was captured intact (HVX_GET_TX), the MAC matches, the RX interrupt fired
 * once at a reproducible instruction, two runs are byte-identical (FNV
 * fingerprint over regs + device state + guest RAM + captured TX), and snapshot /
 * round-trip / branch reproduce.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_net_test hvx_net_test.c
 * Run   (inside L1):  sudo ./hvx_net_test
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
#define GUEST_MEM_BYTES (128UL * 4096)
#define INTR_CTR_GPA    0x2f00UL		/* the ISR increments this */
#define ISR_GPA         0x2800UL		/* GPA_ISR: built-in gate 0x20 targets here */
#define VECTOR          0x20			/* RX completion vector (built-in IDT gate) */

/* guest-RAM layout for enumeration scratch + the two virtqueues + buffers */
#define VID_STORE       0x7000UL		/* config[0x00] (vendor:device) */
#define BAR_STORE       0x7008UL		/* config[0x10] (BAR0) */
#define MAC_LO_STORE    0x7010UL		/* device-cfg mac[0..3] */
#define MAC_HI_STORE    0x7018UL		/* device-cfg mac[4..5] */

#define RX_DESC_GPA     0x8000UL
#define RX_AVAIL_GPA    0x8800UL
#define RX_USED_GPA     0x9000UL
#define RX_BUF_GPA      0xA000UL		/* writable receive buffer (hdr + frame) */

#define TX_DESC_GPA     0xC000UL
#define TX_AVAIL_GPA    0xC800UL
#define TX_USED_GPA     0xD000UL
#define TX_HDR_GPA      0xE000UL		/* 12-byte virtio_net_hdr (readable) */
#define TX_FRAME_GPA    0xE100UL		/* the outbound Ethernet frame (readable) */

#define QSIZE           8
#define RX_BUF_LEN      2048
#define MSIX_ADDR       0xFEE00000U		/* LAPIC MMIO base -> deliver to the BSP */

/* scripted frames (arbitrary but recognizable content) */
#define RX_FRAME_LEN    60
#define TX_FRAME_LEN    60

/* HVX_VNET_TX_CAP is a kernel-internal size; use a comfortably-large userspace cap. */
#define HVX_VNET_TX_CAP_USER 8192

static const uint8_t g_mac[6] = { 0x52, 0x54, 0x00, 0x12, 0x34, 0x56 };

/* Custom guest ISR (overwrites GPA_ISR): bump the completion counter, EOI the
 * LAPIC (x2APIC EOI MSR 0x80b), IRETQ. MSI-X is edge-delivered (no device ack
 * needed), so no virtio ISR-register read. */
static const uint8_t isr_blob[] = {
	0x50, 0x51, 0x52,				/* push rax; push rcx; push rdx */
	0x48, 0xff, 0x04, 0x25, 0x00, 0x2f, 0x00, 0x00,	/* inc qword [0x2f00] */
	0xb9, 0x0b, 0x08, 0x00, 0x00,			/* mov ecx, 0x80b (x2APIC EOI) */
	0x31, 0xc0,					/* xor eax,eax */
	0x31, 0xd2,					/* xor edx,edx */
	0x0f, 0x30,					/* wrmsr (EOI) */
	0x5a, 0x59, 0x58,				/* pop rdx; pop rcx; pop rax */
	0x48, 0xcf,					/* iretq */
};

/* ---- machine-code emitters (so the guest is checked, not hand-hexed) ---- */
static size_t movabs(uint8_t *c, size_t n, uint8_t reg_op, uint64_t v)
{
	c[n++] = 0x48; c[n++] = reg_op; memcpy(c + n, &v, 8); n += 8; return n;
}
static size_t mmio_w32(uint8_t *c, size_t n, uint64_t addr, uint32_t val)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0xbe; memcpy(c + n, &val, 4); n += 4;	/* mov esi, val   */
	c[n++] = 0x89; c[n++] = 0x37;			/* mov [rdi], esi */
	return n;
}
static size_t mmio_w16(uint8_t *c, size_t n, uint64_t addr, uint16_t val)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x66; c[n++] = 0xbe; memcpy(c + n, &val, 2); n += 2;	/* mov si, val */
	c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x37;	/* mov [rdi], si */
	return n;
}
static size_t mmio_r32_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x8b; c[n++] = 0x37;			/* mov esi, [rdi] */
	return n;
}
static size_t mmio_wr_esi(uint8_t *c, size_t n, uint64_t addr)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x89; c[n++] = 0x37;			/* mov [rdi], esi */
	return n;
}
static size_t mmio_r32_store(uint8_t *c, size_t n, uint64_t addr, uint64_t store)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x8b; c[n++] = 0x07;			/* mov eax, [rdi] */
	n = movabs(c, n, 0xbe, store);			/* movabs rsi, store */
	c[n++] = 0x89; c[n++] = 0x06;			/* mov [rsi], eax */
	return n;
}
static size_t mmio_r16_store(uint8_t *c, size_t n, uint64_t addr, uint64_t store)
{
	n = movabs(c, n, 0xbf, addr);			/* movabs rdi, addr */
	c[n++] = 0x66; c[n++] = 0x8b; c[n++] = 0x07;	/* mov ax, [rdi] */
	n = movabs(c, n, 0xbe, store);			/* movabs rsi, store */
	c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x06;	/* mov [rsi], ax */
	return n;
}
static size_t pio_out32(uint8_t *c, size_t n, uint32_t val, uint32_t port)
{
	c[n++] = 0xb8; memcpy(c + n, &val, 4); n += 4;	/* mov eax, val  */
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;	/* mov edx, port */
	c[n++] = 0xef;					/* out dx, eax */
	return n;
}
static uint32_t cfg_addr(unsigned reg) { return 0x80000000u | (HVX_VNET_PCI_DEV << 11) | (reg & 0xfc); }

static size_t pio_cfg_read_store(uint8_t *c, size_t n, unsigned reg, uint64_t store)
{
	uint32_t port = HVX_PCI_CONFIG_DATA;

	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);	/* CONFIG_ADDRESS */
	c[n++] = 0xba; memcpy(c + n, &port, 4); n += 4;		/* mov edx, 0xcfc */
	c[n++] = 0xed;						/* in eax, dx */
	n = movabs(c, n, 0xbf, store);				/* movabs rdi, store */
	c[n++] = 0x89; c[n++] = 0x07;				/* mov [rdi], eax */
	return n;
}
static size_t pio_cfg_write(uint8_t *c, size_t n, unsigned reg, uint32_t val)
{
	n = pio_out32(c, n, cfg_addr(reg), HVX_PCI_CONFIG_ADDR);
	n = pio_out32(c, n, val, HVX_PCI_CONFIG_DATA);
	return n;
}

/* set up one queue's registers (desc/avail/used GPAs, size, msix vector, enable) */
static size_t setup_queue(uint8_t *c, size_t n, uint64_t B, unsigned qi,
			  uint64_t desc, uint64_t avail, uint64_t used, unsigned msix)
{
	n = mmio_w16(c, n, B + 0x16, qi);		/* queue_select */
	n = mmio_w16(c, n, B + 0x18, QSIZE);		/* queue_size */
	n = mmio_w16(c, n, B + 0x1a, msix);		/* queue_msix_vector */
	n = mmio_w32(c, n, B + 0x20, (uint32_t)desc);	/* queue_desc lo */
	n = mmio_w32(c, n, B + 0x24, 0);		/* queue_desc hi */
	n = mmio_w32(c, n, B + 0x28, (uint32_t)avail);	/* queue_driver lo */
	n = mmio_w32(c, n, B + 0x2c, 0);
	n = mmio_w32(c, n, B + 0x30, (uint32_t)used);	/* queue_device lo */
	n = mmio_w32(c, n, B + 0x34, 0);
	n = mmio_w16(c, n, B + 0x1c, 1);		/* queue_enable = 1 */
	return n;
}

static size_t build_guest(uint8_t *c)
{
	const uint64_t B = HVX_VNET_BAR;
	size_t n = 0;

	/* 1. enumerate: vendor|device + BAR0 */
	n = pio_cfg_read_store(c, n, 0x00, VID_STORE);
	n = pio_cfg_read_store(c, n, 0x10, BAR_STORE);
	/* 2. PCI command: memory space (bit1) + bus master (bit2) */
	n = pio_cfg_write(c, n, 0x04, 0x6);
	/* 3. reset the device (device_status = 0) so a replay starts clean */
	n = mmio_w32(c, n, B + 0x14, 0);
	/* 4. enable MSI-X (message control enable bit 15 -> bit 31 of cfg dword 0x90) */
	n = pio_cfg_write(c, n, HVX_VNET_MSIX_CAP, 0x80000000u);
	/* program MSI-X table entry 0 in BAR0: addr_lo/hi, msg_data(vector), ctrl */
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x0, MSIX_ADDR);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x4, 0);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0x8, VECTOR);
	n = mmio_w32(c, n, B + HVX_VPCI_MSIX_TABLE + 0xc, 0);
	/* 5. virtio modern common-config handshake */
	n = mmio_w32(c, n, B + 0x14, 1);	/* device_status = ACKNOWLEDGE */
	n = mmio_w32(c, n, B + 0x14, 3);	/* |= DRIVER */
	/* accept VERSION_1 (feature dword 1) */
	n = mmio_w32(c, n, B + 0x00, 1);	/* device_feature_select = 1 */
	n = mmio_r32_esi(c, n, B + 0x04);	/* esi = device_feature (VERSION_1) */
	n = mmio_w32(c, n, B + 0x08, 1);	/* driver_feature_select = 1 */
	n = mmio_wr_esi(c, n, B + 0x0c);	/* driver_feature = esi */
	/* accept MAC (feature dword 0) */
	n = mmio_w32(c, n, B + 0x00, 0);	/* device_feature_select = 0 */
	n = mmio_r32_esi(c, n, B + 0x04);	/* esi = device_feature (MAC) */
	n = mmio_w32(c, n, B + 0x08, 0);	/* driver_feature_select = 0 */
	n = mmio_wr_esi(c, n, B + 0x0c);	/* driver_feature = esi */
	n = mmio_w32(c, n, B + 0x14, 0xB);	/* |= FEATURES_OK (3|8) */
	/* read the MAC out of device config (BAR + DEVICE offset) */
	n = mmio_r32_store(c, n, B + HVX_VPCI_DEVICE + 0, MAC_LO_STORE);
	n = mmio_r16_store(c, n, B + HVX_VPCI_DEVICE + 4, MAC_HI_STORE);
	/* 6. set up the receiveq (0) bound to MSI-X entry 0, and the transmitq (1)
	 *    with NO MSI-X vector (no TX interrupt needed by this test) */
	n = setup_queue(c, n, B, 0, RX_DESC_GPA, RX_AVAIL_GPA, RX_USED_GPA, 0);
	n = setup_queue(c, n, B, 1, TX_DESC_GPA, TX_AVAIL_GPA, TX_USED_GPA, HVX_MSIX_NO_VECTOR);
	n = mmio_w32(c, n, B + 0x14, 0xF);	/* device_status |= DRIVER_OK */
	/* 7. kick the transmitq (capture) then the receiveq (deliver + arm MSI-X) */
	n = mmio_w16(c, n, B + HVX_VPCI_NOTIFY, 1);	/* notify queue 1 (TX) */
	n = mmio_w16(c, n, B + HVX_VPCI_NOTIFY, 0);	/* notify queue 0 (RX) */
	/* 8. wait for the RX completion ISR, then HLT */
	n = movabs(c, n, 0xbb, INTR_CTR_GPA);		/* movabs rbx, 0x2f00 */
	c[n++] = 0x48; c[n++] = 0x8b; c[n++] = 0x03;	/* wait: mov rax,[rbx] */
	c[n++] = 0x48; c[n++] = 0x85; c[n++] = 0xc0;	/* test rax,rax */
	c[n++] = 0x74; c[n++] = 0xf8;			/* jz wait (-8) */
	c[n++] = 0xf4;					/* hlt */
	return n;
}

/* ---- split-queue descriptor + queue layout ---- */
struct vqd { uint64_t addr; uint32_t len; uint16_t flags; uint16_t next; };

static void put_desc(void *mem, uint64_t base, unsigned i, uint64_t addr, uint32_t len,
		     uint16_t flags, uint16_t next)
{
	struct vqd d = { addr, len, flags, next };
	memcpy((uint8_t *)mem + base + i * 16, &d, sizeof(d));
}
static void put_avail(void *mem, uint64_t base, uint16_t idx, uint16_t head0)
{
	uint16_t a[3] = { 0, idx, head0 };	/* flags, idx, ring[0] */
	memcpy((uint8_t *)mem + base, a, sizeof(a));
}

static void build_queues(void *mem)
{
	unsigned i;

	/* RX: one writable buffer (device writes 12-byte hdr + frame) */
	memset((uint8_t *)mem + RX_BUF_GPA, 0, RX_BUF_LEN);
	put_desc(mem, RX_DESC_GPA, 0, RX_BUF_GPA, RX_BUF_LEN, VIRTQ_DESC_F_WRITE, 0);
	put_avail(mem, RX_AVAIL_GPA, 1, 0);
	memset((uint8_t *)mem + RX_USED_GPA, 0, 8 + 8 * QSIZE);

	/* TX: a 2-descriptor chain [12-byte hdr][frame] (exercises chaining) */
	memset((uint8_t *)mem + TX_HDR_GPA, 0, HVX_VNET_HDR_LEN);
	for (i = 0; i < TX_FRAME_LEN; i++)
		((uint8_t *)mem)[TX_FRAME_GPA + i] = (uint8_t)(0x90 + (i & 0x1f));
	put_desc(mem, TX_DESC_GPA, 0, TX_HDR_GPA, HVX_VNET_HDR_LEN, VIRTQ_DESC_F_NEXT, 1);
	put_desc(mem, TX_DESC_GPA, 1, TX_FRAME_GPA, TX_FRAME_LEN, 0, 0);
	put_avail(mem, TX_AVAIL_GPA, 1, 0);
	memset((uint8_t *)mem + TX_USED_GPA, 0, 8 + 8 * QSIZE);
}

static void make_rx_frame(uint8_t *f)
{
	unsigned i;

	for (i = 0; i < RX_FRAME_LEN; i++)
		f[i] = (uint8_t)(0x40 + (i & 0x1f));
}

/* ---- fingerprint (regs + device control + RAM + captured TX) ---- */
static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}
static uint64_t fingerprint(int fd, void *mem)
{
	struct hvx_snapshot s;
	uint8_t txbuf[HVX_VNET_TX_CAP_USER];
	struct hvx_tx tx = { .len = sizeof(txbuf), .data = (uint64_t)(uintptr_t)txbuf };
	uint64_t ram_len = GUEST_MEM_BYTES;
	uint32_t rx_count;
	uint64_t h = 0xcbf29ce484222325ULL;
	int qi;

	if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("HVX_GET_SNAPSHOT"); return 0; }
	if (ioctl(fd, HVX_GET_TX, &tx) < 0)       { perror("HVX_GET_TX"); return 0; }

	h = fnv(h, "hvx-vnet-v1", 11);
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
	/* pending-inbound-FIFO COUNT, not the absolute (globally free-running) cursors,
	 * so replay -- which re-pushes a frame each run -- fingerprints identically */
	rx_count = s.vnet_rx_tail - s.vnet_rx_head;
	h = fnv(h, &rx_count, sizeof(rx_count));
	h = fnv(h, &s.vnet_tx_len, sizeof(s.vnet_tx_len));
	h = fnv(h, &s.vnet_tx_count, sizeof(s.vnet_tx_count));
	h = fnv(h, &ram_len, sizeof(ram_len));
	h = fnv(h, mem, GUEST_MEM_BYTES);
	h = fnv(h, &tx.len, sizeof(tx.len));
	h = fnv(h, txbuf, tx.len < sizeof(txbuf) ? tx.len : sizeof(txbuf));
	return h;
}

static int attach(int fd)
{
	struct hvx_net_config nc;

	memset(&nc, 0, sizeof(nc));
	memcpy(nc.mac, g_mac, 6);
	return ioctl(fd, HVX_NET_ATTACH, &nc) < 0 ? -1 : 0;
}
static int push_rx(int fd)
{
	struct hvx_rx rx;

	memset(&rx, 0, sizeof(rx));
	rx.len = RX_FRAME_LEN;
	make_rx_frame(rx.buf);
	return ioctl(fd, HVX_PUT_RX, &rx) < 0 ? -1 : 0;
}
static int setup(int fd, void *mem)
{
	struct hvx_regs regs;

	if (push_rx(fd) < 0) { perror("HVX_PUT_RX"); return -1; }
	build_queues(mem);
	memcpy((uint8_t *)mem + ISR_GPA, isr_blob, sizeof(isr_blob));	/* custom EOI ISR */
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
			if (++throttles > 64) {
				fprintf(stderr, "hvx_net_test: FAIL: guest never HLTed "
					"(icount=%" PRIu64 ") -- RX IRQ likely not delivered\n",
					(uint64_t)ctl.icount);
				*out = ctl;
				return -1;
			}
			continue;
		}
		*out = ctl;
		return ctl.reason == EXIT_HLT ? 0 : -1;
	}
}
/* single-step until the RX queue has a completion scheduled but not yet fired */
static int step_until_armed(int fd, uint64_t *irq_icount)
{
	struct hvx_run_ctl ctl;
	struct hvx_snapshot s;
	int k;

	for (k = 0; k < 4000; k++) {
		memset(&ctl, 0, sizeof(ctl));
		ctl.flags = HVX_RUN_SINGLE_STEP | (k == 0 ? HVX_RUN_RESET_ICOUNT : 0);
		ctl.max_insns = 1;
		if (ioctl(fd, HVX_RUNC, &ctl) < 0) { perror("HVX_RUNC"); return -1; }
		if (ctl.reason == HVX_EXIT_THROTTLE) { k--; continue; }
		if (ctl.reason == EXIT_HLT) return -1;		/* fired already -> too late */
		if (ioctl(fd, HVX_GET_SNAPSHOT, &s) < 0) { perror("GET_SNAPSHOT"); return -1; }
		if (s.vnet_completion_armed[0]) {
			if (irq_icount)
				*irq_icount = s.vnet_completion_due[0];
			return 0;
		}
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
	struct hvx_tx tx;
	uint64_t fp_a, fp_b, fp_round, fp_orig, fp_branch;
	uint64_t irq_icount = 0;
	unsigned i;
	int ok_enum, ok_mac, ok_rx, ok_tx, ok_replay, ok_round, ok_branch;
	uint8_t rx_expect[RX_FRAME_LEN];

	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) { perror("open /dev/hvx"); return 1; }
	ramsave = malloc(GUEST_MEM_BYTES);
	txbuf   = malloc(HVX_VNET_TX_CAP_USER);
	if (!ramsave || !txbuf) { perror("malloc"); goto out; }
	make_rx_frame(rx_expect);

	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); goto out; }
	mem = mmap(NULL, GUEST_MEM_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) { perror("mmap"); goto out; }
	if (attach(fd) < 0) { perror("HVX_NET_ATTACH"); goto out; }

	{ uint8_t code[1024]; size_t clen = build_guest(code);
	  if (clen > sizeof(code)) { fprintf(stderr, "guest code overflow\n"); goto out; }
	  memcpy((uint8_t *)mem + HVX_GUEST_CODE_BASE, code, clen); }

	/* run #1 */
	if (setup(fd, mem) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) {
		fprintf(stderr, "hvx_net_test: FAIL: run #1 did not HLT (reason=%u)\n", r.reason);
		goto out;
	}

	/* enumeration + MAC */
	ok_enum = (*(uint32_t *)((uint8_t *)mem + VID_STORE) ==
		   (((uint32_t)HVX_VNET_DEVICE << 16) | HVX_VNET_VENDOR)) &&
		  (*(uint32_t *)((uint8_t *)mem + BAR_STORE) == (uint32_t)HVX_VNET_BAR);
	printf("hvx_net_test: enum vendor:device=0x%08x BAR0=0x%08x -> %s\n",
	       *(uint32_t *)((uint8_t *)mem + VID_STORE),
	       *(uint32_t *)((uint8_t *)mem + BAR_STORE), ok_enum ? "OK" : "BAD");
	{
		uint8_t mac[6];
		memcpy(&mac[0], (uint8_t *)mem + MAC_LO_STORE, 4);
		memcpy(&mac[4], (uint8_t *)mem + MAC_HI_STORE, 2);
		ok_mac = memcmp(mac, g_mac, 6) == 0;
		printf("hvx_net_test: mac=%02x:%02x:%02x:%02x:%02x:%02x -> %s\n",
		       mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], ok_mac ? "OK" : "BAD");
	}

	/* RX delivered: virtio_net_hdr (num_buffers=1) + the frame, intact */
	{
		uint8_t *rb = (uint8_t *)mem + RX_BUF_GPA;
		int hdr_ok = (rb[10] == 1);	/* num_buffers low byte */
		int data_ok = memcmp(rb + HVX_VNET_HDR_LEN, rx_expect, RX_FRAME_LEN) == 0;

		ok_rx = hdr_ok && data_ok &&
			*(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA) == 1;
		printf("hvx_net_test: rx hdr=%s data=%s intr=%" PRIu64 " icount=%" PRIu64 " -> %s\n",
		       hdr_ok ? "OK" : "BAD", data_ok ? "OK" : "BAD",
		       *(volatile uint64_t *)((uint8_t *)mem + INTR_CTR_GPA),
		       (uint64_t)r.icount, ok_rx ? "OK" : "BAD");
	}

	/* TX captured intact */
	memset(&tx, 0, sizeof(tx));
	tx.len = HVX_VNET_TX_CAP_USER; tx.data = (uint64_t)(uintptr_t)txbuf;
	if (ioctl(fd, HVX_GET_TX, &tx) < 0) { perror("HVX_GET_TX"); goto out; }
	{
		int tx_ok = (tx.len == TX_FRAME_LEN) && (tx.count == 1);

		for (i = 0; i < TX_FRAME_LEN && tx_ok; i++)
			if (txbuf[i] != (uint8_t)(0x90 + (i & 0x1f)))
				tx_ok = 0;
		ok_tx = tx_ok;
		printf("hvx_net_test: tx captured len=%" PRIu64 " count=%" PRIu64 " -> %s\n",
		       (uint64_t)tx.len, (uint64_t)tx.count, ok_tx ? "OK" : "BAD");
	}

	if (!ok_enum || !ok_mac || !ok_rx || !ok_tx) {
		fprintf(stderr, "hvx_net_test: FAIL: enumeration / mac / rx / tx\n");
		goto out;
	}
	fp_a = fingerprint(fd, mem);

	/* replay */
	if (setup(fd, mem) < 0) goto out;
	if (run_to_halt(fd, 1, &r) < 0) goto out;
	fp_b = fingerprint(fd, mem);
	ok_replay = (fp_a == fp_b);
	printf("hvx_net_test: replay     fp_a=%016" PRIx64 " fp_b=%016" PRIx64 " -> %s\n",
	       fp_a, fp_b, ok_replay ? "OK" : "MISMATCH");

	/* round-trip: snapshot + restore (RAM) */
	{
		struct hvx_snapshot snap;

		if (ioctl(fd, HVX_GET_SNAPSHOT, &snap) < 0) { perror("GET_SNAPSHOT"); goto out; }
		memcpy(ramsave, mem, GUEST_MEM_BYTES);
		if (ioctl(fd, HVX_SET_SNAPSHOT, &snap) < 0) { perror("SET_SNAPSHOT"); goto out; }
		memcpy(mem, ramsave, GUEST_MEM_BYTES);
		fp_round = fingerprint(fd, mem);
	}
	ok_round = (fp_round == fp_b);
	printf("hvx_net_test: round-trip fp=%016" PRIx64 " (vs %016" PRIx64 ") -> %s\n",
	       fp_round, fp_b, ok_round ? "OK" : "MISMATCH");

	/* branch: step to the armed-but-not-fired point (RX frame already in RAM),
	 * snapshot RAM + state, finish; restore, finish; assert identical. */
	if (setup(fd, mem) < 0) goto out;
	if (step_until_armed(fd, &irq_icount) < 0) {
		fprintf(stderr, "hvx_net_test: FAIL: could not reach the armed RX midpoint\n");
		goto out;
	}
	printf("hvx_net_test: RX completion armed for icount=%" PRIu64 " (reproducible)\n", irq_icount);
	if (ioctl(fd, HVX_GET_SNAPSHOT, &mid) < 0) { perror("GET_SNAPSHOT"); goto out; }
	memcpy(ramsave, mem, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_orig = fingerprint(fd, mem);

	if (ioctl(fd, HVX_SET_SNAPSHOT, &mid) < 0) { perror("SET_SNAPSHOT"); goto out; }
	memcpy(mem, ramsave, GUEST_MEM_BYTES);
	if (run_to_halt(fd, 0, &r) < 0) goto out;
	fp_branch = fingerprint(fd, mem);
	ok_branch = (fp_orig == fp_branch && fp_branch == fp_a);
	printf("hvx_net_test: branch     fp_orig=%016" PRIx64 " fp_branch=%016" PRIx64
	       " (full=%016" PRIx64 ") -> %s\n", fp_orig, fp_branch, fp_a,
	       ok_branch ? "OK" : "MISMATCH");

	if (ok_enum && ok_mac && ok_rx && ok_tx && ok_replay && ok_round && ok_branch) {
		printf("hvx_net_test: MILESTONE 23 PASS: hand-written guest enumerated the virtio-net "
		       "PCI function, negotiated VERSION_1+MAC, set up receiveq/transmitq, received a "
		       "scripted RX frame (MSI-X at a reproducible instruction) and had its TX frame "
		       "captured -- deterministic + snapshot-reproducible\n");
		rc = 0;
	} else {
		fprintf(stderr, "hvx_net_test: FAIL: a check did not hold\n");
	}

out:
	free(ramsave); free(txbuf);
	close(fd);
	return rc;
}

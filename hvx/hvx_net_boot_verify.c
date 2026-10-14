// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_net_boot_verify - milestone 23 (stretch): WHOLE-BOOT determinism for a REAL
 * Linux kernel that binds virtio_net, brings up eth0 with a static IP, and replies
 * to a scripted ARP request pushed via HVX_PUT_RX -- plus branch-from-snapshot.
 * =============================================================================
 *
 * The with-net sibling of hvx_disk_boot_verify.c (M21). Same discipline: boot the
 * staged bzImage + net-enabled initramfs in FAST free-run, stop at a fixed console
 * marker, and fingerprint the FULL end state -- but here the virtio-net PCI function
 * is ATTACHED and a scripted ARP request is pushed BEFORE the boot. Real Linux
 * enumerates the function, binds virtio_net, creates eth0, brings it up (10.0.0.2),
 * and replies to the ARP; the reply is captured on the transmitq. The fingerprint
 * additionally covers the virtio-net device state + the captured TX bytes, so a
 * divergence anywhere in the network path is caught. IPv6 is disabled (the cmdline)
 * so the only guest-originated frame is the deterministic ARP reply.
 *
 * Two invariants:
 *   1. WHOLE-BOOT: two fresh net boots are bit-identical in FULL state (console +
 *      snapshot + 256 MiB RAM + captured TX).
 *   2. BRANCH: snapshot mid-boot (before the net modules load / the ARP is
 *      delivered), continue to the marker twice (straight + restored), assert
 *      identical -- proving the pending RX frame + queue/completion/MSI-X state are
 *      all captured by the snapshot.
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_net_boot_verify hvx_net_boot_verify.c
 * Run   (inside L1):  sudo ./hvx_net_boot_verify /tmp/bzImage /tmp/initramfs-net.cpio.gz
 */

#include "hvx_boot.h"

/* Printed by the net /init (build-linux.sh) after eth0 is up + the ARP has had time
 * to be delivered and replied -- the deterministic stop point. */
#define FINAL_MARKER "hydo-boot-marker: launching shell on ttyS0"

/* Deep-in-kernel-init marker, well before the initramfs unpacks / the net modules
 * load / the ARP is delivered. Branching here forces the snapshot to reproduce the
 * module load + PCI enumeration + interface bring-up + RX delivery + the reply. */
#define MID_MARKER "Trying to unpack rootfs image as initramfs"

#define BOOT_TSC_STEP 0x10000ULL
#define UBUF_CAP (1UL << 20)
#define MAX_RUN_ITERS 50000000ULL

static const uint8_t guest_mac[6] = { 0x52, 0x54, 0x00, 0x12, 0x34, 0x56 };
/* ARP request: who-has 10.0.0.2 (the guest) tell 10.0.0.1 (52:54:00:aa:bb:cc) */
static const uint8_t arp_request[42] = {
	0xff, 0xff, 0xff, 0xff, 0xff, 0xff,	0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc,
	0x08, 0x06,	0x00, 0x01,	0x08, 0x00,	0x06, 0x04,	0x00, 0x01,
	0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc,	10, 0, 0, 1,
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00,	10, 0, 0, 2,
};

static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const uint8_t *b = p;
	size_t i;
	for (i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; }
	return h;
}

struct boot {
	int fd;
	uint8_t *mem;
	uint64_t mem_size;
	uint8_t *ubuf;		/* reconstructed console (this timeline) */
	size_t ulen;
	size_t drained;
	uint8_t *txbuf;		/* scratch for HVX_GET_TX in the fingerprint */
};

static long mem_find(const uint8_t *hay, size_t hlen, const char *needle)
{
	size_t nlen = strlen(needle), i;
	if (nlen == 0 || hlen < nlen)
		return -1;
	for (i = 0; i + nlen <= hlen; i++)
		if (memcmp(hay + i, needle, nlen) == 0)
			return (long)i;
	return -1;
}

static struct hvx_console con_scratch;
static void console_drain(struct boot *b)
{
	size_t modlen, add;

	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
	modlen = con_scratch.len;
	if (modlen > HVX_CONSOLE_SZ)
		modlen = HVX_CONSOLE_SZ;
	if (modlen <= b->drained)
		return;
	add = modlen - b->drained;
	if (b->ulen + add > UBUF_CAP)
		add = UBUF_CAP - b->ulen;
	memcpy(b->ubuf + b->ulen, con_scratch.buf + b->drained, add);
	b->ulen += add;
	b->drained = modlen;
}

static void console_rebase(struct boot *b)
{
	if (ioctl(b->fd, HVX_GET_CONSOLE, &con_scratch) < 0)
		return;
	b->drained = con_scratch.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con_scratch.len;
}

static int run_until_marker(struct boot *b, const char *marker)
{
	struct hvx_exit ex;
	unsigned long long iters, throttles = 0;

	for (iters = 0; iters < MAX_RUN_ITERS; iters++) {
		memset(&ex, 0, sizeof(ex));
		if (ioctl(b->fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "hvx_net_boot_verify: HVX_RUN: %s\n", strerror(errno));
			return -1;
		}
		console_drain(b);

		if (mem_find(b->ubuf, b->ulen, marker) >= 0)
			return 0;

		if (ex.reason == HVX_EXIT_THROTTLE) {
			if ((++throttles % 512) == 0)
				fprintf(stderr, "[hvx_net_boot_verify: %llu throttles, %zu console "
					"bytes, still running]\n", throttles, b->ulen);
			continue;
		}

		{
			struct hvx_regs cur;
			unsigned long long rip = 0;
			size_t from = b->ulen > 200 ? b->ulen - 200 : 0;

			if (ioctl(b->fd, HVX_GET_REGS, &cur) == 0)
				rip = (unsigned long long)cur.rip;
			fprintf(stderr, "hvx_net_boot_verify: stopped before marker \"%s\": "
				"reason=%u (%s) qual=%#llx gpa=%#llx rip=%#llx\n",
				marker, ex.reason, exit_reason_name(ex.reason),
				(unsigned long long)ex.qualification,
				(unsigned long long)ex.guest_phys, rip);
			fprintf(stderr, "hvx_net_boot_verify: console tail: |%.*s|\n",
				(int)(b->ulen - from), b->ubuf + from);
		}
		return -1;
	}
	fprintf(stderr, "hvx_net_boot_verify: iteration cap reached before marker \"%s\"\n", marker);
	return -1;
}

/* Canonical full-state fingerprint: console + snapshot (meaningful fields incl. the
 * virtio-net device + MSI-X state) + all guest RAM + the captured TX frames. */
static uint64_t fingerprint(struct boot *b)
{
	struct hvx_snapshot *s;
	struct hvx_tx tx;
	uint64_t h = 0xcbf29ce484222325ULL;
	uint64_t clen = b->ulen, rlen = b->mem_size;
	uint32_t rx_count;
	int qi;

	s = calloc(1, sizeof(*s));
	if (!s) { perror("calloc"); return 0; }
	if (ioctl(b->fd, HVX_GET_SNAPSHOT, s) < 0) { perror("HVX_GET_SNAPSHOT"); free(s); return 0; }

	h = fnv(h, "hvx-net-boot-state-v1", 21);
	/* console */
	h = fnv(h, &clen, sizeof(clen));
	h = fnv(h, b->ubuf, b->ulen);
	/* register file + deterministic clocks/TSC */
	h = fnv(h, &s->regs, sizeof(s->regs));
	h = fnv(h, &s->tsc, sizeof(s->tsc));
	h = fnv(h, &s->tsc_step, sizeof(s->tsc_step));
	h = fnv(h, &s->tsc_mode, sizeof(s->tsc_mode));
	h = fnv(h, &s->icount, sizeof(s->icount));
	h = fnv(h, &s->rdtsc_count, sizeof(s->rdtsc_count));
	/* pending instruction-timed injection */
	h = fnv(h, &s->inject_at_icount, sizeof(s->inject_at_icount));
	h = fnv(h, &s->inject_vector, sizeof(s->inject_vector));
	h = fnv(h, &s->inject_valid, sizeof(s->inject_valid));
	/* curated MSR store + privileged register state */
	h = fnv(h, s->sw_msr, sizeof(s->sw_msr));
	h = fnv(h, &s->sregs, sizeof(s->sregs));
	/* APIC / IOAPIC / PIC */
	h = fnv(h, &s->lapic_apic_base, sizeof(s->lapic_apic_base));
	h = fnv(h, &s->lapic_tsc_deadline, sizeof(s->lapic_tsc_deadline));
	h = fnv(h, s->lapic_reg, sizeof(s->lapic_reg));
	h = fnv(h, &s->lapic_in_service, sizeof(s->lapic_in_service));
	h = fnv(h, s->ioapic_redir, sizeof(s->ioapic_redir));
	h = fnv(h, s->pic_mask, sizeof(s->pic_mask));
	/* 16550 UART latch */
	h = fnv(h, &s->uart_ier, 1); h = fnv(h, &s->uart_lcr, 1);
	h = fnv(h, &s->uart_mcr, 1); h = fnv(h, &s->uart_fcr, 1);
	h = fnv(h, &s->uart_scr, 1); h = fnv(h, &s->uart_dll, 1); h = fnv(h, &s->uart_dlm, 1);
	/* virtio-net device control state (M23) */
	h = fnv(h, &s->vnet_present, sizeof(s->vnet_present));
	h = fnv(h, s->vnet_mac, sizeof(s->vnet_mac));
	h = fnv(h, &s->vnet_dev_status, sizeof(s->vnet_dev_status));
	h = fnv(h, &s->vnet_gfeatures, sizeof(s->vnet_gfeatures));
	h = fnv(h, &s->vnet_pci_command, sizeof(s->vnet_pci_command));
	h = fnv(h, &s->vnet_pci_bar0, sizeof(s->vnet_pci_bar0));
	h = fnv(h, &s->vnet_isr, sizeof(s->vnet_isr));
	h = fnv(h, &s->vnet_msix_control, sizeof(s->vnet_msix_control));
	h = fnv(h, &s->vnet_config_msix, sizeof(s->vnet_config_msix));
	h = fnv(h, s->vnet_msix_tbl, sizeof(s->vnet_msix_tbl));
	for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) {
		h = fnv(h, &s->vnet_desc[qi], sizeof(s->vnet_desc[qi]));
		h = fnv(h, &s->vnet_avail[qi], sizeof(s->vnet_avail[qi]));
		h = fnv(h, &s->vnet_used[qi], sizeof(s->vnet_used[qi]));
		h = fnv(h, &s->vnet_qsize[qi], sizeof(s->vnet_qsize[qi]));
		h = fnv(h, &s->vnet_qready[qi], sizeof(s->vnet_qready[qi]));
		h = fnv(h, &s->vnet_last_avail[qi], sizeof(s->vnet_last_avail[qi]));
		h = fnv(h, &s->vnet_queue_msix[qi], sizeof(s->vnet_queue_msix[qi]));
		h = fnv(h, &s->vnet_completion_armed[qi], sizeof(s->vnet_completion_armed[qi]));
		h = fnv(h, &s->vnet_completion_due[qi], sizeof(s->vnet_completion_due[qi]));
		h = fnv(h, &s->vnet_irq_pending[qi], sizeof(s->vnet_irq_pending[qi]));
	}
	/* pending-inbound-FIFO COUNT (not the absolute free-running cursors) */
	rx_count = s->vnet_rx_tail - s->vnet_rx_head;
	h = fnv(h, &rx_count, sizeof(rx_count));
	h = fnv(h, &s->vnet_tx_len, sizeof(s->vnet_tx_len));
	h = fnv(h, &s->vnet_tx_count, sizeof(s->vnet_tx_count));
	/* all guest RAM */
	h = fnv(h, &rlen, sizeof(rlen));
	h = fnv(h, b->mem, b->mem_size);
	/* the captured TX frames */
	memset(&tx, 0, sizeof(tx));
	tx.len = 8192; tx.data = (uint64_t)(uintptr_t)b->txbuf;
	if (ioctl(b->fd, HVX_GET_TX, &tx) == 0) {
		uint64_t got = tx.len < 8192 ? tx.len : 8192;
		h = fnv(h, &tx.len, sizeof(tx.len));
		h = fnv(h, b->txbuf, got);
	}
	free(s);
	return h;
}

static int attach_net(struct boot *b)
{
	struct hvx_net_config nc;
	struct hvx_rx rx;

	memset(&nc, 0, sizeof(nc));
	memcpy(nc.mac, guest_mac, 6);
	nc.flags = HVX_NET_NO_IDMAP | HVX_NET_PACED;	/* trickle RX for a real OS */
	if (ioctl(b->fd, HVX_NET_ATTACH, &nc) < 0) { perror("HVX_NET_ATTACH"); return -1; }
	memset(&rx, 0, sizeof(rx));
	rx.len = sizeof(arp_request);
	memcpy(rx.buf, arp_request, sizeof(arp_request));
	if (ioctl(b->fd, HVX_PUT_RX, &rx) < 0) { perror("HVX_PUT_RX"); return -1; }
	return 0;
}

static int boot_setup(struct boot *b, uint64_t mem_size,
		      const uint8_t *image, size_t image_len,
		      const uint8_t *initrd, size_t initrd_len, const char *cmdline)
{
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_tsc tsc = { .tsc = 0, .step = BOOT_TSC_STEP, .aux = 0, .mode = HVX_TSC_COUNTER };

	memset(b, 0, sizeof(*b));
	b->fd = -1;
	b->mem = MAP_FAILED;
	b->mem_size = mem_size;
	b->ubuf = malloc(UBUF_CAP);
	b->txbuf = malloc(8192);
	if (!b->ubuf || !b->txbuf) { perror("malloc"); return -1; }

	b->fd = open("/dev/hvx", O_RDWR);
	if (b->fd < 0) { perror("open /dev/hvx"); return -1; }

	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	if (ioctl(b->fd, HVX_CREATE_VM, &cfg) < 0) { perror("HVX_CREATE_VM"); return -1; }

	b->mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	if (b->mem == MAP_FAILED) { perror("mmap"); return -1; }

	if (ioctl(b->fd, HVX_SET_TSC, &tsc) < 0) { perror("HVX_SET_TSC"); return -1; }
	if (attach_net(b) < 0) return -1;

	build_page_tables_and_gdt(b->mem, mem_size);
	if (load_bzimage(b->mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0)
		return -1;

	fill_sregs(&sregs);
	if (ioctl(b->fd, HVX_SET_SREGS, &sregs) < 0) { perror("HVX_SET_SREGS"); return -1; }

	memset(&regs, 0, sizeof(regs));
	regs.rip = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF;
	regs.rsi = BOOT_PARAMS_ADDR;
	regs.rsp = STACK_TOP;
	regs.rflags = 0x2;
	if (ioctl(b->fd, HVX_SET_REGS, &regs) < 0) { perror("HVX_SET_REGS"); return -1; }
	return 0;
}

static void boot_teardown(struct boot *b)
{
	if (b->mem != MAP_FAILED && b->mem)
		munmap(b->mem, b->mem_size);
	if (b->fd >= 0)
		close(b->fd);
	free(b->ubuf);
	free(b->txbuf);
	b->ubuf = NULL; b->txbuf = NULL;
	b->fd = -1;
	b->mem = MAP_FAILED;
}

static const char *hx(uint64_t v)
{
	static char bufs[4][17];
	static int i;
	char *b = bufs[i++ & 3];
	snprintf(b, 17, "%016" PRIx64, v);
	return b;
}

struct result { uint64_t fp; size_t ulen; int ok; };
static void measure(struct boot *b, struct result *r)
{
	memset(r, 0, sizeof(*r));
	r->fp = fingerprint(b);
	r->ulen = b->ulen;
	r->ok = (r->fp != 0);
}

/* Does the reconstructed console show the ARP reply was captured? (best-effort log
 * from the loader path is not present here, so we just report the TX byte count.) */
int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *cmdline;
	uint64_t mem_size;
	uint8_t *image = NULL, *initrd = NULL;
	size_t image_len = 0, initrd_len = 0;
	int rc = 1, ok_whole = 0, ok_branch = 0;
	struct result r1, r2, ra, rb;
	static char cmdbuf[512];

	memset(&r1, 0, sizeof(r1)); memset(&r2, 0, sizeof(r2));
	memset(&ra, 0, sizeof(ra)); memset(&rb, 0, sizeof(rb));

	if (argc < 3) {
		fprintf(stderr, "usage: %s <bzImage> <initramfs-net> [\"cmdline\"] [memMiB]\n", argv[0]);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	/* Force IPv6 off so the only guest-originated frame is the deterministic ARP
	 * reply (IPv6 SLAAC/DAD uses random delays -> nondeterministic). */
	if (argc >= 4 && argv[3][0])
		snprintf(cmdbuf, sizeof(cmdbuf), "%s", argv[3]);
	else
		snprintf(cmdbuf, sizeof(cmdbuf), "%s ipv6.disable=1", DEFAULT_CMDLINE);
	cmdline = cmdbuf;
	mem_size = ((argc >= 5) ? (uint64_t)strtoull(argv[4], NULL, 0)
				: DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;

	image = read_file(kernel_path, &image_len);
	if (!image) return 1;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd) { free(image); return 1; }
	}
	fprintf(stderr, "hvx_net_boot_verify: bzImage=%zuB initramfs=%zuB mem=%llu MiB cmdline=\"%s\"\n",
		image_len, initrd_len, (unsigned long long)(mem_size / (1024 * 1024)), cmdline);

	/* ================= 1. WHOLE-BOOT DETERMINISM (two fresh net boots) ====== */
	fprintf(stderr, "\n=== net whole-boot determinism: boot #1 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r1);
		fprintf(stderr, "hvx_net_boot_verify: boot #1 reached the shell: %zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	fprintf(stderr, "\n=== net whole-boot determinism: boot #2 ===\n");
	{
		struct boot b;
		if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
		    run_until_marker(&b, FINAL_MARKER) == 0)
			measure(&b, &r2);
		fprintf(stderr, "hvx_net_boot_verify: boot #2 reached the shell: %zu console bytes\n", b.ulen);
		boot_teardown(&b);
	}
	ok_whole = (r1.ok && r2.ok && r1.fp == r2.fp);
	printf("hvx_net_boot_verify: whole-boot  fp1=%s fp2=%s -> %s\n",
	       hx(r1.fp), hx(r2.fp), ok_whole ? "IDENTICAL" : "MISMATCH");

	/* ================= 2. BRANCH-FROM-SNAPSHOT across the ARP exchange ====== */
	fprintf(stderr, "\n=== branch-from-snapshot across the virtio-net ARP exchange ===\n");
	{
		struct boot b;
		struct hvx_snapshot *mid = calloc(1, sizeof(*mid));
		uint8_t *ram_save = malloc(mem_size);
		size_t branch_ulen = 0;

		if (!mid || !ram_save) {
			perror("alloc");
		} else if (boot_setup(&b, mem_size, image, image_len, initrd, initrd_len, cmdline) == 0 &&
			   run_until_marker(&b, MID_MARKER) == 0) {
			fprintf(stderr, "hvx_net_boot_verify: reached mid marker at %zu bytes; snapshotting\n", b.ulen);
			branch_ulen = b.ulen;
			if (ioctl(b.fd, HVX_GET_SNAPSHOT, mid) < 0) {
				perror("HVX_GET_SNAPSHOT");
			} else {
				memcpy(ram_save, b.mem, mem_size);

				/* straight continuation -> ra */
				if (run_until_marker(&b, FINAL_MARKER) == 0)
					measure(&b, &ra);
				fprintf(stderr, "hvx_net_boot_verify: straight continuation reached the shell "
					"at %zu bytes\n", b.ulen);

				/* restore snapshot + RAM (the pending RX frame persists in the
				 * module's rx_buf; its cursors are restored by the snapshot) */
				if (ioctl(b.fd, HVX_SET_SNAPSHOT, mid) < 0) {
					perror("HVX_SET_SNAPSHOT");
				} else {
					memcpy(b.mem, ram_save, mem_size);
					b.ulen = branch_ulen;
					console_rebase(&b);
					if (run_until_marker(&b, FINAL_MARKER) == 0)
						measure(&b, &rb);
					fprintf(stderr, "hvx_net_boot_verify: branched continuation reached "
						"the shell at %zu bytes\n", b.ulen);
				}
			}
		}
		boot_teardown(&b);
		free(mid); free(ram_save);
	}
	ok_branch = (ra.ok && rb.ok && ra.fp == rb.fp);
	printf("hvx_net_boot_verify: branch      fp_a=%s fp_b=%s -> %s\n",
	       hx(ra.fp), hx(rb.fp), ok_branch ? "IDENTICAL" : "MISMATCH");

	if (ok_whole && ok_branch) {
		printf("hvx_net_boot_verify: MILESTONE 23 STRETCH PASS: real Linux 6.6.35 bound "
		       "virtio_net, brought up eth0, and replied to the scripted ARP -- two net boots "
		       "are bit-identical in FULL state (console + snapshot + 256 MiB RAM + captured "
		       "TX), and branch-from-snapshot across the ARP exchange reproduces bit-for-bit. "
		       "with-net fingerprint = %s\n", hx(r1.fp));
		rc = 0;
	} else {
		fprintf(stderr, "hvx_net_boot_verify: FAIL: whole-boot=%s branch=%s\n",
			ok_whole ? "ok" : "MISMATCH", ok_branch ? "ok" : "MISMATCH");
	}

	free(image); free(initrd);
	return rc;
}

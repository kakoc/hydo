// SPDX-License-Identifier: GPL-2.0
/*
 * hvx_linux - Phase-5 userspace loader: boot a real Linux bzImage as an L2 guest
 * on the from-scratch hvx hypervisor via /dev/hvx.
 * =============================================================================
 *
 * WHAT THIS IS
 * ------------
 * This is the "mini-KVM" client that replaces hydo's KVM-based Linux boot path
 * (src/linux.rs / src/detlinux.rs) with one driven entirely by the hvx char
 * device. It performs, from userspace, exactly the setup those Rust modules do
 * against KVM:
 *
 *   1. read a bzImage + initramfs into buffers;
 *   2. size + mmap guest RAM through /dev/hvx (guest-phys == mmap offset);
 *   3. build the 64-bit boot environment IN guest RAM: page tables, GDT, the
 *      Linux boot_params "zero page", the kernel command line, the e820 map,
 *      an Intel MP table, and the initramfs;
 *   4. copy the protected-mode kernel to 1 MiB;
 *   5. set the guest into 64-bit long mode (HVX_SET_SREGS) and point RIP at the
 *      kernel entry with RSI = boot_params (HVX_SET_REGS);
 *   6. loop HVX_RUN, printing each exit and draining the trapped serial console,
 *      until HLT / triple-fault / an exit the module did not handle.
 *
 * The byte-for-byte recipe (boot_params/e820/cmdline/initramfs/MP-table/paging/
 * GDT + initial long-mode CPU state) is ported from src/boot.rs + src/detlinux.rs
 * and now lives in the shared header hvx_boot.h (milestone 16 factored it out so
 * hvx_boot_verify.c can reuse the identical recipe). Every constant/offset there
 * cites its origin as "boot.rs:LINE" / "detlinux.rs:LINE" so the two can be diffed.
 *
 * SERIAL INPUT (milestone 18). The boot reaches the busybox "~ #" prompt, which
 * then blocks reading ttyS0. This loader now feeds that read:
 *   - interactive: if stdin is a TTY, it is put in raw, non-blocking mode and any
 *     bytes typed are pushed to the guest UART RX (HVX_PUT_INPUT) each iteration,
 *     so typing echoes and commands run at the prompt.
 *   - scripted (the automated demo): set the env var HVX_INPUT to a command
 *     string (C escapes \n \r \t \\ are honoured); once the "~ #" prompt appears
 *     the string is fed once, and the loader watches for HVX_EXPECT (default
 *     "hydo-interactive") in the console to declare the command ran. This is used
 *     for the end-to-end demo because a live TTY over two nested remote shells is
 *     unreliable; live typing is the same HVX_PUT_INPUT code path.
 *
 * STATUS: WORKING as of M14 (first real Linux serial output), M15 (boots to the
 * busybox initramfs shell prompt) and M18 (that shell is now interactive).
 *
 * Build (inside L1):  cc -O2 -Wall -o hvx_linux hvx_linux.c
 * Run   (inside L1):  sudo ./hvx_linux <bzImage> <initramfs> ["cmdline"] [memMiB]
 * Demo  (inside L1):  sudo HVX_INPUT='echo hydo-interactive\n' ./hvx_linux <bz> <initrd>
 */
#include "hvx_boot.h"
#include <termios.h>

/* ==========================================================================
 * Serial input: interactive raw-tty forwarding + scripted feed (milestone 18).
 * ========================================================================== */

static struct termios g_saved_tio;
static int g_raw = 0;

static void restore_tty(void)
{
	if (g_raw) {
		tcsetattr(STDIN_FILENO, TCSANOW, &g_saved_tio);
		g_raw = 0;
	}
}

/* Put stdin in raw, non-blocking mode so keystrokes reach the guest one at a
 * time (no line buffering, no local echo) and a read never blocks the run loop. */
static void set_raw_nonblock(void)
{
	struct termios raw;
	int fl;

	if (tcgetattr(STDIN_FILENO, &g_saved_tio) == 0) {
		raw = g_saved_tio;
		cfmakeraw(&raw);
		if (tcsetattr(STDIN_FILENO, TCSANOW, &raw) == 0) {
			g_raw = 1;
			atexit(restore_tty);
		}
	}
	fl = fcntl(STDIN_FILENO, F_GETFL, 0);
	if (fl >= 0)
		fcntl(STDIN_FILENO, F_SETFL, fl | O_NONBLOCK);
}

/* Does the guest console currently contain `needle`? Used in scripted mode to
 * wait for the shell prompt (before feeding) and for the expected command output
 * (to declare success). The console buffer is large, so keep it off the stack. */
static int console_contains(int fd, const char *needle)
{
	static struct hvx_console con;	/* 64 KiB; static, not on the stack */
	size_t nlen = strlen(needle), len, i;

	if (ioctl(fd, HVX_GET_CONSOLE, &con) < 0)
		return 0;
	len = con.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con.len;
	if (nlen == 0 || len < nlen)
		return 0;
	for (i = 0; i + nlen <= len; i++)
		if (memcmp(con.buf + i, needle, nlen) == 0)
			return 1;
	return 0;
}

/* Translate C escapes (\n \r \t \\) in the scripted input string so a shell
 * command with a trailing newline can be passed on the command line / env var. */
static size_t unescape(const char *s, char *out, size_t outsz)
{
	size_t n = 0;

	while (*s && n + 1 < outsz) {
		if (*s == '\\' && s[1]) {
			s++;
			switch (*s) {
			case 'n': out[n++] = '\n'; break;
			case 'r': out[n++] = '\r'; break;
			case 't': out[n++] = '\t'; break;
			case '\\': out[n++] = '\\'; break;
			default:  out[n++] = *s;   break;
			}
			s++;
		} else {
			out[n++] = *s++;
		}
	}
	out[n] = '\0';
	return n;
}

/* Push a byte string into the guest UART RX in HVX_INPUT_MAX-sized chunks. */
static void feed_input(int fd, const char *buf, size_t len)
{
	struct hvx_input in;
	size_t off = 0;

	while (off < len) {
		size_t chunk = len - off;

		if (chunk > HVX_INPUT_MAX)
			chunk = HVX_INPUT_MAX;
		in.len = (uint32_t)chunk;
		memcpy(in.buf, buf + off, chunk);
		if (ioctl(fd, HVX_PUT_INPUT, &in) < 0) {
			fprintf(stderr, "hvx_linux: HVX_PUT_INPUT: %s\n", strerror(errno));
			return;
		}
		off += chunk;
	}
}

static int g_arp_reply_seen;
/* Read + decode the frames the guest transmitted (milestone 23). Returns the number
 * of frames captured; sets g_arp_reply_seen if an ARP reply is found in the capture. */
static uint64_t dump_tx(int fd)
{
	uint8_t buf[8192];
	struct hvx_tx tx;

	memset(&tx, 0, sizeof(tx));
	tx.len = sizeof(buf);
	tx.data = (uint64_t)(uintptr_t)buf;
	if (ioctl(fd, HVX_GET_TX, &tx) < 0) {
		fprintf(stderr, "hvx_linux: HVX_GET_TX: %s\n", strerror(errno));
		return 0;
	}
	if (tx.count == 0)
		return 0;
	{
		size_t n = tx.len < sizeof(buf) ? tx.len : sizeof(buf), i;
		/* Frames are concatenated in the capture; scan for an ARP REPLY signature
		 * (ethertype 0x0806 + oper 0x0002 + our sender MAC/IP) anywhere in it, since
		 * an interface bring-up may transmit other frames first. */
		static const uint8_t sig[] = {
			0x08, 0x06,				/* ethertype ARP */
			0x00, 0x01, 0x08, 0x00, 0x06, 0x04,	/* htype/ptype/hlen/plen */
			0x00, 0x02,				/* oper: reply */
			0x52, 0x54, 0x00, 0x12, 0x34, 0x56,	/* sender HW = guest MAC */
			10, 0, 0, 2,				/* sender IP = 10.0.0.2 */
		};
		long at = -1;

		fprintf(stderr, "hvx_linux: captured %llu TX frame(s), %llu bytes\n",
			(unsigned long long)tx.count, (unsigned long long)tx.len);
		for (i = 0; n >= sizeof(sig) && i + sizeof(sig) <= n; i++)
			if (memcmp(buf + i, sig, sizeof(sig)) == 0) { at = (long)i; break; }
		if (at >= 0) {
			const uint8_t *f = buf + at;	/* points at the ethertype field */
			fprintf(stderr, "  -> ARP REPLY found: 10.0.0.2 is-at "
				"%02x:%02x:%02x:%02x:%02x:%02x (target IP %d.%d.%d.%d)\n",
				f[10], f[11], f[12], f[13], f[14], f[15],
				f[26], f[27], f[28], f[29]);
			g_arp_reply_seen = 1;
		} else {
			fprintf(stderr, "  (no ARP reply signature in the capture yet)\n");
		}
	}
	return tx.count;
}

/* ==========================================================================
 * main
 * ========================================================================== */

int main(int argc, char **argv)
{
	const char *kernel_path, *initrd_path, *cmdline;
	uint64_t mem_size;
	uint8_t *image = NULL, *initrd = NULL;
	size_t image_len = 0, initrd_len = 0;
	int fd = -1, rc = 1;
	uint8_t *mem = MAP_FAILED;
	struct hvx_config cfg;
	struct hvx_sregs sregs;
	struct hvx_regs regs;
	struct hvx_exit ex;
	size_t shown = 0;
	unsigned long long iters, max_iters;

	/* serial-input mode (milestone 18). Scripted (env HVX_INPUT) wins; else, if
	 * stdin is a TTY, interactive raw forwarding; else no input (original boot). */
	const char *raw_script = getenv("HVX_INPUT");
	const char *expect = getenv("HVX_EXPECT");
	char script[512];
	size_t script_len = 0;
	int interactive = 0, scripted = 0, fed = 0;
	unsigned long long fed_at_iter = 0;

	/* virtio-net stretch (milestone 23). HVX_NET set => attach the virtio-net device
	 * and push a scripted ARP request; stop once the guest's ARP reply is captured. */
	const char *net_env = getenv("HVX_NET");
	int net_mode = (net_env && net_env[0]);
	int net_done = 0;
	unsigned long long net_marker_iter = 0;
	static const uint8_t guest_mac[6] = { 0x52, 0x54, 0x00, 0x12, 0x34, 0x56 };
	/* ARP request: who-has 10.0.0.2 (the guest) tell 10.0.0.1 (sender 52:54:00:aa:bb:cc) */
	static const uint8_t arp_request[42] = {
		0xff, 0xff, 0xff, 0xff, 0xff, 0xff,		/* eth dst: broadcast */
		0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc,		/* eth src: sender */
		0x08, 0x06,					/* ethertype: ARP */
		0x00, 0x01,					/* htype: Ethernet */
		0x08, 0x00,					/* ptype: IPv4 */
		0x06, 0x04,					/* hlen 6, plen 4 */
		0x00, 0x01,					/* oper: request */
		0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc,		/* sender HW */
		10, 0, 0, 1,					/* sender IP 10.0.0.1 */
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00,		/* target HW: unknown */
		10, 0, 0, 2,					/* target IP 10.0.0.2 (guest) */
	};

	if (argc < 3) {
		fprintf(stderr,
			"usage: %s <bzImage> <initramfs> [\"cmdline\"] [memMiB]\n"
			"  bzImage    : x86-64 Linux kernel (bzImage)\n"
			"  initramfs  : cpio(.gz) initramfs (use \"\" or \"-\" to omit)\n"
			"  cmdline    : kernel command line (default: %s)\n"
			"  memMiB     : guest RAM in MiB (default: %lu)\n"
			"  env HVX_INPUT=\"cmd\\n\"  : scripted input fed at the shell prompt\n"
			"  env HVX_EXPECT=\"str\"   : console marker that means the cmd ran\n",
			argv[0], DEFAULT_CMDLINE, DEFAULT_MEM_MIB);
		return 2;
	}
	kernel_path = argv[1];
	initrd_path = argv[2];
	cmdline     = (argc >= 4 && argv[3][0]) ? argv[3] : DEFAULT_CMDLINE;
	mem_size    = ((argc >= 5) ? (uint64_t)strtoull(argv[4], NULL, 0)
				   : DEFAULT_MEM_MIB) * 1024ULL * 1024ULL;
	if (mem_size == 0)
		mem_size = DEFAULT_MEM_MIB * 1024ULL * 1024ULL;

	if (raw_script && raw_script[0]) {
		scripted = 1;
		script_len = unescape(raw_script, script, sizeof(script));
		if (!expect)
			expect = "hydo-interactive";
	} else if (isatty(STDIN_FILENO)) {
		set_raw_nonblock();
		interactive = 1;
	}

	/* --- read the kernel + (optional) initramfs into host buffers. --- */
	image = read_file(kernel_path, &image_len);
	if (!image)
		goto out;
	if (initrd_path[0] && strcmp(initrd_path, "-") != 0) {
		initrd = read_file(initrd_path, &initrd_len);
		if (!initrd)
			goto out;
	}

	/* --- create the VM + map its RAM (hvx_abi.h usage sequence). --- */
	fd = open("/dev/hvx", O_RDWR);
	if (fd < 0) {
		fprintf(stderr, "hvx_linux: open /dev/hvx: %s\n", strerror(errno));
		goto out;
	}
	memset(&cfg, 0, sizeof(cfg));
	cfg.mem_size = mem_size;
	/* Deterministic idle heartbeat (M27): opt in when HVX_IDLE_HEARTBEAT is set (a
	 * co-located loopback service like redis needs it; OFF by default so every other
	 * boot -- incl. the no-disk determinism baseline -- stays byte-identical). */
	if (getenv("HVX_IDLE_HEARTBEAT"))
		cfg.flags |= HVX_VM_IDLE_HEARTBEAT;
	/* x86-64-v2 CPUID level (M31): opt in when HVX_CPU_V2 is set (a real glibc userland
	 * -- a distro OpenJDK JRE -- needs it; OFF by default so every other boot stays
	 * byte-identical). */
	if (getenv("HVX_CPU_V2"))
		cfg.flags |= HVX_VM_X86_64_V2;
	/* guest AVX/AVX2 (x86-64-v3): opt in when HVX_CPU_V3 is set; off by default */
	if (getenv("HVX_CPU_V3"))
		cfg.flags |= HVX_VM_X86_64_V3;
	if (ioctl(fd, HVX_CREATE_VM, &cfg) < 0) {
		fprintf(stderr, "hvx_linux: HVX_CREATE_VM(%#" PRIx64 "): %s\n",
			mem_size, strerror(errno));
		goto out;
	}
	mem = mmap(NULL, mem_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (mem == MAP_FAILED) {
		fprintf(stderr, "hvx_linux: mmap(%#" PRIx64 "): %s\n",
			mem_size, strerror(errno));
		goto out;
	}

	/* --- deterministic virtual TSC for the boot (milestone 15). COUNTER mode
	 * with a coarse per-exit step so the LAPIC TSC-deadline timer -- armed in
	 * ticks of the 1.6 GHz frequency we advertise via CPUID.15H -- comes due
	 * within a bounded number of VM-exits during free-run (the module also
	 * jumps time to the deadline on an idle HLT). --- */
	{
		struct hvx_tsc tsc = { .tsc = 0, .step = 0x10000, .aux = 0,
				       .mode = HVX_TSC_COUNTER };
		if (ioctl(fd, HVX_SET_TSC, &tsc) < 0) {
			fprintf(stderr, "hvx_linux: HVX_SET_TSC: %s\n", strerror(errno));
			goto out;
		}
	}

	/* --- optional virtio-blk disk (milestone 21). If HVX_DISK names a file, load
	 * it as the backing image and attach it, ENABLING the virtio-blk PCI function;
	 * real Linux enumerates it, binds virtio_pci + virtio_blk, and reads /dev/vda.
	 * HVX_DISK_NO_IDMAP: Linux builds its own page tables + ioremaps MMIO, so the
	 * module must NOT poke the 4th-GiB identity map into the guest's tables. --- */
	{
		const char *disk_path = getenv("HVX_DISK");

		if (disk_path && disk_path[0]) {
			size_t disk_len = 0;
			uint8_t *disk = read_file(disk_path, &disk_len);
			struct hvx_disk dk;

			if (!disk)
				goto out;
			dk.len = disk_len;
			dk.data = (uint64_t)(uintptr_t)disk;
			dk.flags = HVX_DISK_NO_IDMAP;
			if (ioctl(fd, HVX_SET_DISK, &dk) < 0) {
				fprintf(stderr, "hvx_linux: HVX_SET_DISK(%zu): %s\n",
					disk_len, strerror(errno));
				free(disk);
				goto out;
			}
			free(disk);
			fprintf(stderr, "hvx_linux: attached virtio-blk disk %s (%zu bytes)\n",
				disk_path, disk_len);

			/* --- optional deterministic fault injection (milestone 22). If
			 * HVX_FAULT names a seed, install a SEEDED fault policy so the real
			 * kernel's /dev/vda I/O is perturbed reproducibly. Defaults to an
			 * I/O-error class at probability 100 (every request errors), so the
			 * kernel logs blk_update_request/Buffer I/O errors DETERMINISTICALLY;
			 * HVX_FAULT_CLASSES / HVX_FAULT_PROB / HVX_FAULT_MAXDELAY override the
			 * policy. Inert (never called) when HVX_FAULT is unset. --- */
			const char *fault_seed = getenv("HVX_FAULT");

			if (fault_seed && fault_seed[0]) {
				const char *fc = getenv("HVX_FAULT_CLASSES");
				const char *fp = getenv("HVX_FAULT_PROB");
				const char *fmd = getenv("HVX_FAULT_MAXDELAY");
				struct hvx_fault f;

				memset(&f, 0, sizeof(f));
				f.seed = strtoull(fault_seed, NULL, 0);
				f.classes = fc ? (uint32_t)strtoul(fc, NULL, 0) : HVX_FAULT_ERROR;
				f.probability = fp ? (uint32_t)strtoul(fp, NULL, 0) : 100;
				f.max_delay = fmd ? (uint32_t)strtoul(fmd, NULL, 0) : 0;
				if (ioctl(fd, HVX_SET_FAULT, &f) < 0) {
					fprintf(stderr, "hvx_linux: HVX_SET_FAULT: %s\n", strerror(errno));
					goto out;
				}
				fprintf(stderr, "hvx_linux: fault policy seed=0x%llx classes=0x%x prob=%u maxdelay=%u\n",
					(unsigned long long)f.seed, f.classes, f.probability, f.max_delay);
			}
		}
	}

	/* --- optional virtio-net device (milestone 23). If HVX_NET is set, attach the
	 * virtio-net PCI function (HVX_NET_NO_IDMAP -- real Linux builds its own page
	 * tables) and push a scripted ARP request into the RX FIFO. Linux binds
	 * virtio_net, brings up eth0 with a static IP (the net /init), and replies; the
	 * reply is captured into the TX buffer (dumped at the end). The whole path is
	 * deterministic: the ARP is delivered at the receiveq kick (virtnet_open) and
	 * the reply's TX capture is a pure function of the instruction clock. --- */
	if (net_mode) {
		struct hvx_net_config nc;
		struct hvx_rx rx;

		memset(&nc, 0, sizeof(nc));
		memcpy(nc.mac, guest_mac, 6);
		nc.flags = HVX_NET_NO_IDMAP | HVX_NET_PACED;	/* trickle RX for a real OS */
		if (ioctl(fd, HVX_NET_ATTACH, &nc) < 0) {
			fprintf(stderr, "hvx_linux: HVX_NET_ATTACH: %s\n", strerror(errno));
			goto out;
		}
		/* --- M24 stretch: optional deterministic net fault. If HVX_NET_FAULT names a
		 * seed, install a SEEDED net-fault policy so the scripted ARP is perturbed
		 * reproducibly. Defaults to class DROP at probability 100 (every scripted ARP
		 * copy is dropped => the guest never receives it => NO ARP reply is captured,
		 * a deterministic behavior change vs the un-faulted boot which answers).
		 * HVX_NET_FAULT_CLASSES / HVX_NET_FAULT_PROB / HVX_NET_FAULT_MAXDELAY override.
		 * Inert (never called) when HVX_NET_FAULT is unset. --- */
		{
			const char *nf = getenv("HVX_NET_FAULT");

			if (nf && nf[0]) {
				const char *nc = getenv("HVX_NET_FAULT_CLASSES");
				const char *np = getenv("HVX_NET_FAULT_PROB");
				const char *nmd = getenv("HVX_NET_FAULT_MAXDELAY");
				struct hvx_net_fault f;

				memset(&f, 0, sizeof(f));
				f.seed = strtoull(nf, NULL, 0);
				f.classes = nc ? (uint32_t)strtoul(nc, NULL, 0) : HVX_NFAULT_DROP;
				f.probability = np ? (uint32_t)strtoul(np, NULL, 0) : 100;
				f.max_delay = nmd ? (uint32_t)strtoul(nmd, NULL, 0) : 0;
				if (ioctl(fd, HVX_SET_NET_FAULT, &f) < 0) {
					fprintf(stderr, "hvx_linux: HVX_SET_NET_FAULT: %s\n", strerror(errno));
					goto out;
				}
				fprintf(stderr, "hvx_linux: net fault policy seed=0x%llx classes=0x%x prob=%u maxdelay=%u\n",
					(unsigned long long)f.seed, f.classes, f.probability, f.max_delay);
			}
		}
		/* Push SEVERAL copies of the ARP request. The first is delivered at the
		 * receiveq kick inside virtnet_open -- before NETDEV_UP fully activates the
		 * IP, so arp_process may drop it. Linux refills + re-kicks the receiveq after
		 * each NAPI poll, so a later copy is delivered once the interface is fully up
		 * and gets answered. Delivery of every copy is at a deterministic kick, so
		 * the (eventual) reply is still a pure function of the instruction clock. */
		{
			int k;

			memset(&rx, 0, sizeof(rx));
			rx.len = sizeof(arp_request);
			memcpy(rx.buf, arp_request, sizeof(arp_request));
			for (k = 0; k < 6; k++)
				if (ioctl(fd, HVX_PUT_RX, &rx) < 0) {
					fprintf(stderr, "hvx_linux: HVX_PUT_RX: %s\n", strerror(errno));
					goto out;
				}
		}
		fprintf(stderr, "hvx_linux: attached virtio-net (mac %02x:%02x:%02x:%02x:%02x:%02x), "
			"pushed 6x a %zu-byte ARP request (who-has 10.0.0.2 tell 10.0.0.1)\n",
			guest_mac[0], guest_mac[1], guest_mac[2], guest_mac[3], guest_mac[4],
			guest_mac[5], sizeof(arp_request));
	}

	/* --- build the guest boot environment in RAM. --- */
	build_page_tables_and_gdt(mem, mem_size);
	if (load_bzimage(mem, mem_size, image, image_len, initrd, initrd_len, cmdline) < 0)
		goto out;

	/* --- install long-mode system + general registers. --- */
	fill_sregs(&sregs);
	if (ioctl(fd, HVX_SET_SREGS, &sregs) < 0) {
		fprintf(stderr, "hvx_linux: HVX_SET_SREGS: %s\n", strerror(errno));
		goto out;
	}

	/* HVX_SET_REGS carries the GPRs + rip/rsp/rflags. RSI must hold the
	 * boot_params pointer for the 64-bit Linux entry (detlinux.rs:245-249). */
	memset(&regs, 0, sizeof(regs));
	regs.rip    = KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF; /* 0x100200 */
	regs.rsi    = BOOT_PARAMS_ADDR;                    /* 0x10000  */
	regs.rsp    = STACK_TOP;                           /* 0x8000   */
	regs.rflags = 0x2;                                 /* reserved bit; IF clear */
	if (ioctl(fd, HVX_SET_REGS, &regs) < 0) {
		fprintf(stderr, "hvx_linux: HVX_SET_REGS: %s\n", strerror(errno));
		goto out;
	}

	fprintf(stderr, "hvx_linux: entering guest: rip=%#llx rsi=%#llx rsp=%#llx "
		"cr3=%#llx (mem %llu MiB)\n",
		(unsigned long long)regs.rip, (unsigned long long)regs.rsi,
		(unsigned long long)regs.rsp, (unsigned long long)sregs.cr3,
		(unsigned long long)(mem_size / (1024 * 1024)));
	if (scripted)
		fprintf(stderr, "hvx_linux: scripted input armed (%zu bytes); "
			"waiting for the \"~ #\" prompt\n", script_len);
	else if (interactive)
		fprintf(stderr, "hvx_linux: interactive: type at the prompt "
			"(input forwarded to the guest ttyS0)\n");
	fprintf(stderr, "hvx_linux: ---- serial console ----\n");

	/* --- run loop: free-run with HVX_RUN. The guest is NOT single-stepped, so
	 * it will exceed the module's 65536-in-kernel-exit cap and return
	 * HVX_EXIT_THROTTLE many times before reaching a real exit; that is NORMAL
	 * (M13 makes guest state persist across re-entry), so we simply re-run on
	 * THROTTLE. We stop and report on HLT / triple-fault / VMFAIL / any exit the
	 * module did not resolve, printing reason+qualification+guest_phys+guest RIP
	 * (RIP fetched via HVX_GET_REGS since hvx_exit does not carry it). --- */
	max_iters = 100000000ULL; /* safety cap against a silent spin */
	{
	unsigned long long throttles = 0;
	for (iters = 0; iters < max_iters; iters++) {
		struct hvx_regs cur;
		unsigned long long rip = 0;

		if (ioctl(fd, HVX_RUN, &ex) < 0) {
			if (errno == EINTR)
				continue;
			fprintf(stderr, "\nhvx_linux: HVX_RUN: %s\n", strerror(errno));
			goto out;
		}

		/* Show any newly-trapped serial output first. */
		drain_console(fd, &shown);

		/* Serial input (milestone 18). Interactive: forward stdin bytes as
		 * they arrive. Scripted: once the "~ #" prompt is up (the shell is
		 * reading ttyS0, past its startup RBR flush), feed the command once. */
		if (interactive) {
			char kb[HVX_INPUT_MAX];
			ssize_t n = read(STDIN_FILENO, kb, sizeof(kb));

			if (n > 0)
				feed_input(fd, kb, (size_t)n);
		} else if (scripted && !fed && console_contains(fd, "~ #")) {
			feed_input(fd, script, script_len);
			fed = 1;
			fed_at_iter = iters;
			fprintf(stderr, "\nhvx_linux: [fed %zu scripted bytes at the prompt]\n",
				script_len);
		}

		if (ex.reason == HVX_EXIT_THROTTLE) {
			/* In-kernel run-loop bound hit; keep running. Emit a sparse
			 * heartbeat to stderr so a long silent stretch is visibly
			 * making progress rather than wedged. */
			if ((++throttles % 256) == 0)
				fprintf(stderr, "[hvx_linux: %llu throttles, still running]\n",
					throttles);

			/* Net demo (milestone 23): once the interface is configured, poll for
			 * the guest's ARP reply captured in the TX buffer; stop when it lands. */
			if (net_mode && !net_done) {
				if (!net_marker_iter && console_contains(fd, "hydo-net-marker")) {
					net_marker_iter = iters;
					fprintf(stderr, "\nhvx_linux: [net interface configured; "
						"waiting for the ARP reply on the transmitq]\n");
				}
				if (net_marker_iter) {
					dump_tx(fd);
					if (g_arp_reply_seen) {
						drain_console(fd, &shown);
						fprintf(stderr, "\nhvx_linux: DEMO PASS: guest replied to the "
							"scripted ARP request; reply captured on the transmitq\n");
						net_done = 1;
						rc = 0;
						break;
					}
				}
				if (net_marker_iter && iters - net_marker_iter > 800000ULL) {
					fprintf(stderr, "\nhvx_linux: DEMO: no ARP reply captured within "
						"budget after the interface came up\n");
					break;
				}
				continue;
			}

			/* Scripted demo: after the command was fed, stop once its output
			 * appears (success), or after a bounded budget (report + dump). */
			if (scripted && fed) {
				if (console_contains(fd, expect)) {
					drain_console(fd, &shown);
					fprintf(stderr, "\nhvx_linux: DEMO PASS: guest shell "
						"consumed the input and produced \"%s\"\n", expect);
					rc = 0;
					break;
				}
				if (iters - fed_at_iter > 400000ULL) {
					fprintf(stderr, "\nhvx_linux: DEMO: expected output "
						"\"%s\" not seen within budget after feeding\n",
						expect);
					break;
				}
			}
			continue;
		}

		/* We are about to stop or report: fetch the guest RIP for context. */
		if (ioctl(fd, HVX_GET_REGS, &cur) == 0)
			rip = (unsigned long long)cur.rip;

		if (ex.reason == EXIT_HLT) {
			fprintf(stderr, "\nhvx_linux: guest HLT after %llu exits "
				"(rip=%#llx)\n", iters + 1, rip);
			rc = 0;
			break;
		}
		if (ex.reason == EXIT_TRIPLE_FAULT) {
			fprintf(stderr, "\nhvx_linux: guest TRIPLE FAULT "
				"(qual=%#" PRIx64 " gpa=%#" PRIx64 " rip=%#llx)\n",
				ex.qualification, ex.guest_phys, rip);
			dump_fault_insn(mem, mem_size, rip);
			break;
		}
		if (ex.reason == HVX_EXIT_VMFAIL) {
			fprintf(stderr, "\nhvx_linux: VMFAIL: VM-instruction error %" PRIu64
				" (VMLAUNCH/VMRESUME failed) rip=%#llx\n",
				ex.qualification, rip);
			break;
		}

		/* Anything else is an exit the module did not resolve internally.
		 * Report it (reason/qualification/faulting GPA/RIP) and stop - we
		 * cannot emulate it from here. For an EPT violation the guest_phys is
		 * the faulting GPA: 0xFEE00000 (LAPIC) / 0xFEC00000 (IOAPIC) means we
		 * reached the expected Phase-6 APIC-MMIO wall. */
		fprintf(stderr, "\nhvx_linux: unhandled exit: reason=%u (%s) "
			"insn_len=%u qual=%#" PRIx64 " gpa=%#" PRIx64 " rip=%#llx\n",
			ex.reason, exit_reason_name(ex.reason), ex.insn_len,
			ex.qualification, ex.guest_phys, rip);
		dump_fault_insn(mem, mem_size, rip);
		break;
	}
	}
	if (iters >= max_iters)
		fprintf(stderr, "\nhvx_linux: exit cap (%llu) reached, stopping\n", max_iters);

	/* Final drain + dump of the accumulated console. */
	drain_console(fd, &shown);
	{
		struct hvx_console con;
		if (ioctl(fd, HVX_GET_CONSOLE, &con) == 0) {
			size_t n = con.len > HVX_CONSOLE_SZ ? HVX_CONSOLE_SZ : con.len;
			fprintf(stderr, "\nhvx_linux: ---- console (%u bytes total) ----\n",
				con.len);
			fwrite(con.buf, 1, n, stdout);
			fflush(stdout);
			fprintf(stderr, "\nhvx_linux: -----------------------------------\n");
		}
	}

out:
	restore_tty();
	if (mem != MAP_FAILED)
		munmap(mem, mem_size);
	if (fd >= 0)
		close(fd);
	free(image);
	free(initrd);
	return rc;
}

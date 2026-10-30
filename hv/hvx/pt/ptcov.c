// SPDX-License-Identifier: GPL-2.0
/*
 * ptcov -- decode an hvx Intel PT guest trace into the set of executed basic blocks.
 *
 * Usage: ptcov <trace.bin> [--ram ram.bin VA_BASE SIZE ...] [--elf FILE ...] > blocks.txt
 *   --ram ram.bin VA SIZE   code bytes come from a guest RAM dump: VA maps to RAM offset
 *                           VA - 0xffffffff80000000 (the guest kernel's direct text map);
 *                           repeatable for several regions
 *   --elf FILE              code bytes come from the executable segments of an ELF file
 *                           at their link addresses (a non-PIE binary such as hydostack)
 *
 * Output (stdout): the address of every executed instruction, one per line (hex),
 * sorted. Instruction-level, not block-level: a VM exit cuts the trace (and so a
 * decoded block) at a host-timed point, so block boundaries differ run to run while the
 * set of executed instructions does not. A summary goes to stderr: instructions
 * decoded, unique instructions, decode errors (an error resyncs at the next PSB, so
 * coverage there is lost, not wrong).
 *
 * Built with libipt (Intel's reference decoder): cc -O2 -o ptcov ptcov.c -lipt
 */
#include <intel-pt.h>
#include <elf.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <inttypes.h>

#define KTEXT_MAP 0xffffffff80000000ULL

/* Code regions held in memory (a file read once): libipt asks for bytes through a
 * callback, which avoids its per-access file I/O. */
struct region { uint64_t va, size; const uint8_t *bytes; uint8_t *hit; };
static struct region regs_[64];
static int nregs;

static int read_mem(uint8_t *buf, size_t size, const struct pt_asid *asid, uint64_t ip, void *ctx)
{
	int i;

	(void)asid; (void)ctx;
	for (i = 0; i < nregs; i++)
		if (ip >= regs_[i].va && ip < regs_[i].va + regs_[i].size) {
			uint64_t n = regs_[i].va + regs_[i].size - ip;

			if (n > size)
				n = size;
			memcpy(buf, regs_[i].bytes + (ip - regs_[i].va), n);
			return (int)n;
		}
	return -pte_nomap;
}
static uint64_t err_hist[64], err_dec[10];

static uint8_t *slurp(const char *path, size_t *len)
{
	FILE *f = fopen(path, "rb");
	uint8_t *buf;
	long n;

	if (!f) { perror(path); exit(1); }
	fseek(f, 0, SEEK_END);
	n = ftell(f);
	fseek(f, 0, SEEK_SET);
	buf = malloc(n ? n : 1);
	if (!buf || fread(buf, 1, n, f) != (size_t)n) { perror(path); exit(1); }
	fclose(f);
	*len = n;
	return buf;
}

static void add_elf(struct pt_image *img, const char *path)
{
	size_t len;
	uint8_t *f = slurp(path, &len);
	Elf64_Ehdr *eh = (Elf64_Ehdr *)f;
	int i, n = 0;

	if (len < sizeof(*eh) || memcmp(eh->e_ident, ELFMAG, SELFMAG) || eh->e_ident[EI_CLASS] != ELFCLASS64) {
		fprintf(stderr, "ptcov: %s is not a 64-bit ELF\n", path);
		exit(1);
	}
	for (i = 0; i < eh->e_phnum; i++) {
		Elf64_Phdr *ph = (Elf64_Phdr *)(f + eh->e_phoff + (size_t)i * eh->e_phentsize);
		int st;

		if (ph->p_type != PT_LOAD || !(ph->p_flags & PF_X) || !ph->p_filesz)
			continue;
		(void)st;
		regs_[nregs++] = (struct region){ ph->p_vaddr, ph->p_filesz, f + ph->p_offset };
		fprintf(stderr, "ptcov: code from %s: [%#" PRIx64 ", %#" PRIx64 ")\n", path,
			(uint64_t)ph->p_vaddr, (uint64_t)(ph->p_vaddr + ph->p_filesz));
		n++;
	}
	if (!n) { fprintf(stderr, "ptcov: %s has no executable segment\n", path); exit(1); }
}

int main(int argc, char **argv)
{
	struct pt_config cfg;
	struct pt_image *img;
	struct pt_insn_decoder *dec;
	uint8_t *trace;
	size_t tlen;
	uint64_t ninsn = 0, nuniq = 0, nerr = 0, nsync = 0, i;
	int a, st;

	if (argc < 3) {
		fprintf(stderr, "usage: %s <trace.bin> [--ram ram.bin VA SIZE]... [--elf FILE]...\n", argv[0]);
		return 2;
	}
	trace = slurp(argv[1], &tlen);
	img = pt_image_alloc("guest");
	for (a = 2; a < argc; a++) {
		if (!strcmp(argv[a], "--ram") && a + 3 < argc) {
			uint64_t va = strtoull(argv[a + 2], NULL, 0), sz = strtoull(argv[a + 3], NULL, 0);
			size_t rlen;
			const uint8_t *ram = slurp(argv[a + 1], &rlen);

			if (va - KTEXT_MAP + sz > rlen) { fprintf(stderr, "ptcov: region outside the RAM dump\n"); return 1; }
			regs_[nregs++] = (struct region){ va, sz, ram + (va - KTEXT_MAP) };
			fprintf(stderr, "ptcov: code from %s: [%#" PRIx64 ", %#" PRIx64 ")\n", argv[a + 1], va, va + sz);
			a += 3;
		} else if (!strcmp(argv[a], "--elf") && a + 1 < argc) {
			add_elf(img, argv[++a]);
		} else {
			fprintf(stderr, "ptcov: bad argument %s\n", argv[a]);
			return 2;
		}
	}

	pt_config_init(&cfg);
	cfg.begin = trace;
	cfg.end = trace + tlen;
	cfg.cpu.vendor = pcv_intel;	/* decode on the tracing box: Kaby Lake, family 6 model 158 */
	cfg.cpu.family = 6;
	cfg.cpu.model = 158;
	cfg.cpu.stepping = 9;
	pt_cpu_errata(&cfg.errata, &cfg.cpu);
	dec = pt_insn_alloc_decoder(&cfg);
	if (!dec) { fprintf(stderr, "ptcov: cannot allocate the decoder\n"); return 1; }
	pt_image_set_callback(img, read_mem, NULL);
	pt_insn_set_image(dec, img);
	for (i = 0; i < (uint64_t)nregs; i++)
		regs_[i].hit = calloc(regs_[i].size / 8 + 1, 1);

	for (;;) {
		st = pt_insn_sync_forward(dec);
		if (st < 0)
			break;			/* -pte_eos: no further PSB */
		nsync++;
		for (;;) {
			struct pt_insn in;
			int r;

			while (st & pts_event_pending) {
				struct pt_event ev;

				st = pt_insn_event(dec, &ev, sizeof(ev));
				if (st < 0)
					break;
			}
			if (st < 0)
				break;
			st = pt_insn_next(dec, &in, sizeof(in));
			if (st < 0)
				break;
			ninsn++;
			for (r = 0; r < nregs; r++)
				if (in.ip >= regs_[r].va && in.ip < regs_[r].va + regs_[r].size) {
					uint64_t o = in.ip - regs_[r].va;

					regs_[r].hit[o >> 3] |= (uint8_t)(1u << (o & 7));
					break;
				}
		}
		if (st == -pte_eos)
			break;
		nerr++;			/* lost sync: resume at the next PSB */
		err_hist[(-st) & 63]++;
		{
			uint64_t off = 0;

			pt_insn_get_offset(dec, &off);
			err_dec[(off * 10) / (tlen ? tlen : 1) < 10 ? (off * 10) / tlen : 9]++;
		}
	}

	{
		uint64_t uniq = 0, o;
		int r;

		for (r = 0; r < nregs; r++)
			for (o = 0; o < regs_[r].size; o++)
				if (regs_[r].hit[o >> 3] & (1u << (o & 7))) {
					printf("%" PRIx64 "\n", regs_[r].va + o);
					uniq++;
				}
		nuniq = uniq;
	}
	fprintf(stderr, "ptcov: decode errors per tenth of the trace:");
	for (i = 0; i < 10; i++)
		fprintf(stderr, " %" PRIu64, err_dec[i]);
	fprintf(stderr, "\n");
	for (i = 0; i < 64; i++)
		if (err_hist[i])
			fprintf(stderr, "ptcov:   %" PRIu64 " x %s\n", err_hist[i], pt_errstr((enum pt_error_code)i));
	fprintf(stderr, "ptcov: trace %zu bytes, %" PRIu64 " sync points, %" PRIu64
		" instructions decoded, %" PRIu64 " unique instructions, %" PRIu64 " decode errors\n",
		tlen, nsync, ninsn, nuniq, nerr);
	return 0;
}

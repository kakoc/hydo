// SPDX-License-Identifier: GPL-2.0
/*
 * ptedges.h -- cheap per-run coverage feedback for the explorer (Milestone 41).
 *
 * Decodes an hvx Intel PT trace of ONE program (the front door, a static non-PIE ELF
 * linked at a unique address so the trace's IP filter holds that program only) into the
 * set of control-flow EDGES it executed, with hit counts: edge = (address of the branch
 * instruction that ended a basic block, address of the next block). This is AFL's notion
 * of coverage; the hit counts are bucketed by the reader (explore/guided.py).
 *
 * Uses libipt's INSTRUCTION decoder: its instruction stream is identical run to run (checked:
 * two traces of one schedule decode to the same 79,825,286 instructions), which the block
 * decoder's block boundaries are not -- they follow the trace's on/off points.
 * An edge is recorded between two consecutively executed instructions of the program when
 * the first is a branch-class instruction. Tracing goes off and on again at every VM
 * exit (at host-timed points) and whenever the program leaves its own text (a syscall, a
 * call into the vDSO); the edge chain is KEPT across both. A VM exit splits a block at an
 * arbitrary instruction, but a block that ends there ends with a non-branch and yields no
 * edge, and the branch that really ends it yields the same edge as without the exit; a
 * syscall or an out-of-program call yields the edge (call site -> return point), which is
 * itself a pure function of the program's execution. Only lost trace data (overflow)
 * breaks the chain. So the edge set and its counts depend only on what the guest ran.
 *
 * Output file: one line per edge, "<from-hex> <to-hex> <count>", sorted.
 */
#ifndef HYDO_NO_LIBIPT
#include <intel-pt.h>
#endif
#include <elf.h>

struct pte_region { uint64_t va, size; const uint8_t *bytes; };
static struct pte_region pte_regs[8];
static int pte_nregs;
static uint8_t *pte_elf;

#ifndef HYDO_NO_LIBIPT
static int pte_read_mem(uint8_t *buf, size_t size, const struct pt_asid *asid, uint64_t ip, void *ctx)
{
	int i;

	(void)asid; (void)ctx;
	for (i = 0; i < pte_nregs; i++)
		if (ip >= pte_regs[i].va && ip < pte_regs[i].va + pte_regs[i].size) {
			uint64_t n = pte_regs[i].va + pte_regs[i].size - ip;

			if (n > size)
				n = size;
			memcpy(buf, pte_regs[i].bytes + (ip - pte_regs[i].va), n);
			return (int)n;
		}
	return -pte_nomap;
}
#endif

/* Load the executable segments of the traced program. Returns 0 and the text range. */
static int pte_load_elf(const char *path, uint64_t *lo, uint64_t *hi)
{
	FILE *f = fopen(path, "rb");
	long n;
	Elf64_Ehdr *eh;
	int i;

	if (!f) { perror(path); return -1; }
	fseek(f, 0, SEEK_END); n = ftell(f); fseek(f, 0, SEEK_SET);
	pte_elf = malloc(n);
	if (!pte_elf || fread(pte_elf, 1, n, f) != (size_t)n) { fclose(f); return -1; }
	fclose(f);
	eh = (Elf64_Ehdr *)pte_elf;
	if (memcmp(eh->e_ident, ELFMAG, SELFMAG)) return -1;
	*lo = ~0ULL; *hi = 0;
	for (i = 0; i < eh->e_phnum && pte_nregs < 8; i++) {
		Elf64_Phdr *ph = (Elf64_Phdr *)(pte_elf + eh->e_phoff + (size_t)i * eh->e_phentsize);

		if (ph->p_type != PT_LOAD || !(ph->p_flags & PF_X) || !ph->p_filesz)
			continue;
		pte_regs[pte_nregs++] = (struct pte_region){ ph->p_vaddr, ph->p_filesz, pte_elf + ph->p_offset };
		if (ph->p_vaddr < *lo) *lo = ph->p_vaddr;
		if (ph->p_vaddr + ph->p_filesz > *hi) *hi = ph->p_vaddr + ph->p_filesz;
	}
	return pte_nregs ? 0 : -1;
}

struct pte_ent { uint64_t from, to, n; };
struct pte_map { struct pte_ent *e; size_t cap, used; };

static void pte_add(struct pte_map *m, uint64_t from, uint64_t to)
{
	size_t i;

	if (m->used * 2 >= m->cap) {		/* grow + rehash */
		struct pte_map g = { calloc(m->cap ? m->cap * 2 : 1 << 14, sizeof(struct pte_ent)), m->cap ? m->cap * 2 : 1 << 14, 0 };
		size_t k;

		for (k = 0; k < m->cap; k++)
			if (m->e[k].n) {
				size_t j = ((m->e[k].from * 0x9E3779B97F4A7C15ULL) ^ m->e[k].to) & (g.cap - 1);
				while (g.e[j].n) j = (j + 1) & (g.cap - 1);
				g.e[j] = m->e[k]; g.used++;
			}
		free(m->e);
		*m = g;
	}
	i = ((from * 0x9E3779B97F4A7C15ULL) ^ to) & (m->cap - 1);
	while (m->e[i].n && (m->e[i].from != from || m->e[i].to != to))
		i = (i + 1) & (m->cap - 1);
	if (!m->e[i].n) { m->e[i].from = from; m->e[i].to = to; m->used++; }
	m->e[i].n++;
}

static int pte_cmp(const void *a, const void *b)
{
	const struct pte_ent *x = a, *y = b;
	if (x->from != y->from) return x->from < y->from ? -1 : 1;
	if (x->to != y->to) return x->to < y->to ? -1 : 1;
	return 0;
}

/* Decode trace[0..len) and write the edge file. Returns the number of edges (or -1);
 * *errs gets the number of decode errors (each resyncs at the next PSB). */
#ifndef HYDO_NO_LIBIPT
static long pte_decode(const uint8_t *trace, size_t len, const char *outpath, uint64_t *errs)
{
	struct pt_config cfg;
	struct pt_image *img = pt_image_alloc("hydostack");
	struct pt_insn_decoder *dec;
	struct pte_map m = { 0 };
	FILE *f;
	size_t k, n = 0;
	int st;

	*errs = 0;
	pt_config_init(&cfg);
	cfg.begin = (uint8_t *)trace;
	cfg.end = (uint8_t *)trace + len;
	cfg.cpu.vendor = pcv_intel;		/* the tracing box: Kaby Lake, family 6 model 158 */
	cfg.cpu.family = 6; cfg.cpu.model = 158; cfg.cpu.stepping = 9;
	pt_cpu_errata(&cfg.errata, &cfg.cpu);
	dec = pt_insn_alloc_decoder(&cfg);
	if (!dec) return -1;
	pt_image_set_callback(img, pte_read_mem, NULL);
	pt_insn_set_image(dec, img);
	for (;;) {
		uint64_t prev_ip = 0;
		int prev_branch = 0;

		st = pt_insn_sync_forward(dec);
		if (st < 0)
			break;
		for (;;) {
			struct pt_insn in;

			while (st & pts_event_pending) {
				struct pt_event ev;

				st = pt_insn_event(dec, &ev, sizeof(ev));
				if (st < 0)
					break;
				/* Only lost trace data breaks the chain. NOT ptev_tsx: every PSB+
				 * the CPU emits when tracing is switched back on after a VM exit
				 * carries a MODE.TSX packet, so resetting on it dropped one edge per
				 * exit (host-timed) and made the counts differ run to run. */
				if (ev.type == ptev_overflow)
					prev_branch = 0;
			}
			if (st < 0)
				break;
			st = pt_insn_next(dec, &in, sizeof(in));
			if (st < 0)
				break;
			if (prev_branch)
				pte_add(&m, prev_ip, in.ip);
			prev_ip = in.ip;
			prev_branch = in.iclass != ptic_other && in.iclass != ptic_error;
		}
		if (st == -pte_eos)
			break;
		(*errs)++;
	}
	pt_insn_free_decoder(dec);
	pt_image_free(img);
	for (k = 0; k < m.cap; k++)
		if (m.e[k].n) m.e[n++] = m.e[k];
	qsort(m.e, n, sizeof(m.e[0]), pte_cmp);
	if (!(f = fopen(outpath, "w"))) { free(m.e); return -1; }
	for (k = 0; k < n; k++)
		fprintf(f, "%llx %llx %llu\n", (unsigned long long)m.e[k].from,
			(unsigned long long)m.e[k].to, (unsigned long long)m.e[k].n);
	fclose(f);
	free(m.e);
	return (long)n;
}
#else
/* Built without libipt (cc -DHYDO_NO_LIBIPT, the Linux release when libipt-dev is missing):
 * coverage feedback (HVX_EXPLORE_PT_ELF, guided.py) is unavailable; every other mode works. */
static long pte_decode(const uint8_t *trace, size_t len, const char *outpath, uint64_t *errs)
{
	(void)trace; (void)len; (void)outpath;
	*errs = 0;
	fprintf(stderr, "hvx_explore: built without libipt (HYDO_NO_LIBIPT): no coverage decoding\n");
	return -1;
}
#endif

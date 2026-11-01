// SPDX-License-Identifier: GPL-2.0
/*
 * ptedges.c -- the coverage decoder of hydo-hvx's tracing (cargo feature `pt`, needs libipt).
 *
 * This is the decoding half of hv/hvx/ptedges.h (Milestone 41), which hvx_explore.c included
 * until M137 (restore both from git: `git show <M137's parent>:hv/hvx/ptedges.h`). The loop is
 * the same text; only the code regions come from the caller (hydo-hvx loads the ELF in Rust,
 * src/pt.rs) instead of static variables, so the edge file is the same byte for byte.
 *
 * Decodes an hvx Intel PT trace of ONE program (a static non-PIE ELF linked at a unique address,
 * so the trace's IP filter holds that program only) into the set of control-flow EDGES it
 * executed, with hit counts: edge = (address of the branch instruction that ended a basic block,
 * address of the next block). It uses libipt's INSTRUCTION decoder, whose instruction stream is
 * identical run to run; an edge is recorded between two consecutively executed instructions of
 * the program when the first is a branch-class instruction, and only lost trace data (overflow)
 * breaks the chain, so the edge set and its counts depend only on what the guest ran.
 *
 * Output file: one line per edge, "<from-hex> <to-hex> <count>", sorted.
 */
#include <intel-pt.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct hydo_pte_region { uint64_t va, size; const uint8_t *bytes; };
struct pte_regs { const struct hydo_pte_region *r; int n; };

static int pte_read_mem(uint8_t *buf, size_t size, const struct pt_asid *asid, uint64_t ip, void *ctx)
{
	const struct pte_regs *rs = ctx;
	int i;

	(void)asid;
	for (i = 0; i < rs->n; i++)
		if (ip >= rs->r[i].va && ip < rs->r[i].va + rs->r[i].size) {
			uint64_t n = rs->r[i].va + rs->r[i].size - ip;

			if (n > size)
				n = size;
			memcpy(buf, rs->r[i].bytes + (ip - rs->r[i].va), n);
			return (int)n;
		}
	return -pte_nomap;
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
long hydo_pte_decode(const uint8_t *trace, size_t len, const struct hydo_pte_region *regs, int nregs,
		     const char *outpath, uint64_t *errs)
{
	struct pt_config cfg;
	struct pt_image *img = pt_image_alloc("hydostack");
	struct pt_insn_decoder *dec;
	struct pte_map m = { 0 };
	struct pte_regs rs = { regs, nregs };
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
	pt_image_set_callback(img, pte_read_mem, &rs);
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

// SPDX-License-Identifier: GPL-2.0
/*
 * ptmulti -- decode ONE hvx Intel PT trace into per-program control-flow edge sets
 * (whole-system coverage for the explorer, Milestone 43 prep).
 *
 *   ptmulti <trace.bin> <outdir> <name>=<elf>[@<bias>] [<name>=<elf>[@<bias>] ...]
 *
 * Each program is an ELF whose executable PT_LOAD segments are placed at p_vaddr + bias
 * (bias 0 for a non-PIE executable, the load base for a PIE). The programs must occupy
 * disjoint address ranges -- the richcov image arranges that (hvx/richcov/build-richcov.sh:
 * Redis at 0x40001000, the front door at 0x2000001000, mysqld at the no-ASLR PIE base
 * 0x555555554000), so an address alone says which program it belongs to. The trace is
 * expected to be user-only and IP-filtered to those ranges, as the explorer configures it.
 *
 * Edges follow the M41 rule (hvx/explore/ptedges.h): an edge is recorded between two
 * consecutively executed instructions when the first is a branch-class instruction; the
 * chain is kept across trace on/off points (VM exits, leaving the program's text for a
 * syscall or a library) so the edge set is a pure function of what the guest ran. What is
 * NEW here: the chain is also cut when the next instruction belongs to a DIFFERENT program
 * (that is a process switch, not control flow), and so is an edge whose target lies outside
 * every program. Remaining limitation, shared with ptedges.h: a switch between two threads
 * of the same program cannot be seen in a user-only trace and yields one edge from the last
 * branch of one thread to the next instruction of the other; it is deterministic (the guest
 * scheduler decides it), so the set stays reproducible, but that edge is not real control flow.
 *
 * Output in <outdir>:
 *   <name>.edges   "from to count" per edge, sorted, addresses as executed (bias included)
 *   <name>.funcs   "count function" -- branch-edge hits per function (needs .symtab)
 *   summary.txt    per program: instructions, edges, functions covered; and decode errors
 * Build: cc -O2 -o ptmulti ptmulti.c -lipt
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <elf.h>
#include <intel-pt.h>

#define MAXPROG 8
#define MAXREG 32

struct region { uint64_t va, size; const uint8_t *bytes; int prog; };
struct sym { uint64_t lo, hi; const char *name; };
struct edge { uint64_t from, to, n; };
struct emap { struct edge *e; size_t cap, used; };
struct prog {
	const char *name, *path;
	uint64_t bias, lo, hi;		/* executable address range, bias included */
	uint8_t *elf;
	struct sym *syms; size_t nsyms;
	struct emap edges;
	uint64_t insns;
};

static struct prog progs[MAXPROG];
static int nprogs;
static struct region regs[MAXREG];
static int nregs;

static int read_mem(uint8_t *buf, size_t size, const struct pt_asid *asid, uint64_t ip, void *ctx)
{
	int i;

	(void)asid; (void)ctx;
	for (i = 0; i < nregs; i++)
		if (ip >= regs[i].va && ip < regs[i].va + regs[i].size) {
			uint64_t n = regs[i].va + regs[i].size - ip;

			if (n > size)
				n = size;
			memcpy(buf, regs[i].bytes + (ip - regs[i].va), n);
			return (int)n;
		}
	return -pte_nomap;
}

static int prog_of(uint64_t ip)
{
	int i;

	for (i = 0; i < nprogs; i++)
		if (ip >= progs[i].lo && ip < progs[i].hi)
			return i;
	return -1;
}

static int sym_cmp(const void *a, const void *b)
{
	const struct sym *x = a, *y = b;
	return x->lo < y->lo ? -1 : x->lo > y->lo;
}

static int load_prog(struct prog *p)
{
	FILE *f = fopen(p->path, "rb");
	long n;
	Elf64_Ehdr *eh;
	Elf64_Shdr *sh;
	int i;

	if (!f) { perror(p->path); return -1; }
	fseek(f, 0, SEEK_END); n = ftell(f); fseek(f, 0, SEEK_SET);
	p->elf = malloc(n);
	if (!p->elf || fread(p->elf, 1, n, f) != (size_t)n) { fclose(f); return -1; }
	fclose(f);
	eh = (Elf64_Ehdr *)p->elf;
	if (memcmp(eh->e_ident, ELFMAG, SELFMAG) || eh->e_ident[EI_CLASS] != ELFCLASS64) {
		fprintf(stderr, "%s: not a 64-bit ELF\n", p->path);
		return -1;
	}
	p->lo = ~0ULL; p->hi = 0;
	for (i = 0; i < eh->e_phnum; i++) {
		Elf64_Phdr *ph = (Elf64_Phdr *)(p->elf + eh->e_phoff + (size_t)i * eh->e_phentsize);
		uint64_t va = ph->p_vaddr + p->bias;

		if (ph->p_type != PT_LOAD || !(ph->p_flags & PF_X) || !ph->p_filesz)
			continue;
		if (nregs >= MAXREG) { fprintf(stderr, "too many segments\n"); return -1; }
		regs[nregs++] = (struct region){ va, ph->p_filesz, p->elf + ph->p_offset, (int)(p - progs) };
		if (va < p->lo) p->lo = va;
		if (va + ph->p_filesz > p->hi) p->hi = va + ph->p_filesz;
	}
	if (p->hi == 0) { fprintf(stderr, "%s: no executable segment\n", p->path); return -1; }
	/* function symbols from .symtab (or .dynsym if stripped) */
	sh = (Elf64_Shdr *)(p->elf + eh->e_shoff);
	for (i = 0; eh->e_shoff && i < eh->e_shnum; i++) {
		Elf64_Shdr *s = &sh[i];
		size_t k, cnt;
		const char *str;

		if (s->sh_type != SHT_SYMTAB)
			continue;
		cnt = s->sh_size / sizeof(Elf64_Sym);
		str = (const char *)(p->elf + sh[s->sh_link].sh_offset);
		p->syms = calloc(cnt, sizeof(struct sym));
		for (k = 0; k < cnt; k++) {
			Elf64_Sym *y = (Elf64_Sym *)(p->elf + s->sh_offset) + k;

			if (ELF64_ST_TYPE(y->st_info) != STT_FUNC || !y->st_value)
				continue;
			p->syms[p->nsyms++] = (struct sym){ y->st_value + p->bias,
				y->st_value + p->bias + (y->st_size ? y->st_size : 1), str + y->st_name };
		}
		qsort(p->syms, p->nsyms, sizeof(struct sym), sym_cmp);
	}
	return 0;
}

static const char *func_of(const struct prog *p, uint64_t ip)
{
	size_t lo = 0, hi = p->nsyms;

	while (lo < hi) {			/* last symbol with lo <= ip */
		size_t mid = (lo + hi) / 2;

		if (p->syms[mid].lo <= ip) lo = mid + 1; else hi = mid;
	}
	if (lo == 0)
		return NULL;
	return ip < p->syms[lo - 1].hi ? p->syms[lo - 1].name : NULL;
}

static void emap_add(struct emap *m, uint64_t from, uint64_t to)
{
	size_t i;

	if (m->used * 2 >= m->cap) {
		size_t nc = m->cap ? m->cap * 2 : 1 << 14, k;
		struct edge *ne = calloc(nc, sizeof(struct edge));

		for (k = 0; k < m->cap; k++)
			if (m->e[k].n) {
				size_t j = ((m->e[k].from * 0x9E3779B97F4A7C15ULL) ^ m->e[k].to) & (nc - 1);

				while (ne[j].n) j = (j + 1) & (nc - 1);
				ne[j] = m->e[k];
			}
		free(m->e);
		m->e = ne; m->cap = nc;
	}
	i = ((from * 0x9E3779B97F4A7C15ULL) ^ to) & (m->cap - 1);
	while (m->e[i].n && (m->e[i].from != from || m->e[i].to != to))
		i = (i + 1) & (m->cap - 1);
	if (!m->e[i].n) { m->e[i].from = from; m->e[i].to = to; m->used++; }
	m->e[i].n++;
}

static int edge_cmp(const void *a, const void *b)
{
	const struct edge *x = a, *y = b;
	if (x->from != y->from) return x->from < y->from ? -1 : 1;
	if (x->to != y->to) return x->to < y->to ? -1 : 1;
	return 0;
}

struct fcount { const char *name; uint64_t n; };
static int fc_cmp(const void *a, const void *b)
{
	const struct fcount *x = a, *y = b;
	if (x->n != y->n) return x->n < y->n ? 1 : -1;
	return strcmp(x->name, y->name);
}
static int fc_name_cmp(const void *a, const void *b)
{
	const struct fcount *x = a, *y = b;
	return strcmp(x->name, y->name);
}

int main(int argc, char **argv)
{
	const char *trace_path, *outdir;
	FILE *f;
	long tlen;
	uint8_t *trace;
	struct pt_config cfg;
	struct pt_image *img;
	struct pt_insn_decoder *dec;
	uint64_t errs = 0, outside = 0;
	char path[4096];
	int i, st;

	if (argc < 4) {
		fprintf(stderr, "usage: %s <trace.bin> <outdir> <name>=<elf>[@<bias>] ...\n", argv[0]);
		return 2;
	}
	trace_path = argv[1]; outdir = argv[2];
	for (i = 3; i < argc && nprogs < MAXPROG; i++) {
		char *a = strdup(argv[i]), *eq = strchr(a, '='), *at;
		struct prog *p = &progs[nprogs];

		if (!eq) { fprintf(stderr, "bad program spec: %s\n", argv[i]); return 2; }
		*eq = 0;
		at = strchr(eq + 1, '@');
		if (at) { *at = 0; p->bias = strtoull(at + 1, NULL, 0); }
		p->name = a; p->path = eq + 1;
		if (load_prog(p)) return 1;
		nprogs++;
	}
	for (i = 0; i < nprogs; i++) {		/* ranges must be disjoint */
		int j;

		for (j = i + 1; j < nprogs; j++)
			if (progs[i].lo < progs[j].hi && progs[j].lo < progs[i].hi) {
				fprintf(stderr, "programs %s and %s overlap in memory\n", progs[i].name, progs[j].name);
				return 2;
			}
	}

	if (!(f = fopen(trace_path, "rb"))) { perror(trace_path); return 1; }
	fseek(f, 0, SEEK_END); tlen = ftell(f); fseek(f, 0, SEEK_SET);
	trace = malloc(tlen);
	if (!trace || fread(trace, 1, tlen, f) != (size_t)tlen) { fclose(f); return 1; }
	fclose(f);

	pt_config_init(&cfg);
	cfg.begin = trace;
	cfg.end = trace + tlen;
	cfg.cpu.vendor = pcv_intel;		/* the tracing box: Kaby Lake, family 6 model 158 */
	cfg.cpu.family = 6; cfg.cpu.model = 158; cfg.cpu.stepping = 9;
	pt_cpu_errata(&cfg.errata, &cfg.cpu);
	dec = pt_insn_alloc_decoder(&cfg);
	img = pt_image_alloc("richcov");
	if (!dec || !img) return 1;
	pt_image_set_callback(img, read_mem, NULL);
	pt_insn_set_image(dec, img);

	for (;;) {
		uint64_t prev_ip = 0;
		int prev_prog = -1, prev_branch = 0;

		st = pt_insn_sync_forward(dec);
		if (st < 0)
			break;
		for (;;) {
			struct pt_insn in;
			int pi;

			while (st & pts_event_pending) {
				struct pt_event ev;

				st = pt_insn_event(dec, &ev, sizeof(ev));
				if (st < 0)
					break;
				if (ev.type == ptev_overflow)	/* lost trace data: the chain breaks */
					prev_branch = 0;
			}
			if (st < 0)
				break;
			st = pt_insn_next(dec, &in, sizeof(in));
			if (st < 0)
				break;
			pi = prog_of(in.ip);
			if (pi < 0) {			/* should not happen with the IP filter */
				outside++;
				prev_branch = 0;
				continue;
			}
			progs[pi].insns++;
			if (prev_branch && pi == prev_prog)
				emap_add(&progs[pi].edges, prev_ip, in.ip);
			prev_ip = in.ip;
			prev_prog = pi;
			prev_branch = in.iclass != ptic_other && in.iclass != ptic_error;
		}
		if (st == -pte_eos)
			break;
		errs++;				/* each error resyncs at the next PSB */
	}
	pt_insn_free_decoder(dec);
	pt_image_free(img);

	snprintf(path, sizeof(path), "%s/summary.txt", outdir);
	{
		FILE *sum = fopen(path, "w");

		if (!sum) { perror(path); return 1; }
		fprintf(sum, "trace=%s bytes=%ld decode_errors=%llu outside_programs=%llu\n", trace_path, tlen,
			(unsigned long long)errs, (unsigned long long)outside);
		fprintf(sum, "%-14s %14s %10s %10s\n", "program", "instructions", "edges", "functions");
		for (i = 0; i < nprogs; i++) {
			struct prog *p = &progs[i];
			size_t k, ne = 0, nf = 0;
			struct fcount *fc;
			FILE *o;

			for (k = 0; k < p->edges.cap; k++)
				if (p->edges.e && p->edges.e[k].n) p->edges.e[ne++] = p->edges.e[k];
			qsort(p->edges.e, ne, sizeof(struct edge), edge_cmp);
			snprintf(path, sizeof(path), "%s/%s.edges", outdir, p->name);
			if (!(o = fopen(path, "w"))) { perror(path); return 1; }
			for (k = 0; k < ne; k++)
				fprintf(o, "%llx %llx %llu\n", (unsigned long long)p->edges.e[k].from,
					(unsigned long long)p->edges.e[k].to, (unsigned long long)p->edges.e[k].n);
			fclose(o);

			/* per-function branch-edge hits, attributed by the edge's source address */
			fc = calloc(ne + 1, sizeof(*fc));
			for (k = 0; k < ne; k++) {
				const char *fn = func_of(p, p->edges.e[k].from);

				fc[k].name = fn ? fn : "?";
				fc[k].n = p->edges.e[k].n;
			}
			qsort(fc, ne, sizeof(*fc), fc_name_cmp);
			for (k = 0; k < ne; k++) {		/* merge equal names */
				if (nf && !strcmp(fc[nf - 1].name, fc[k].name)) fc[nf - 1].n += fc[k].n;
				else fc[nf++] = fc[k];
			}
			qsort(fc, nf, sizeof(*fc), fc_cmp);
			snprintf(path, sizeof(path), "%s/%s.funcs", outdir, p->name);
			if (!(o = fopen(path, "w"))) { perror(path); return 1; }
			for (k = 0; k < nf; k++)
				fprintf(o, "%llu %s\n", (unsigned long long)fc[k].n, fc[k].name);
			fclose(o);
			fprintf(sum, "%-14s %14llu %10zu %10zu\n", p->name, (unsigned long long)p->insns, ne, nf);
			free(fc);
		}
		fclose(sum);
	}
	snprintf(path, sizeof(path), "%s/summary.txt", outdir);
	if ((f = fopen(path, "r"))) {
		char line[512];

		while (fgets(line, sizeof(line), f))
			fputs(line, stdout);
		fclose(f);
	}
	return errs > 1000 ? 1 : 0;
}

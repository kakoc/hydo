// SPDX-License-Identifier: GPL-2.0
/*
 * ref_ptedges -- test helper: decode a single-program trace with the explorer's own M41
 * decoder (hv/hvx/ptedges.h, included unmodified) so ptmulti's output for that
 * program can be checked against it edge for edge.
 *   ref_ptedges <elf> <trace.bin> <out.edges>
 * Build: cc -O2 -I../../hv/hvx -o ref_ptedges ref_ptedges.c -lipt
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "ptedges.h"

int main(int argc, char **argv)
{
	uint64_t lo, hi, errs = 0;
	FILE *f;
	long n, edges;
	uint8_t *buf;

	if (argc != 4) { fprintf(stderr, "usage: %s <elf> <trace.bin> <out.edges>\n", argv[0]); return 2; }
	if (pte_load_elf(argv[1], &lo, &hi)) return 1;
	if (!(f = fopen(argv[2], "rb"))) { perror(argv[2]); return 1; }
	fseek(f, 0, SEEK_END); n = ftell(f); fseek(f, 0, SEEK_SET);
	buf = malloc(n);
	if (!buf || fread(buf, 1, n, f) != (size_t)n) return 1;
	fclose(f);
	edges = pte_decode(buf, n, argv[3], &errs);
	printf("ref_ptedges: text [%#llx, %#llx) edges=%ld decode_errors=%llu\n",
	       (unsigned long long)lo, (unsigned long long)hi, edges, (unsigned long long)errs);
	return edges < 0;
}

// SPDX-License-Identifier: GPL-2.0
/*
 * hydo_assert_report -- print the property table of one or more saved guest consoles.
 *
 *   hydo_assert_report <console> [<console>...]
 *
 * Each file is one run (e.g. the HVX_CONSOLE_OUT dump of a harness). The table aggregates
 * the HYDO-ASSERT records of all of them (hvx/ASSERTIONS.md, hvx/hydo_assert_table.h).
 * Exit status: 0 = no violation, 1 = at least one violated property, 2 = usage / I/O error.
 */
#include <stdio.h>
#include <stdlib.h>
#include "hydo_assert_table.h"

int main(int argc, char **argv)
{
	static struct ha_table t;
	int i;

	if (argc < 2) {
		fprintf(stderr, "usage: %s <console> [<console>...]\n", argv[0]);
		return 2;
	}
	ha_init(&t);
	for (i = 1; i < argc; i++) {
		FILE *f = fopen(argv[i], "rb");
		char *buf;
		long n;

		if (!f) {
			perror(argv[i]);
			return 2;
		}
		fseek(f, 0, SEEK_END);
		n = ftell(f);
		fseek(f, 0, SEEK_SET);
		buf = malloc(n > 0 ? (size_t)n : 1);
		if (!buf || fread(buf, 1, (size_t)n, f) != (size_t)n) {
			fprintf(stderr, "%s: read failed\n", argv[i]);
			return 2;
		}
		fclose(f);
		ha_feed_console(&t, buf, (size_t)n);
		free(buf);
	}
	ha_print(&t, stdout);
	return ha_violations(&t) ? 1 : 0;
}

/*
 * M116 feasibility experiment on the Mac engine (docs/design/guest-control.md): a host command
 * typed on the serial input at an exact exit count of the workload, and in reaction to a console
 * line, must keep every fingerprint reproducible (serve twice, and one == serve).
 *
 * build.sh generates gen/serve_gc.c from hv/hvm/explore/serve.c with two one-line substitutions
 * (the stop hook becomes gc_hook, which calls serve.c's marker_hook after gc_due; and the typed
 * "run ..." line also calls gc_arm). hvm.c and serve.c themselves are unchanged.
 *
 *   GC_AT="<delta>:<text>[;...]"   type <text>\n at the loop top where M.exits = exits(S1) + delta
 *   GC_REACT="<marker>@<delay>:<text>"  when <marker> appears in the workload's console, type
 *                                  <text>\n <delay> exits after the exit at which it appeared
 * Log lines on stderr: "GC put exits=<n> rel=<n> vtime=<n> text=...".
 */
#include <stdint.h>
static int gc_hook(void);
static void gc_arm(const char *line);
#include "gen/serve_gc.c"

#define GC_MAX 16
static struct { uint64_t at; int done; char text[256]; } gc_t[GC_MAX];
static int gc_n = -1, gc_nbase, gc_live, gc_reacted;
static uint64_t gc_run0;
static size_t gc_from, gc_scan;
static char gc_marker[128], gc_rtext[256];
static uint64_t gc_rdelay;

static void gc_parse(void)
{
	const char *at = getenv("GC_AT"), *re = getenv("GC_REACT");
	gc_n = 0;
	if (at && *at) {
		char buf[4096], *save = NULL, *tok;
		snprintf(buf, sizeof buf, "%s", at);
		for (tok = strtok_r(buf, ";", &save); tok && gc_n < GC_MAX; tok = strtok_r(NULL, ";", &save)) {
			char *c = strchr(tok, ':');
			if (!c) continue;
			*c = 0;
			gc_t[gc_n].at = strtoull(tok, NULL, 0);
			snprintf(gc_t[gc_n].text, sizeof gc_t[gc_n].text, "%s", c + 1);
			gc_n++;
		}
	}
	if (re && *re) {
		const char *a = strrchr(re, '@'), *c = a ? strchr(a, ':') : NULL;
		if (a && c && (size_t)(a - re) < sizeof gc_marker) {
			memcpy(gc_marker, re, (size_t)(a - re));
			gc_marker[a - re] = 0;
			gc_rdelay = strtoull(a + 1, NULL, 0);
			snprintf(gc_rtext, sizeof gc_rtext, "%s", c + 1);
		}
	}
	gc_nbase = gc_n;
}

static void gc_arm(const char *line)
{
	if (gc_n < 0) gc_parse();
	while (*line == ' ') line++;	/* hvm types the space after the bar */
	if (strncmp(line, "run", 3)) return;
	gc_n = gc_nbase;	/* forget the last run's reaction */
	gc_run0 = M.exits;
	gc_live = 1;
	gc_reacted = 0;
	gc_from = gc_scan = M.console_len;
	for (int i = 0; i < gc_n; i++) gc_t[i].done = 0;
	fprintf(stderr, "GC run exits=%llu vtime=%llu\n", (unsigned long long)M.exits, (unsigned long long)M.vtime);
}

static void gc_type(const char *text, uint64_t rel)
{
	char b[260];
	snprintf(b, sizeof b, "%s\n", text);
	hvm_put_input(b);
	fprintf(stderr, "GC put exits=%llu rel=%llu vtime=%llu text=%s\n", (unsigned long long)M.exits,
		(unsigned long long)rel, (unsigned long long)M.vtime, text);
}

static int gc_hook(void)
{
	if (gc_live) {
		uint64_t rel = M.exits - gc_run0;
		for (int i = 0; i < gc_n; i++)
			if (!gc_t[i].done && rel >= gc_t[i].at) { gc_t[i].done = 1; gc_type(gc_t[i].text, rel); }
		if (gc_marker[0] && !gc_reacted) {
			size_t ml = strlen(gc_marker), st = gc_scan > ml ? gc_scan - ml : 0;
			if (st < gc_from) st = gc_from;
			if (M.console_len >= st + ml && memmem(console + st, M.console_len - st, gc_marker, ml)) {
				gc_reacted = 1;
				fprintf(stderr, "GC react marker seen at exits=%llu rel=%llu\n", (unsigned long long)M.exits,
					(unsigned long long)rel);
				if (gc_n < GC_MAX) {
					gc_t[gc_n].at = rel + gc_rdelay;
					gc_t[gc_n].done = 0;
					snprintf(gc_t[gc_n].text, sizeof gc_t[gc_n].text, "%s", gc_rtext);
					gc_n++;
					if (!gc_rdelay) { gc_t[gc_n - 1].done = 1; gc_type(gc_rtext, rel); }
				}
			}
			gc_scan = M.console_len;
		}
	}
	return marker_hook();
}

/* hydo_assert_table.h -- host side of hydo's assertion protocol (hvx/ASSERTIONS.md).
 *
 * Parses the HYDO-ASSERT-DECL / HYDO-ASSERT / HYDO-SETUP-COMPLETE records that guest code
 * writes to the console, and aggregates them into a property table over one or more runs
 * (a run = one guest console; an exploration = many runs). Header-only, no allocation
 * beyond a fixed table, so any harness or the explorer can include it.
 *
 *   struct ha_table t; ha_init(&t);
 *   ha_feed_console(&t, buf, len);   // once per run (bumps t.runs)
 *   ha_print(&t, stdout);            // the property table
 *   if (ha_violations(&t)) ...       // always-false, unreachable-hit, always-never-reached
 *
 * Verdicts:
 *   always                 FAIL if any evaluation was false, or if it was never evaluated
 *   always_or_unreachable  FAIL if any evaluation was false; never evaluated is fine
 *   sometimes / reachable  "NOT EXERCISED" if no evaluation was true in any run
 *   unreachable            FAIL if it was ever reached
 */
#ifndef HYDO_ASSERT_TABLE_H
#define HYDO_ASSERT_TABLE_H

#include <stdio.h>
#include <string.h>

#define HA_MAX_PROPS 256
#define HA_ID_LEN    96
#define HA_MSG_LEN   160
#define HA_TYPE_LEN  24

struct ha_prop {
	char type[HA_TYPE_LEN];
	char id[HA_ID_LEN];
	char msg[HA_MSG_LEN];
	int declared;			/* seen in a HYDO-ASSERT-DECL */
	unsigned long passes, fails;	/* evaluation records (ok=true / ok=false) */
	unsigned long runs_pass, runs_fail;	/* runs with at least one such record */
	char first_fail[HA_MSG_LEN];	/* details of the first failing record seen */
	/* per-run scratch */
	int run_pass, run_fail;
};

struct ha_table {
	struct ha_prop p[HA_MAX_PROPS];
	int n;
	unsigned long runs, runs_setup_complete;
};

static inline void ha_init(struct ha_table *t) { memset(t, 0, sizeof(*t)); }

/* copy the JSON string value of "key" (no unescaping beyond \" and \\) */
static inline int ha_jstr(const char *rec, const char *key, char *out, size_t outn)
{
	char pat[40];
	const char *p;
	size_t o = 0;

	snprintf(pat, sizeof(pat), "\"%s\":\"", key);
	p = strstr(rec, pat);
	if (!p)
		return 0;
	p += strlen(pat);
	while (*p && *p != '"' && *p != '\n' && o + 1 < outn) {
		if (*p == '\\' && p[1])
			p++;
		out[o++] = *p++;
	}
	out[o] = 0;
	return 1;
}

/* the raw JSON value of "key" (object/number/bool), up to the end of the record */
static inline int ha_jraw(const char *rec, const char *key, char *out, size_t outn)
{
	char pat[40];
	const char *p, *e;
	size_t len;

	snprintf(pat, sizeof(pat), "\"%s\":", key);
	p = strstr(rec, pat);
	if (!p)
		return 0;
	p += strlen(pat);
	e = p;
	while (*e && *e != '\n' && *e != '\r')
		e++;
	len = (size_t)(e - p);
	if (len >= outn)
		len = outn - 1;
	memcpy(out, p, len);
	out[len] = 0;
	{	/* drop the record's own closing brace(s): keep the value balanced */
		int depth = 0;
		size_t k;

		for (k = 0; k < len; k++) {
			if (out[k] == '{')
				depth++;
			else if (out[k] == '}' && --depth < 0) {
				out[k] = 0;
				break;
			}
		}
	}
	return 1;
}

static inline struct ha_prop *ha_get(struct ha_table *t, const char *id, const char *type)
{
	int i;

	for (i = 0; i < t->n; i++)
		if (!strcmp(t->p[i].id, id))
			return &t->p[i];
	if (t->n >= HA_MAX_PROPS)
		return NULL;
	memset(&t->p[t->n], 0, sizeof(t->p[0]));
	snprintf(t->p[t->n].id, HA_ID_LEN, "%s", id);
	snprintf(t->p[t->n].type, HA_TYPE_LEN, "%s", type);
	return &t->p[t->n++];
}

/* one record (the text after the tag) */
static inline void ha_record(struct ha_table *t, int decl, const char *rec)
{
	char type[HA_TYPE_LEN] = "", id[HA_ID_LEN] = "", msg[HA_MSG_LEN] = "";
	struct ha_prop *p;

	if (!ha_jstr(rec, "t", type, sizeof(type)) || !ha_jstr(rec, "id", id, sizeof(id)))
		return;
	ha_jstr(rec, "msg", msg, sizeof(msg));
	p = ha_get(t, id, type);
	if (!p)
		return;
	if (!p->msg[0] && msg[0])
		snprintf(p->msg, HA_MSG_LEN, "%s", msg);
	if (decl) {
		p->declared = 1;
		return;
	}
	if (strstr(rec, "\"ok\":true")) {
		p->passes++;
		p->run_pass = 1;
	} else {
		p->fails++;
		p->run_fail = 1;
		if (!p->first_fail[0] && !ha_jraw(rec, "details", p->first_fail, HA_MSG_LEN))
			snprintf(p->first_fail, HA_MSG_LEN, "-");
	}
}

/* Feed one whole guest console (one run). Records may appear anywhere in a line. */
static inline void ha_feed_console(struct ha_table *t, const char *buf, size_t len)
{
	static const char tag_decl[] = "HYDO-ASSERT-DECL ", tag[] = "HYDO-ASSERT ";
	char line[1024];
	size_t i = 0, o;
	int k, setup = 0;

	for (k = 0; k < t->n; k++)
		t->p[k].run_pass = t->p[k].run_fail = 0;
	while (i < len) {
		const char *s;

		o = 0;
		while (i < len && buf[i] != '\n') {
			if (o + 1 < sizeof(line))
				line[o++] = buf[i];
			i++;
		}
		i++;
		line[o] = 0;
		if (strstr(line, "HYDO-SETUP-COMPLETE"))
			setup = 1;
		if ((s = strstr(line, tag_decl)))
			ha_record(t, 1, s + sizeof(tag_decl) - 1);
		else if ((s = strstr(line, tag)))
			ha_record(t, 0, s + sizeof(tag) - 1);
	}
	for (k = 0; k < t->n; k++) {
		t->p[k].runs_pass += t->p[k].run_pass;
		t->p[k].runs_fail += t->p[k].run_fail;
	}
	t->runs++;
	t->runs_setup_complete += setup;
}

/* verdict of one property over everything fed so far */
static inline const char *ha_verdict(const struct ha_prop *p)
{
	if (!strcmp(p->type, "always"))
		return p->fails ? "FAIL" : (p->passes ? "PASS" : "FAIL (never evaluated)");
	if (!strcmp(p->type, "always_or_unreachable"))
		return p->fails ? "FAIL" : "PASS";
	if (!strcmp(p->type, "unreachable"))
		return p->fails ? "FAIL (reached)" : "PASS";
	/* sometimes, reachable */
	return p->passes ? "PASS" : "NOT EXERCISED";
}

static inline int ha_violations(const struct ha_table *t)
{
	int i, v = 0;

	for (i = 0; i < t->n; i++)
		if (!strncmp(ha_verdict(&t->p[i]), "FAIL", 4))
			v++;
	return v;
}

static inline void ha_print(const struct ha_table *t, FILE *f)
{
	int i;

	fprintf(f, "%-22s %-32s %7s %7s  %-24s %s\n", "type", "id", "ok", "fail", "verdict", "first failure / description");
	for (i = 0; i < t->n; i++) {
		const struct ha_prop *p = &t->p[i];

		fprintf(f, "%-22s %-32s %7lu %7lu  %-24s %s%s\n", p->type, p->id, p->passes, p->fails,
			ha_verdict(p), p->first_fail[0] ? p->first_fail : p->msg,
			p->declared ? "" : "  [not in catalog]");
	}
	fprintf(f, "runs=%lu (setup-complete seen in %lu); properties=%d; violations=%d\n",
		t->runs, t->runs_setup_complete, t->n, ha_violations(t));
}

#endif /* HYDO_ASSERT_TABLE_H */

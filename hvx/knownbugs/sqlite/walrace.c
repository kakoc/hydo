/* walrace -- a generic concurrent workload + checker for SQLite in WAL mode (known-bug target #1).
 *
 * Written from the public description of the WAL-reset bug only (sqlite.org/wal.html section 11:
 * "two or more database connections open on the same file, in separate threads or processes ...
 * attempt to write or checkpoint at the same instant"); it does not aim at the bug's exact
 * window. Like the published write-up's workload it "just runs writes and checkpoints concurrently" and
 * checks the two standard properties: no committed write is lost, the file is not corrupt.
 *
 *   walrace run   <db> <acked> <outcomes> <from> <to>    workload steps [from, to)
 *   walrace check <db> <acked>                           exit 0 holds, 1 violated, 2 could not check
 *
 * One step = one round: W writer threads (each its own connection) commit small transactions
 * while one checkpointer thread (its own connection) runs checkpoints back to back in seeded
 * modes, and one reader thread holds short read transactions. Everything that varies is drawn
 * from a seed (file /tmp/walrace.seed, written by the manifest's S knob; default 0) and the step
 * number -- no clocks, no rand(). The interleaving itself is the guest scheduler's, which hvx
 * makes deterministic; seeded tick jitter (explore.py --timing) is what varies it.
 *
 * Every committed transaction appends its id to <acked> right after COMMIT returns SQLITE_OK.
 * Built statically against the amalgamation of the SQLite version under test.
 */
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/resource.h>
#include <fcntl.h>
#include "sqlite3.h"

static uint64_t mix(uint64_t x)
{
	x += 0x9E3779B97F4A7C15ULL;
	x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
	x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
	return x ^ (x >> 31);
}

struct rng { uint64_t s; };
static uint64_t rnext(struct rng *r) { r->s += 0x9E3779B97F4A7C15ULL; return mix(r->s); }
static unsigned rbelow(struct rng *r, unsigned n) { return (unsigned)(rnext(r) % n); }

static int64_t value_of(int64_t id) { return (int64_t)(mix((uint64_t)id) & 0x7fffffffffffULL); }

static const char *g_db;
static FILE *g_acked, *g_out;
static const char *g_acked_path;
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static volatile int g_writers_left;
static uint64_t g_seed;
static long g_ckpt, g_ckpt_busy, g_ckpt_restartish;

/* per-run shape, drawn once per run from the seed */
static int W, TXNS, MAXROWS, MAXPAD, AUTOCKPT, CKPT_YIELD, READER, NCKPT, MODES, NMODES, MODE[4], MIXED;

/* "reset" mode (seed file "<hex> reset", see read_seed): a separate workload shape that makes
 * WAL restarts frequent. Off by default; with it off nothing below runs and the default
 * workload is unchanged. g_count: count WAL restarts (also in the default shape when the
 * environment variable WALRACE_COUNT=1 is set; for native measurements only). */
static int g_reset, g_count, BUSY_YIELD, RD_CONN, SYNC_OFF, RD_HOLD;   /* g_reset: 1 = "reset", 2 = "reset2" */
static long g_ckpt_restart_ok;          /* RESTART/TRUNCATE checkpoints that returned SQLITE_OK */
static long g_complete_small, g_complete_big;   /* checkpoints that completed a WAL of < / >= 256 frames */
static long g_restarts;                 /* WAL header salt changes seen (= WAL restarts, lower bound) */
static uint64_t g_last_salt;            /* last salt pair seen in this step (0 = none yet) */
static pthread_mutex_t g_salt_mu = PTHREAD_MUTEX_INITIALIZER;

/* The WAL file header holds two salt words at offsets 16..23. SQLite writes a new header, with
 * a new salt pair, exactly when the WAL is restarted from frame 0 (by a writer after a complete
 * checkpoint, or by a RESTART/TRUNCATE checkpoint followed by the next write). Each thread reads
 * the header after its own commits and checkpoints and counts a change of the salt pair; a writer
 * that reset the WAL sees its own new header right after its commit, so this misses a restart
 * only when two happen between two reads. Read-only, on its own descriptor (SQLite holds no POSIX
 * locks on the -wal file, so opening and closing it cannot drop SQLite's locks). */
static void note_nckpt(int *fd)
{
	if (!g_count) return;
	if (*fd < 0) {
		char path[512];
		snprintf(path, sizeof(path), "%s-wal", g_db);
		*fd = open(path, O_RDONLY);
		if (*fd < 0) return;
	}
	uint64_t salt;
	if (pread(*fd, &salt, 8, 16) != 8 || !salt) return;     /* empty (truncated) WAL file */
	pthread_mutex_lock(&g_salt_mu);
	if (salt != g_last_salt) {
		if (g_last_salt) g_restarts++;
		g_last_salt = salt;
	}
	pthread_mutex_unlock(&g_salt_mu);
}

/* reset mode, seeded: a busy handler that yields and retries instead of sleeping 1..100 ms
 * (sqlite3_busy_timeout's default), so contending writers stay ready to commit */
static int busy_yield(void *p, int n)
{
	(void)p;
	if (n > 20000000) return 0;
	sched_yield();
	return 1;
}

static sqlite3 *open_db(void)
{
	sqlite3 *db;
	if (sqlite3_open_v2(g_db, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX, NULL) != SQLITE_OK) {
		fprintf(stderr, "walrace: open %s: %s\n", g_db, sqlite3_errmsg(db));
		exit(2);
	}
	if (g_reset && BUSY_YIELD)
		sqlite3_busy_handler(db, busy_yield, NULL);
	else
		sqlite3_busy_timeout(db, 20000);
	sqlite3_exec(db, "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;", 0, 0, 0);
	if (SYNC_OFF)
		sqlite3_exec(db, "PRAGMA synchronous=OFF;", 0, 0, 0);
	char q[64];
	snprintf(q, sizeof(q), "PRAGMA wal_autocheckpoint=%d;", AUTOCKPT);
	sqlite3_exec(db, q, 0, 0, 0);
	return db;
}

struct warg { int step, w; };

static void *writer(void *p)
{
	struct warg *a = p;
	struct rng r = { mix(g_seed ^ ((uint64_t)a->step << 20) ^ (uint64_t)(a->w + 1) * 0x51ED) };
	sqlite3 *db = open_db();
	static char pad[8192];
	memset(pad, 'p', sizeof(pad));
	sqlite3 *ck = MIXED ? open_db() : NULL;     /* mixed: this thread also checkpoints, on its own second connection */
	sqlite3_stmt *ins;
	int walfd = -1;
	sqlite3_prepare_v2(db, "INSERT INTO t(id, val, pad, n) VALUES(?1, ?2, ?3, ?4)", -1, &ins, 0);
	for (int i = 0; i < TXNS; i++) {
		/* transaction id (what <acked> records, kept short: the guest prints the acked list
		 * on its 64 KiB console); its rows are tx*8 .. tx*8+rows-1, row 0 also stores rows */
		int64_t tx = g_reset ? (((int64_t)a->step * 8 + a->w) << 20) + i     /* reset mode: TXNS < 2^20 */
				     : ((int64_t)a->step * 8 + a->w) * 256 + i;
		int64_t id = tx * 8;
		int rows = 1 + (int)rbelow(&r, MAXROWS);
		int rc = sqlite3_exec(db, "BEGIN IMMEDIATE", 0, 0, 0);
		for (int k = 0; rc == SQLITE_OK && k < rows; k++) {
			sqlite3_reset(ins);
			sqlite3_bind_int64(ins, 1, id + k);
			sqlite3_bind_int64(ins, 2, value_of(id + k));
			sqlite3_bind_blob(ins, 3, pad, 1 + (int)rbelow(&r, MAXPAD), SQLITE_STATIC);
			sqlite3_bind_int(ins, 4, k ? 0 : rows);
			rc = sqlite3_step(ins) == SQLITE_DONE ? SQLITE_OK : SQLITE_ERROR;
		}
		if (rc == SQLITE_OK)
			rc = sqlite3_exec(db, "COMMIT", 0, 0, 0);
		else
			sqlite3_exec(db, "ROLLBACK", 0, 0, 0);
		if (rc == SQLITE_OK) note_nckpt(&walfd);
		pthread_mutex_lock(&g_mu);
		if (rc == SQLITE_OK) fprintf(g_acked, "%lld\n", (long long)tx);
		if (!g_reset || rc != SQLITE_OK)        /* reset mode: only failures (the list is long) */
			fprintf(g_out, "%lld %s\n", (long long)tx, rc == SQLITE_OK ? "ack" : "failed");
		fflush(g_acked); fflush(g_out);
		pthread_mutex_unlock(&g_mu);
		if (ck && rbelow(&r, 2)) {
			int log = 0, ckd = 0;
			sqlite3_wal_checkpoint_v2(ck, NULL, MODE[rbelow(&r, NMODES)], &log, &ckd);
		}
	}
	if (ck) sqlite3_close(ck);
	if (walfd >= 0) close(walfd);
	sqlite3_finalize(ins);
	sqlite3_close(db);
	__sync_fetch_and_sub(&g_writers_left, 1);
	return NULL;
}

static void *checkpointer(void *p)
{
	struct warg *a = p;
	struct rng r = { mix(g_seed ^ ((uint64_t)a->step << 20) ^ 0xC4EC4E ^ (uint64_t)a->w * 0x1F3) };
	sqlite3 *db = open_db();
	int walfd = -1;
	while (g_writers_left > 0) {
		int burst = 1 + (int)rbelow(&r, 3);     /* checkpoints back to back */
		for (int b = 0; b < burst; b++) {
			int log = 0, ck = 0, mode = MODE[rbelow(&r, NMODES)];
			int rc = sqlite3_wal_checkpoint_v2(db, NULL, mode, &log, &ck);
			g_ckpt++;
			if (rc == SQLITE_BUSY) g_ckpt_busy++;
			else if (log == 0) g_ckpt_restartish++;     /* WAL empty/reset afterwards */
			if (g_count && rc == SQLITE_OK && (mode == SQLITE_CHECKPOINT_RESTART || mode == SQLITE_CHECKPOINT_TRUNCATE))
				__sync_fetch_and_add(&g_ckpt_restart_ok, 1);
			if (g_count && rc == SQLITE_OK && log > 0 && ck == log)      /* this call left the WAL fully checkpointed */
				__sync_fetch_and_add(log >= 256 ? &g_complete_big : &g_complete_small, 1);
			note_nckpt(&walfd);
		}
		if (CKPT_YIELD && rbelow(&r, 2))
			sched_yield();
	}
	if (walfd >= 0) close(walfd);
	sqlite3_close(db);
	return NULL;
}

static void *reader(void *p)
{
	struct warg *a = p;
	struct rng r = { mix(g_seed ^ ((uint64_t)a->step << 20) ^ 0x8EAD ^ (uint64_t)a->w * 0x1F3) };
	sqlite3 *db = open_db();
	int left = RD_CONN ? 1 + (int)rbelow(&r, RD_CONN) : 0;    /* reset mode: reconnect after this many */
	while (g_writers_left > 0) {
		if (g_reset && RD_CONN && --left < 0) {
			sqlite3_close(db);
			db = open_db();
			left = (int)rbelow(&r, RD_CONN);
		}
		sqlite3_exec(db, "BEGIN", 0, 0, 0);
		sqlite3_exec(db, rbelow(&r, 2) ? "SELECT count(*) FROM t" : "SELECT max(id) FROM t", 0, 0, 0);
		if (RD_HOLD) {      /* reset2: keep this read transaction (and its WAL snapshot) open a while */
			int hold = (int)rbelow(&r, RD_HOLD);
			for (int h = 0; h < hold && g_writers_left > 0; h++) {
				sched_yield();
				sqlite3_exec(db, "SELECT max(id) FROM t", 0, 0, 0);
			}
		}
		sqlite3_exec(db, "COMMIT", 0, 0, 0);
		if (rbelow(&r, 2))
			sched_yield();
	}
	sqlite3_close(db);
	return NULL;
}

static int g_aimed;

/* /tmp/walrace.seed: "<hex seed>" or "<hex seed> aimed". "aimed" is NOT part of the blind
 * search: it pins the shape to what the bug write-up (sqlite.org wal.html 11.1) and the 3.51.3
 * fix describe -- back-to-back PASSIVE checkpoints racing writers, no readers -- so that a
 * separate, labelled experiment can ask whether hydo's preemption points can land in the
 * window at all once the workload is right. */
static uint64_t read_seed(void)
{
	const char *sf = getenv("WALRACE_SEED_FILE");     /* native runs: several at once */
	FILE *f = fopen(sf && *sf ? sf : "/tmp/walrace.seed", "r");
	unsigned long long v = 0;
	char w[16] = "";
	if (f) { if (fscanf(f, "%llx %15s", &v, w) < 1) v = 0; fclose(f); }
	g_aimed = !strcmp(w, "aimed");
	g_reset = !strcmp(w, "reset") ? 1 : !strcmp(w, "reset2") ? 2 : 0;
	{   /* hydo-reset.yaml sets the mode in the environment (the S knob writes only the seed) */
		const char *m = getenv("WALRACE_MODE");
		if (m && !strcmp(m, "reset")) g_reset = 1;
		if (m && !strcmp(m, "reset2")) g_reset = 2;
	}
	return v;
}

static int run(int from, int to)
{
	g_seed = read_seed();
	struct rng shape = { mix(g_seed ^ 0x5EA9E) };
	W = 2 + (int)rbelow(&shape, 4);                 /* 2..5 writers */
	NCKPT = 1 + (int)rbelow(&shape, 2);             /* 1..2 checkpointer threads */
	TXNS = 40 + (int)rbelow(&shape, 80);             /* < 256 (transaction id layout) */
	MAXROWS = 1 + (int)rbelow(&shape, 4);
	MAXPAD = 16 + (int)rbelow(&shape, 1000);
	{ static const int ac[] = { 0, 1, 4, 16, 100, 1000 }; AUTOCKPT = ac[rbelow(&shape, 6)]; }
	CKPT_YIELD = (int)rbelow(&shape, 2);
	READER = (int)rbelow(&shape, 3);                /* 0..2 reader threads */
	MIXED = (int)rbelow(&shape, 2);                 /* writers also checkpoint (second connection) */
	if (g_aimed) { NCKPT = 1; READER = 0; MIXED = 0; CKPT_YIELD = 0; AUTOCKPT = 0; }
	/* swarm-style: which checkpoint modes this run uses at all (a non-empty subset) */
	do MODES = (int)rbelow(&shape, 16); while (!MODES);
	if (g_aimed) MODES = 1;                         /* PASSIVE only */
	{
		static const int all[] = { SQLITE_CHECKPOINT_PASSIVE, SQLITE_CHECKPOINT_FULL,
					   SQLITE_CHECKPOINT_RESTART, SQLITE_CHECKPOINT_TRUNCATE };
		NMODES = 0;
		for (int b = 0; b < 4; b++) if (MODES & (1 << b)) MODE[NMODES++] = all[b];
	}
	{ const char *c = getenv("WALRACE_COUNT"); g_count = c && *c == '1'; }
	{ const char *c = getenv("WALRACE_SYNC_OFF"); SYNC_OFF = c && *c == '1'; }   /* native, on a disk with slow fsync */
	if (g_reset) {
		/* reset mode: its own shape stream (the default draws above are unchanged) */
		struct rng rs = { mix(g_seed ^ 0x2E5E7) };
		static const int ac[] = { 0, 1, 2, 4, 16 };
		static const int all[] = { SQLITE_CHECKPOINT_PASSIVE, SQLITE_CHECKPOINT_FULL,
					   SQLITE_CHECKPOINT_RESTART, SQLITE_CHECKPOINT_TRUNCATE };
		W = 1 + (int)rbelow(&rs, 4);            /* 1..4 writers */
		NCKPT = 1 + (int)rbelow(&rs, 3);        /* 1..3 checkpointer threads */
		READER = (int)rbelow(&rs, 3);           /* 0..2 readers */
		RD_CONN = READER ? 1 + (int)rbelow(&rs, 8) : 0;   /* readers reconnect every 1..8 reads */
		TXNS = 300 + (int)rbelow(&rs, 1701);    /* 300..2000 small transactions per writer and step */
		MAXROWS = 1 + (int)rbelow(&rs, 2);
		MAXPAD = 16 + (int)rbelow(&rs, 400);
		AUTOCKPT = ac[rbelow(&rs, 5)];
		CKPT_YIELD = (int)rbelow(&rs, 2);
		MIXED = (int)rbelow(&rs, 2);
		BUSY_YIELD = (int)rbelow(&rs, 2);
		do MODES = (int)rbelow(&rs, 16); while (!MODES);
		NMODES = 0;
		for (int b = 0; b < 4; b++) if (MODES & (1 << b)) MODE[NMODES++] = all[b];
		if (g_reset == 2) {
			/* reset2: let the WAL grow before it is fully checkpointed: 1..3 readers that hold
			 * their read transaction for up to 4..2048 short reads, transactions of 1..8 rows */
			static const int hold[] = { 4, 32, 256, 2048 };
			READER = 1 + (int)rbelow(&rs, 3);
			RD_CONN = 1 + (int)rbelow(&rs, 8);
			RD_HOLD = hold[rbelow(&rs, 4)];
			MAXROWS = 1 + (int)rbelow(&rs, 8);
		}
		/* counting is on in reset mode; WALRACE_COUNT=0 turns it off (to show that the
		 * counter's own reads of the -wal file do not change the outcome) */
		{ const char *c = getenv("WALRACE_COUNT"); g_count = !(c && *c == '0'); }
	}
	char hdr[320];
	if (g_reset)
		snprintf(hdr, sizeof(hdr), "walrace: seed=%llx writers=%d ckpters=%d txns=%d rows<=%d pad<=%d autockpt=%d yield=%d readers=%d modes=%x mixed=%d aimed=%d reset=%d busyyield=%d rdconn=%d hold=%d syncoff=%d steps=%d..%d\n",
			 (unsigned long long)g_seed, W, NCKPT, TXNS, MAXROWS, MAXPAD, AUTOCKPT, CKPT_YIELD, READER, MODES, MIXED, g_aimed,
			 g_reset, BUSY_YIELD, RD_CONN, RD_HOLD, SYNC_OFF, from, to);
	else
	snprintf(hdr, sizeof(hdr), "walrace: seed=%llx writers=%d ckpters=%d txns=%d rows<=%d pad<=%d autockpt=%d yield=%d readers=%d modes=%x mixed=%d aimed=%d steps=%d..%d\n",
		 (unsigned long long)g_seed, W, NCKPT, TXNS, MAXROWS, MAXPAD, AUTOCKPT, CKPT_YIELD, READER, MODES, MIXED, g_aimed, from, to);
	fputs(hdr, stdout);
	FILE *hc = fopen("/dev/console", "w");
	if (hc) { fputs(hdr, hc); fclose(hc); }
	sqlite3 *db = open_db();
	sqlite3_exec(db, "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, val INTEGER, pad BLOB, n INTEGER)", 0, 0, 0);
	sqlite3_close(db);
	if (g_reset) {      /* reset mode: the acked list goes to <acked>.r (too long for the guest console) */
		char p[600];
		snprintf(p, sizeof(p), "%s.r", g_acked_path);
		fclose(g_acked);
		g_acked = fopen(p, "a");
		if (!g_acked) { perror("walrace: acked.r"); return 2; }
	}
	for (int step = from; step < to; step++) {
		pthread_t wt[8], ct[3], rt[3];
		struct warg wa[8], ca[3], ra[3];
		g_writers_left = W;
		g_last_salt = 0;
		for (int c = 0; c < NCKPT; c++) {
			ca[c].step = step; ca[c].w = c;
			pthread_create(&ct[c], NULL, checkpointer, &ca[c]);
		}
		for (int c = 0; c < READER; c++) {
			ra[c].step = step; ra[c].w = c;
			pthread_create(&rt[c], NULL, reader, &ra[c]);
		}
		for (int w = 0; w < W; w++) {
			wa[w].step = step; wa[w].w = w;
			pthread_create(&wt[w], NULL, writer, &wa[w]);
		}
		for (int w = 0; w < W; w++) pthread_join(wt[w], NULL);
		for (int c = 0; c < NCKPT; c++) pthread_join(ct[c], NULL);
		for (int c = 0; c < READER; c++) pthread_join(rt[c], NULL);
	}
	struct rusage ru;
	getrusage(RUSAGE_SELF, &ru);
	/* how much the threads were actually interleaved: on hydo's one vCPU every switch between
	 * them is a context switch of this process (voluntary = blocked/yielded, involuntary =
	 * preempted at a tick) */
	char line[320];
	snprintf(line, sizeof(line), "walrace: done steps=%d..%d checkpoints=%ld busy=%ld wal_empty_after=%ld "
		 "csw_voluntary=%ld csw_involuntary=%ld\n", from, to, g_ckpt, g_ckpt_busy, g_ckpt_restartish,
		 ru.ru_nvcsw, ru.ru_nivcsw);
	if (g_count) {      /* appended to the same line, so the old parsers still match */
		size_t n = strlen(line) - 1;
		snprintf(line + n, sizeof(line) - n, " wal_restarts=%ld ckpt_restart_ok=%ld complete=%ld complete_ge256=%ld\n",
			 g_restarts, g_ckpt_restart_ok, g_complete_small + g_complete_big, g_complete_big);
	}
	fputs(line, stdout);
	FILE *con = fopen("/dev/console", "w");     /* also on the console, for the explorer's logs */
	if (con) { fputs(line, con); fclose(con); }
	return 0;
}

/* exit 0 = holds, 1 = violated, 2 = could not be evaluated */
static int check(const char *acked)
{
	sqlite3 *db;
	if (sqlite3_open_v2(g_db, &db, SQLITE_OPEN_READWRITE, NULL) != SQLITE_OK) {
		printf("HYDO-ORACLE: VIOLATION cannot open the database: %s\n", sqlite3_errmsg(db));
		return 1;
	}
	sqlite3_busy_timeout(db, 20000);
	sqlite3_stmt *st;
	int bad = 0;
	if (sqlite3_prepare_v2(db, "PRAGMA integrity_check", -1, &st, 0) != SQLITE_OK) {
		printf("HYDO-ORACLE: VIOLATION integrity_check failed to run: %s\n", sqlite3_errmsg(db));
		return 1;
	}
	char first[300] = "";
	int nmsg = 0;
	while (sqlite3_step(st) == SQLITE_ROW) {
		const char *m = (const char *)sqlite3_column_text(st, 0);
		if (m && strcmp(m, "ok") != 0) {
			if (!nmsg) snprintf(first, sizeof(first), "%s", m);
			nmsg++;
		}
	}
	sqlite3_finalize(st);
	if (nmsg) {
		printf("HYDO-ORACLE: VIOLATION database corrupt (integrity_check: %d problems; first: %s)\n", nmsg, first);
		bad = 1;
	}
	FILE *f = fopen(acked, "r");
	char rpath[600];
	snprintf(rpath, sizeof(rpath), "%s.r", acked);
	FILE *f2 = fopen(rpath, "r");       /* reset mode's acked list; absent otherwise */
	long long tx, n = 0, lost = 0, wrong = 0, firstbad = -1;
	sqlite3_prepare_v2(db, "SELECT id, val, n FROM t WHERE id >= ?1 AND id < ?1 + 8 ORDER BY id", -1, &st, 0);
	while ((f && fscanf(f, "%lld", &tx) == 1) || (f2 && fscanf(f2, "%lld", &tx) == 1)) {
		n++;
		sqlite3_reset(st);
		sqlite3_bind_int64(st, 1, tx * 8);
		int rc, got = 0, want = -1, bad = 0;
		while ((rc = sqlite3_step(st)) == SQLITE_ROW) {
			long long id = sqlite3_column_int64(st, 0);
			if (sqlite3_column_int64(st, 1) != value_of(id)) bad = 1;
			if (id == tx * 8) want = sqlite3_column_int(st, 2);
			got++;
		}
		if (rc != SQLITE_DONE) {
			printf("HYDO-ORACLE: VIOLATION read of committed transaction %lld failed: %s\n", tx, sqlite3_errmsg(db));
			sqlite3_finalize(st); sqlite3_close(db);
			return 1;
		}
		if (want < 0 || got != want) { lost++; if (firstbad < 0) firstbad = tx; }
		else if (bad) { wrong++; if (firstbad < 0) firstbad = tx; }
	}
	if (f) fclose(f);
	if (f2) fclose(f2);
	sqlite3_finalize(st);
	sqlite3_close(db);
	if (lost || wrong) {
		printf("HYDO-ORACLE: VIOLATION committed transactions lost=%lld wrong=%lld of %lld (first tx %lld)\n", lost, wrong, n, firstbad);
		return 1;
	}
	if (bad) return 1;
	printf("committed=%lld transactions all present, integrity ok (sqlite %s)\n", n, sqlite3_libversion());
	return 0;
}

int main(int argc, char **argv)
{
	if (argc == 7 && !strcmp(argv[1], "run")) {
		g_db = argv[2];
		g_acked_path = argv[3];
		g_acked = fopen(argv[3], "a");
		g_out = fopen(argv[4], "a");
		if (!g_acked || !g_out) { perror("walrace: acked/outcomes"); return 2; }
		return run(atoi(argv[5]), atoi(argv[6]));
	}
	if (argc == 4 && !strcmp(argv[1], "check")) {
		g_db = argv[2];
		return check(argv[3]);
	}
	fprintf(stderr, "usage: walrace run <db> <acked> <outcomes> <from> <to> | walrace check <db> <acked>\n");
	return 2;
}

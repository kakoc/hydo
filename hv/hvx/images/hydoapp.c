// SPDX-License-Identifier: GPL-2.0
/*
 * hydoapp - milestone 30 (Rung 4) application tier for the multi-service loopback system.
 * ============================================================================
 *
 * This is ONE static binary with two roles selected by argv[1], so the demo is three
 * co-located processes talking over the in-kernel 127.0.0.1 loopback:
 *
 *     client (hydoapp load/verify/ping)  --loopback:8080-->  app-server (hydoapp server)
 *                                                            --loopback:6379-->  redis-server
 *
 * The app-server is a REAL second service (service A): it listens on 127.0.0.1:8080, and
 * is itself BACKED BY REDIS (service B) over loopback, speaking Redis's RESP protocol. It
 * is a thin application tier / aggregator, NOT a proxy that leaks the store to the client:
 *   - LOAD <n>   : the app writes led:0..led:n-1 into redis (value = a deterministic
 *                  function of the key) and, once redis has +OK'd all n, ACKs the client.
 *                  Each ACK means "the SYSTEM accepted n committed writes."
 *   - VERIFY <n> : the app reads led:0..led:n-1 back from redis (one MGET), computes an
 *                  AGGREGATE (sum of the committed integers + which keys are missing), and
 *                  returns it. This is the app doing real application work over the store.
 *   - PING       : readiness probe.
 *
 * The SYSTEM-LEVEL invariant (checked by /init after a crash-recover): the aggregate the
 * app RECOMPUTES from redis must equal the aggregate the client COMMITTED. A durability
 * fault that makes redis lose a committed write makes the app return a WRONG/STALE answer
 * to the client -- the app (service A) and the client's committed history DISAGREE. That
 * cross-service disagreement is the Rung-4 bug (vs M27, where the invariant was Redis's
 * own data with a direct redis-cli client).
 *
 * Deterministic by construction: fixed keys, value a pure function of the key, NO time and
 * NO RNG. Built static+reproducible with the pinned musl gcc (build-linux.sh 2j).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <signal.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>

#define APP_PORT     8080
#define REDIS_PORT   6379
#define KEY_PREFIX   "led:"
#define VBASE        100000        /* value of key k is VBASE+k */
#define VPAD_LEN     1024          /* fixed non-space pad so each committed value spans an
                                    * AOF page (a per-request DROP then loses a NAMED key) */
#define LINE_CAP     4096
#define VAL_CAP      (VPAD_LEN + 64)

static char PAD[VPAD_LEN + 1];
static void pad_init(void)
{
	int i;
	for (i = 0; i < VPAD_LEN; i++)
		PAD[i] = "0123456789abcdef"[i & 15];   /* deterministic, no spaces/CR/LF */
	PAD[VPAD_LEN] = '\0';
}

/* ---- a tiny buffered socket reader (read may return partial; RESP needs framing) ---- */
struct rb { int fd; unsigned char b[16384]; int p, n; };
static void rb_init(struct rb *r, int fd) { r->fd = fd; r->p = r->n = 0; }
static int rb_fill(struct rb *r)
{
	if (r->p < r->n)
		return 1;
	r->p = 0;
	do { r->n = read(r->fd, r->b, sizeof(r->b)); } while (r->n < 0 && errno == EINTR);
	return r->n > 0;
}
static int rb_getc(struct rb *r)
{
	if (!rb_fill(r))
		return -1;
	return r->b[r->p++];
}
/* read one line (up to '\n'); strip trailing '\r'; NUL-terminate; return length or -1. */
static int rb_line(struct rb *r, char *out, int cap)
{
	int i = 0, c;
	while ((c = rb_getc(r)) >= 0) {
		if (c == '\n') {
			while (i > 0 && out[i - 1] == '\r')
				i--;
			out[i] = '\0';
			return i;
		}
		if (i < cap - 1)
			out[i++] = (char)c;
	}
	if (i > 0) { out[i] = '\0'; return i; }
	return -1;
}
/* read exactly n bytes (into out, or discard if out==NULL). */
static int rb_readn(struct rb *r, char *out, int n)
{
	int got = 0;
	while (got < n) {
		int take;
		if (!rb_fill(r))
			return -1;
		take = r->n - r->p;
		if (take > n - got)
			take = n - got;
		if (out)
			memcpy(out + got, r->b + r->p, take);
		r->p += take;
		got += take;
	}
	return got;
}

static int wfull(int fd, const void *buf, size_t n)
{
	const char *p = buf;
	while (n) {
		ssize_t w = write(fd, p, n);
		if (w < 0) { if (errno == EINTR) continue; return -1; }
		p += w; n -= (size_t)w;
	}
	return 0;
}

static int tcp_connect(unsigned short port)
{
	struct sockaddr_in a;
	int fd = socket(AF_INET, SOCK_STREAM, 0), one = 1;
	if (fd < 0)
		return -1;
	setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
	memset(&a, 0, sizeof(a));
	a.sin_family = AF_INET;
	a.sin_port = htons(port);
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);   /* 127.0.0.1 */
	if (connect(fd, (struct sockaddr *)&a, sizeof(a)) < 0) { close(fd); return -1; }
	return fd;
}

/* ================= app-server -> redis (RESP over a persistent connection) ============ */
static int g_redis = -1;
static struct rb g_rrb;

static int redis_connect(void)
{
	if (g_redis >= 0)
		close(g_redis);
	g_redis = tcp_connect(REDIS_PORT);
	if (g_redis >= 0)
		rb_init(&g_rrb, g_redis);
	return g_redis;
}
static int redis_ensure(void) { return g_redis >= 0 ? g_redis : redis_connect(); }

/* SET led:<k> <VBASE+k>_<PAD>  (inline command; value is a single space-free token). */
static int redis_set(int k)
{
	char line[VAL_CAP + 64], reply[LINE_CAP];
	int len, attempt;
	len = snprintf(line, sizeof(line), "SET " KEY_PREFIX "%d %d_%s\r\n", k, VBASE + k, PAD);
	for (attempt = 0; attempt < 2; attempt++) {
		if (redis_ensure() < 0)
			return -1;
		if (wfull(g_redis, line, (size_t)len) == 0 &&
		    rb_line(&g_rrb, reply, sizeof(reply)) >= 0)
			return reply[0] == '+' ? 0 : -1;   /* "+OK" */
		redis_connect();                            /* stale connection: reconnect + retry */
	}
	return -1;
}

/* MGET led:0 .. led:n-1 -> aggregate. total = sum of present integer values; missing =
 * count of nil replies; firstmiss = smallest missing key (-1 if none). Returns 0 / -1. */
static int redis_mget(int n, long long *total, int *present, int *missing, int *firstmiss)
{
	char cmd[64 * 1024], scratch[VAL_CAP + 8], hdr[64];
	int off = 0, i, attempt;

	off += snprintf(cmd + off, sizeof(cmd) - off, "MGET");
	for (i = 0; i < n; i++)
		off += snprintf(cmd + off, sizeof(cmd) - off, " " KEY_PREFIX "%d", i);
	off += snprintf(cmd + off, sizeof(cmd) - off, "\r\n");

	for (attempt = 0; attempt < 2; attempt++) {
		int count;
		if (redis_ensure() < 0)
			return -1;
		if (wfull(g_redis, cmd, (size_t)off) != 0 ||
		    rb_line(&g_rrb, hdr, sizeof(hdr)) < 0 || hdr[0] != '*') {
			redis_connect();
			continue;
		}
		count = atoi(hdr + 1);
		*total = 0; *present = 0; *missing = 0; *firstmiss = -1;
		for (i = 0; i < count; i++) {
			int vlen;
			if (rb_line(&g_rrb, hdr, sizeof(hdr)) < 0 || hdr[0] != '$')
				return -1;
			vlen = atoi(hdr + 1);
			if (vlen < 0) {                     /* nil: key absent (lost) */
				(*missing)++;
				if (*firstmiss < 0)
					*firstmiss = i;
				continue;
			}
			if (vlen < (int)sizeof(scratch)) {
				if (rb_readn(&g_rrb, scratch, vlen) < 0)
					return -1;
				scratch[vlen] = '\0';
				*total += strtoll(scratch, NULL, 10);
			} else {                            /* value larger than scratch: discard */
				if (rb_readn(&g_rrb, NULL, vlen) < 0)
					return -1;
			}
			if (rb_readn(&g_rrb, NULL, 2) < 0)  /* trailing CRLF */
				return -1;
			(*present)++;
		}
		return 0;
	}
	return -1;
}

/* ================================ the app-server ===================================== */
static void serve_conn(int c)
{
	struct rb rb;
	char req[LINE_CAP], reply[256];
	int n;

	rb_init(&rb, c);
	if (rb_line(&rb, req, sizeof(req)) < 0)
		return;

	if (strncmp(req, "PING", 4) == 0) {
		wfull(c, "PONG\n", 5);
	} else if (strncmp(req, "LOAD ", 5) == 0) {
		int k, want = atoi(req + 5), ok = 1;
		for (k = 0; k < want; k++)
			if (redis_set(k) != 0) { ok = 0; break; }
		if (ok)
			n = snprintf(reply, sizeof(reply), "ACK count=%d\n", want);
		else
			n = snprintf(reply, sizeof(reply), "ERR redis set failed at key=%d\n", k);
		wfull(c, reply, (size_t)n);
	} else if (strncmp(req, "VERIFY ", 7) == 0) {
		int want = atoi(req + 7), present = 0, missing = 0, firstmiss = -1;
		long long total = 0;
		if (redis_mget(want, &total, &present, &missing, &firstmiss) == 0)
			n = snprintf(reply, sizeof(reply),
				     "SUM total=%lld count=%d missing=%d firstmiss=%d\n",
				     total, present, missing, firstmiss);
		else
			n = snprintf(reply, sizeof(reply), "ERR redis mget failed\n");
		wfull(c, reply, (size_t)n);
	} else {
		wfull(c, "ERR unknown\n", 12);
	}
}

static int run_server(void)
{
	struct sockaddr_in a;
	int ls, one = 1;

	signal(SIGPIPE, SIG_IGN);
	ls = socket(AF_INET, SOCK_STREAM, 0);
	if (ls < 0) { perror("socket"); return 1; }
	setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
	memset(&a, 0, sizeof(a));
	a.sin_family = AF_INET;
	a.sin_port = htons(APP_PORT);
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (bind(ls, (struct sockaddr *)&a, sizeof(a)) < 0) { perror("bind"); return 1; }
	if (listen(ls, 16) < 0) { perror("listen"); return 1; }
	fprintf(stderr, "hydoapp: server listening on 127.0.0.1:%d (backed by redis:%d)\n",
		APP_PORT, REDIS_PORT);
	for (;;) {
		int c = accept(ls, NULL, NULL);
		if (c < 0) { if (errno == EINTR) continue; perror("accept"); break; }
		serve_conn(c);
		close(c);
	}
	return 1;
}

/* ================================ the client role ==================================== */
static int run_client(const char *line, int print_reply)
{
	char reply[512];
	struct rb rb;
	int fd = tcp_connect(APP_PORT), len;

	if (fd < 0) { fprintf(stderr, "hydoapp: cannot connect to app:%d\n", APP_PORT); return 3; }
	if (wfull(fd, line, strlen(line)) != 0) { close(fd); return 3; }
	rb_init(&rb, fd);
	len = rb_line(&rb, reply, sizeof(reply));
	close(fd);
	if (len < 0) { fprintf(stderr, "hydoapp: no reply from app\n"); return 3; }
	if (print_reply)
		printf("%s\n", reply);
	if (strncmp(reply, "ACK", 3) == 0 || strncmp(reply, "SUM", 3) == 0 ||
	    strncmp(reply, "PONG", 4) == 0)
		return 0;
	return 1;
}

int main(int argc, char **argv)
{
	pad_init();
	if (argc < 2) {
		fprintf(stderr, "usage: %s server | load <n> | verify <n> | ping\n", argv[0]);
		return 2;
	}
	if (strcmp(argv[1], "server") == 0)
		return run_server();
	if (strcmp(argv[1], "load") == 0 && argc >= 3) {
		char l[64];
		snprintf(l, sizeof(l), "LOAD %d\n", atoi(argv[2]));
		return run_client(l, 1);
	}
	if (strcmp(argv[1], "verify") == 0 && argc >= 3) {
		char l[64];
		snprintf(l, sizeof(l), "VERIFY %d\n", atoi(argv[2]));
		return run_client(l, 1);
	}
	if (strcmp(argv[1], "ping") == 0)
		return run_client("PING\n", 1);
	fprintf(stderr, "hydoapp: unknown role '%s'\n", argv[1]);
	return 2;
}

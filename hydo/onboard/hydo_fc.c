// SPDX-License-Identifier: GPL-2.0
/*
 * hydo_fc -- the "freeze at a call" fault of manifest-built hydo images (F= token,
 * hydo/onboard/MANIFEST.md "Freeze at a call", hydo/onboard/hydo-init-freeze.sh).
 *
 * The init puts every TCP packet between a service and one of its peers into netfilter queue
 * <queue> (iptables -j NFQUEUE in OUTPUT, both directions) from workload step <step> on, and
 * starts this helper. The helper reads each queued packet, counts the packets that carry TCP
 * payload in the chosen direction (and, if given, contain the match bytes), and accepts them.
 * At the n-th such packet it HOLDS that packet, sends SIGSTOP to the service's whole process
 * tree, waits until every thread of the tree is in the stopped state, and only then accepts
 * the packet. So the service cannot run a single instruction past that event:
 *   dir req  (svc > target): the packet the service just sent; the peer receives it while the
 *            service is frozen (the call is "in flight" during the freeze)
 *   dir resp (svc < target): the peer's packet to the service; the kernel buffers it, the
 *            service reads it only after the freeze
 * It runs SCHED_FIFO, so on the guest's vCPU it preempts the sender at the next preemption
 * point; under hvx everything in the guest is deterministic, so the freeze lands on the same
 * instruction every run. After <dur> ms of guest time (CLOCK_MONOTONIC) it sends SIGCONT to the
 * same processes, then keeps accepting packets until the init removes the rules and kills it.
 * n = 0 never fires: a trace of the traffic (the log), for finding the n of an event.
 *
 * usage: hydo_fc <queue> <pidfile> <svc-ip|-> <target-port|s<own-port>|-> <req|resp> <n> <match|-> <dur-ms>
 *                <logfile> <markerdir>
 *   direction of a packet: if svc-ip is given, req = source address is svc-ip; else with
 *   s<own-port> (the peer is the workload client) req = source port is the service's own port;
 *   otherwise req = destination port is target-port. match: bytes, %xx escapes any byte.
 * Console lines (stdout): "hydo-fc: FROZE ...", "hydo-fc: RESUMED ...", one HYDO-ASSERT
 * reachable record. Marker files <markerdir>/fc.fired and fc.resumed.
 *
 * Raw netlink (no libnetfilter_queue): built static with the pinned musl gcc (docs/BUILD-PINS.md).
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <linux/netlink.h>

/* linux/netfilter/nfnetlink.h + nfnetlink_queue.h, the parts used here */
#define NFNL_SUBSYS_QUEUE 3
#define NFQNL_MSG_PACKET 0
#define NFQNL_MSG_VERDICT 1
#define NFQNL_MSG_CONFIG 2
#define NFQNL_CFG_CMD_BIND 1
#define NFQA_CFG_CMD 1
#define NFQA_CFG_PARAMS 2
#define NFQA_CFG_QUEUE_MAXLEN 3
#define NFQNL_COPY_PACKET 2
#define NFQA_PACKET_HDR 1
#define NFQA_VERDICT_HDR 2
#define NFQA_PAYLOAD 10
#define NF_ACCEPT 1
struct nfgenmsg { uint8_t nfgen_family, version; uint16_t res_id; };
struct nfqnl_msg_config_cmd { uint8_t command, _pad; uint16_t pf; };
struct nfqnl_msg_config_params { uint32_t copy_range; uint8_t copy_mode; } __attribute__((packed));
struct nfqnl_msg_packet_hdr { uint32_t packet_id; uint16_t hw_protocol; uint8_t hook; } __attribute__((packed));
struct nfqnl_msg_verdict_hdr { uint32_t verdict, id; };

static int nl, queue, logfd = -1, nlog;
static char buf[1 << 18];
static const char *mdir;

static void out(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void out(const char *fmt, ...)
{
	char b[1024];
	va_list ap;
	va_start(ap, fmt);
	int n = vsnprintf(b, sizeof b, fmt, ap);
	va_end(ap);
	if (n > (int)sizeof b - 1) n = sizeof b - 1;
	if (write(1, b, n) < 0) {}
}

static void marker(const char *name)
{
	char p[512];
	snprintf(p, sizeof p, "%s/%s", mdir, name);
	int fd = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	if (fd >= 0) close(fd);
}

static void addattr(struct nlmsghdr *h, int type, const void *d, int len)
{
	struct nlattr *a = (struct nlattr *)((char *)h + NLMSG_ALIGN(h->nlmsg_len));
	a->nla_type = type;
	a->nla_len = NLA_HDRLEN + len;
	memcpy((char *)a + NLA_HDRLEN, d, len);
	h->nlmsg_len = NLMSG_ALIGN(h->nlmsg_len) + NLA_ALIGN(a->nla_len);
}

static struct nlmsghdr *msg(char *b, int type, int flags)
{
	memset(b, 0, 256);
	struct nlmsghdr *h = (struct nlmsghdr *)b;
	h->nlmsg_len = NLMSG_LENGTH(sizeof(struct nfgenmsg));
	h->nlmsg_type = (NFNL_SUBSYS_QUEUE << 8) | type;
	h->nlmsg_flags = NLM_F_REQUEST | flags;
	struct nfgenmsg *g = NLMSG_DATA(h);
	g->nfgen_family = AF_UNSPEC;
	g->version = 0;
	g->res_id = htons(queue);
	return h;
}

static int send_ack(struct nlmsghdr *h)
{
	struct sockaddr_nl k = { .nl_family = AF_NETLINK };
	if (sendto(nl, h, h->nlmsg_len, 0, (struct sockaddr *)&k, sizeof k) < 0) return -1;
	char r[1024];
	int n = recv(nl, r, sizeof r, 0);
	if (n < 0) return -1;
	struct nlmsghdr *e = (struct nlmsghdr *)r;
	if (e->nlmsg_type == NLMSG_ERROR && ((struct nlmsgerr *)NLMSG_DATA(e))->error != 0) {
		errno = -((struct nlmsgerr *)NLMSG_DATA(e))->error;
		return -1;
	}
	return 0;
}

static void verdict(uint32_t id)
{
	char b[256];
	struct nlmsghdr *h = msg(b, NFQNL_MSG_VERDICT, 0);
	struct nfqnl_msg_verdict_hdr v = { htonl(NF_ACCEPT), id };   /* id is already big-endian */
	addattr(h, NFQA_VERDICT_HDR, &v, sizeof v);
	struct sockaddr_nl k = { .nl_family = AF_NETLINK };
	if (sendto(nl, h, h->nlmsg_len, 0, (struct sockaddr *)&k, sizeof k) < 0)
		out("hydo-fc: verdict failed: %s\n", strerror(errno));
}

/* ------------------------------------------------------------ the service's process tree */
#define MAXP 512
static int tree[MAXP], ntree;

static int read_file(const char *p, char *b, int len)
{
	int fd = open(p, O_RDONLY);
	if (fd < 0) return -1;
	int n = read(fd, b, len - 1);
	close(fd);
	if (n < 0) return -1;
	b[n] = 0;
	return n;
}

static int stat_of(const char *path, char *state, int *ppid)   /* /proc/.../stat */
{
	char b[1024];
	if (read_file(path, b, sizeof b) < 0) return -1;
	char *p = strrchr(b, ')');
	if (!p) return -1;
	if (state) *state = p[2];
	if (ppid) *ppid = atoi(p + 4);
	return 0;
}

static void collect(int root)   /* root and every descendant (like hydo-init.sh's svc_tree) */
{
	int pid[4096], par[4096], n = 0;
	DIR *d = opendir("/proc");
	struct dirent *e;
	while (d && (e = readdir(d)) && n < 4096) {
		if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
		char p[300];
		int pp;
		snprintf(p, sizeof p, "/proc/%s/stat", e->d_name);
		if (stat_of(p, NULL, &pp) == 0) { pid[n] = atoi(e->d_name); par[n] = pp; n++; }
	}
	if (d) closedir(d);
	ntree = 0;
	tree[ntree++] = root;
	for (int ch = 1; ch;) {
		ch = 0;
		for (int i = 0; i < n; i++) {
			int in = 0, pin = 0;
			for (int j = 0; j < ntree; j++) { in |= tree[j] == pid[i]; pin |= tree[j] == par[i]; }
			if (!in && pin && ntree < MAXP) { tree[ntree++] = pid[i]; ch = 1; }
		}
	}
}

static int all_stopped(void)   /* every thread of every process: stopped (T/t), dead or gone */
{
	for (int i = 0; i < ntree; i++) {
		char p[64];
		snprintf(p, sizeof p, "/proc/%d/task", tree[i]);
		DIR *d = opendir(p);
		if (!d) continue;
		struct dirent *e;
		int ok = 1;
		while ((e = readdir(d))) {
			if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
			char q[320], st = '?';
			snprintf(q, sizeof q, "/proc/%d/task/%s/stat", tree[i], e->d_name);
			if (stat_of(q, &st, NULL) == 0 && !strchr("TtZX", st)) { ok = 0; break; }
		}
		closedir(d);
		if (!ok) return 0;
	}
	return 1;
}

static int64_t now_ms(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return (int64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

/* ------------------------------------------------------------ packets */
static void snippet(char *o, int olen, const unsigned char *p, int n)
{
	int k = 0;
	for (int i = 0; i < n && k < olen - 1; i++)
		o[k++] = (p[i] >= 0x20 && p[i] < 0x7f && p[i] != ' ') ? p[i] : '.';
	o[k] = 0;
}

static int unescape(const char *s, char *o)
{
	int n = 0;
	for (; *s; s++) {
		if (s[0] == '%' && s[1] && s[2]) {
			char h[3] = { s[1], s[2], 0 };
			o[n++] = (char)strtol(h, NULL, 16);
			s += 2;
		} else
			o[n++] = *s;
	}
	return n;
}

int main(int argc, char **argv)
{
	if (argc != 11) {
		fprintf(stderr, "usage: hydo_fc <queue> <pidfile> <svc-ip|-> <target-port|-> <req|resp> <n> "
				"<match|-> <dur-ms> <logfile> <markerdir>\n");
		return 2;
	}
	queue = atoi(argv[1]);
	const char *pidfile = argv[2];
	uint32_t sip = 0;
	int have_sip = strcmp(argv[3], "-") != 0, own = argv[4][0] == 's';
	int tport = strcmp(argv[4], "-") ? atoi(argv[4] + own) : -1;
	if (have_sip) inet_pton(AF_INET, argv[3], &sip);
	int want_req = strcmp(argv[5], "req") == 0;
	long target = atol(argv[6]);
	char match[256];
	int mlen = strcmp(argv[7], "-") ? unescape(argv[7], match) : 0;
	long dur = atol(argv[8]);
	mdir = argv[10];
	logfd = open(argv[9], O_WRONLY | O_CREAT | O_TRUNC, 0644);

	struct sched_param sp = { .sched_priority = 50 };
	/* musl stubs sched_setscheduler() (ENOSYS); the system call itself sets this thread */
	if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &sp) < 0) out("hydo-fc: SCHED_FIFO failed: %s\n", strerror(errno));

	nl = socket(AF_NETLINK, SOCK_RAW, NETLINK_NETFILTER);
	struct sockaddr_nl me = { .nl_family = AF_NETLINK };
	if (nl < 0 || bind(nl, (struct sockaddr *)&me, sizeof me) < 0) { perror("hydo_fc: netlink"); return 1; }
	int rb = 8 << 20;
	setsockopt(nl, SOL_SOCKET, SO_RCVBUFFORCE, &rb, sizeof rb);
	char b[256];
	struct nlmsghdr *h = msg(b, NFQNL_MSG_CONFIG, NLM_F_ACK);
	struct nfqnl_msg_config_cmd cmd = { NFQNL_CFG_CMD_BIND, 0, htons(AF_INET) };
	addattr(h, NFQA_CFG_CMD, &cmd, sizeof cmd);
	if (send_ack(h) < 0) { perror("hydo_fc: bind queue"); return 1; }
	h = msg(b, NFQNL_MSG_CONFIG, NLM_F_ACK);
	struct nfqnl_msg_config_params par = { htonl(0xffff), NFQNL_COPY_PACKET };
	addattr(h, NFQA_CFG_PARAMS, &par, sizeof par);
	uint32_t ml = htonl(8192);
	addattr(h, NFQA_CFG_QUEUE_MAXLEN, &ml, sizeof ml);
	if (send_ack(h) < 0) { perror("hydo_fc: queue params"); return 1; }
	marker("fc.bound");

	long nreq = 0, nresp = 0, counted = 0;
	int fired = 0, resumed = 0;
	int64_t resume_at = 0;
	for (;;) {
		int timeout = -1;
		if (fired && !resumed) {
			int64_t left = resume_at - now_ms();
			timeout = left > 0 ? (int)left : 0;
		}
		struct pollfd pf = { nl, POLLIN, 0 };
		int pr = poll(&pf, 1, timeout);
		if (fired && !resumed && now_ms() >= resume_at) {
			for (int i = 0; i < ntree; i++) kill(tree[i], SIGCONT);
			resumed = 1;
			marker("fc.resumed");
			out("hydo-fc: RESUMED after %ld ms (%d processes)\n", dur, ntree);
		}
		if (pr <= 0) continue;
		int n = recv(nl, buf, sizeof buf, 0);
		if (n < 0) {
			if (errno == ENOBUFS) out("hydo-fc: netlink ENOBUFS (packets dropped)\n");
			continue;
		}
		for (struct nlmsghdr *m = (struct nlmsghdr *)buf; NLMSG_OK(m, n); m = NLMSG_NEXT(m, n)) {
			if ((m->nlmsg_type & 0xff) != NFQNL_MSG_PACKET || (m->nlmsg_type >> 8) != NFNL_SUBSYS_QUEUE) continue;
			uint32_t id = 0;
			const unsigned char *pkt = NULL;
			int plen = 0, have_id = 0;
			int alen = m->nlmsg_len - NLMSG_LENGTH(sizeof(struct nfgenmsg));
			struct nlattr *a = (struct nlattr *)((char *)NLMSG_DATA(m) + NLMSG_ALIGN(sizeof(struct nfgenmsg)));
			while (alen >= (int)NLA_HDRLEN && a->nla_len >= NLA_HDRLEN && a->nla_len <= alen) {
				int t = a->nla_type & NLA_TYPE_MASK;
				if (t == NFQA_PACKET_HDR) { id = ((struct nfqnl_msg_packet_hdr *)((char *)a + NLA_HDRLEN))->packet_id; have_id = 1; }
				if (t == NFQA_PAYLOAD) { pkt = (const unsigned char *)a + NLA_HDRLEN; plen = a->nla_len - NLA_HDRLEN; }
				alen -= NLA_ALIGN(a->nla_len);
				a = (struct nlattr *)((char *)a + NLA_ALIGN(a->nla_len));
			}
			if (!have_id) continue;
			/* IPv4 + TCP payload */
			const unsigned char *pay = NULL;
			int paylen = 0, sport = 0, dport = 0;
			uint32_t saddr = 0;
			if (pkt && plen >= 20 && (pkt[0] >> 4) == 4 && pkt[9] == 6) {
				int ihl = (pkt[0] & 15) * 4, tot = (pkt[2] << 8) | pkt[3];
				memcpy(&saddr, pkt + 12, 4);
				if (plen >= ihl + 20) {
					int doff = (pkt[ihl + 12] >> 4) * 4;
					sport = (pkt[ihl] << 8) | pkt[ihl + 1];
					dport = (pkt[ihl + 2] << 8) | pkt[ihl + 3];
					if (tot > plen) tot = plen;
					if (tot > ihl + doff) { pay = pkt + ihl + doff; paylen = tot - ihl - doff; }
				}
			}
			if (paylen <= 0 || fired) { verdict(id); continue; }
			int is_req = have_sip ? saddr == sip : own ? sport == tport : dport == tport;
			long k = is_req ? ++nreq : ++nresp;
			int hit = is_req == want_req && (!mlen || memmem(pay, paylen, match, mlen));
			if (hit) counted++;
			if (logfd >= 0 && nlog < 200000) {
				char sn[161], line[400], c[32] = "";
				snippet(sn, sizeof sn, pay, paylen < 160 ? paylen : 160);
				if (hit) snprintf(c, sizeof c, " #%ld", counted);
				snprintf(line, sizeof line, "%s%ld %d>%d len=%d%s t=%lld %s\n", is_req ? "req" : "resp", k,
					 sport, dport, paylen, c, (long long)now_ms(), sn);
				if (write(logfd, line, strlen(line)) < 0) {}
				nlog++;
			}
			if (!hit || target <= 0 || counted != target) { verdict(id); continue; }
			/* the n-th event: freeze the service before it can run past it, then let it through */
			char pb[32];
			int root = read_file(pidfile, pb, sizeof pb) > 0 ? atoi(pb) : 0;
			if (root > 1) {
				collect(root);
				for (int i = 0; i < ntree; i++) kill(tree[i], SIGSTOP);
				int w = 0;
				struct timespec ts = { 0, 200000 };
				while (!all_stopped() && w < 10000) { nanosleep(&ts, NULL); w++; }
				char sn[61];
				snippet(sn, sizeof sn, pay, paylen < 60 ? paylen : 60);
				out("hydo-fc: FROZE pid %d (%d processes%s) at %s #%ld (%s%ld, %d>%d len=%d) for %ld ms: %s\n", root, ntree,
				    w >= 10000 ? ", NOT all threads stopped" : "", want_req ? "req" : "resp", counted,
				    is_req ? "req" : "resp", k, sport, dport, paylen, dur, sn);
				out("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"ok\":true,"
				    "\"msg\":\"a service was frozen at a call (F=)\"}\n");
			} else
				out("hydo-fc: n-th event reached but no pid in %s; nothing frozen\n", pidfile);
			fired = 1;
			marker("fc.fired");
			resume_at = now_ms() + dur;
			verdict(id);
		}
	}
}

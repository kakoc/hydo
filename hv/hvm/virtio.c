/*
 * virtio.c -- the virtio-mmio (version 2) transport, device setup from the environment,
 * snapshot, fingerprint. The devices themselves are in virtio_blk.c and virtio_net.c.
 */
#include "virtio_int.h"
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

struct vstate V;
struct vstats VS;
uint8_t *disk;
struct unsync *cache;
uint64_t cache_cap;
uint8_t net_rx[NET_RX_MAX][NET_FRAME_MAX];
uint32_t net_rx_len[NET_RX_MAX];
uint8_t *net_tx;
int blk_idx = -1, net_idx = -1;

static uint8_t *g_ram;
static uint64_t g_base, g_size;

#define LATENCY_EXITS 16              /* hvx's HVX_VBLK_LATENCY / HVX_VNET_LATENCY, in exits of virtual time */

int grw(uint64_t gpa, void *buf, uint64_t len, int to_guest)
{
	if (gpa < g_base || gpa - g_base > g_size || len > g_size - (gpa - g_base)) return 0;
	if (to_guest) memcpy(g_ram + (gpa - g_base), buf, len); else memcpy(buf, g_ram + (gpa - g_base), len);
	return 1;
}

void vdev_complete(struct vdev *d, uint64_t now, uint64_t extra_exits)
{
	d->isr |= 1;
	d->armed = 1;
	d->due = now + (LATENCY_EXITS + extra_exits) * V.step;   /* overwrites an earlier pending kick, as hvx does */
}

/* ---------------- setup ---------------- */
static uint32_t parse_classes(const char *s, int net)
{
	if (!s) return net ? NF_DROP : F_ERROR;
	if (*s >= '0' && *s <= '9') return (uint32_t)strtoul(s, 0, 0);
	uint32_t c = 0;
	if (net) {
		if (strstr(s, "drop")) c |= NF_DROP;
		if (strstr(s, "corrupt")) c |= NF_CORRUPT;
		if (strstr(s, "dup")) c |= NF_DUP;
		return c;
	}
	if (strstr(s, "delay")) c |= F_DELAY;
	if (strstr(s, "error")) c |= F_ERROR;
	if (strstr(s, "unflushed")) c |= F_UNFLUSHED;
	{ const char *p = s; while ((p = strstr(p, "drop"))) { c |= F_DROP; p += 4; } }   /* "drop" (lying disk) */
	if (strstr(s, "reorder")) c |= F_REORDER;
	if (strstr(s, "torn")) c |= F_TORN;
	return c;
}

void virtio_setup(void)
{
	memset(&V, 0, sizeof V);
	const char *dp = getenv("HVM_DISK");
	if (dp) {
		int fd = open(dp, O_RDONLY); if (fd < 0) { perror(dp); exit(1); }
		struct stat st; fstat(fd, &st);
		if (st.st_size % 512 || !st.st_size) { fprintf(stderr, "hvm: %s: size must be a nonzero multiple of 512\n", dp); exit(1); }
		disk = malloc(st.st_size);
		if (read(fd, disk, st.st_size) != st.st_size) { perror("read disk"); exit(1); }
		close(fd);
		blk_idx = V.ndev++;
		V.dev[blk_idx].type = VDEV_BLK;
		V.blk.disk_size = st.st_size;
		if (getenv("HVM_FAULT")) {
			V.blk.seed = strtoull(getenv("HVM_FAULT"), 0, 0);
			V.blk.classes = parse_classes(getenv("HVM_FAULT_CLASSES"), 0);
			V.blk.prob = getenv("HVM_FAULT_PROB") ? atoi(getenv("HVM_FAULT_PROB")) : 100;
			V.blk.max_delay = getenv("HVM_FAULT_MAXDELAY") ? atoi(getenv("HVM_FAULT_MAXDELAY")) : 8;
		}
	}
	if (getenv("HVM_NET")) {
		net_idx = V.ndev++;
		V.dev[net_idx].type = VDEV_NET;
		uint8_t mac[6] = { 0x52, 0x54, 0x00, 0x12, 0x34, 0x56 };
		memcpy(V.net.mac, mac, 6);
		V.net.pace = getenv("HVM_NET_RX_PACE") ? strtoull(getenv("HVM_NET_RX_PACE"), 0, 0) : 4800000;
		V.net.rx_next_at = V.net.pace;
		net_tx = calloc(1, NET_TX_CAP);
		const char *rp = getenv("HVM_NET_RX");
		if (rp) {
			FILE *f = fopen(rp, "rb"); if (!f) { perror(rp); exit(1); }
			uint8_t h[2];
			while (V.net.rx_n < NET_RX_MAX && fread(h, 1, 2, f) == 2) {
				uint32_t n = (h[0] << 8) | h[1];
				if (n > NET_FRAME_MAX || fread(net_rx[V.net.rx_n], 1, n, f) != n) { fprintf(stderr, "hvm: bad frame in %s\n", rp); exit(1); }
				net_rx_len[V.net.rx_n++] = n;
			}
			fclose(f);
		}
		if (getenv("HVM_NET_FAULT")) {
			V.net.seed = strtoull(getenv("HVM_NET_FAULT"), 0, 0);
			V.net.classes = parse_classes(getenv("HVM_NET_FAULT_CLASSES"), 1);
			V.net.prob = getenv("HVM_NET_FAULT_PROB") ? atoi(getenv("HVM_NET_FAULT_PROB")) : 100;
		}
	}
}

void virtio_bind(uint8_t *ram, uint64_t base, uint64_t size, uint64_t step) { g_ram = ram; g_base = base; g_size = size; V.step = step; }
int virtio_ndev(void) { return V.ndev; }
int virtio_attached(void) { return V.ndev > 0; }
void virtio_dev_info(int i, uint64_t *base, int *spi) { *base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_SIZE; *spi = VIRTIO_SPI_BASE + i; }

/* ---------------- transport ---------------- */
static void vdev_reset(int i)
{
	uint32_t type = V.dev[i].type;
	memset(&V.dev[i], 0, sizeof V.dev[i]);
	V.dev[i].type = type;
	if (i == blk_idx) blk_reset();
}

static uint64_t dev_features(int i) { return (1ULL << 32) /* VERSION_1 */ | (i == blk_idx ? blk_features() : (1ULL << 5) /* NET_F_MAC */); }

int virtio_mmio(uint64_t ipa, int write, int size, uint64_t *val, uint64_t now)
{
	if (ipa < VIRTIO_MMIO_BASE || ipa >= VIRTIO_MMIO_BASE + V.ndev * VIRTIO_MMIO_SIZE) return 0;
	int i = (ipa - VIRTIO_MMIO_BASE) / VIRTIO_MMIO_SIZE;
	uint64_t off = (ipa - VIRTIO_MMIO_BASE) % VIRTIO_MMIO_SIZE;
	struct vdev *d = &V.dev[i];
	struct vq *q = &d->q[d->qsel < VQ_MAX ? d->qsel : 0];
	int nq = d->type == VDEV_NET ? 2 : 1;
	if (off >= 0x100) {                                   /* device config */
		if (!write) *val = i == blk_idx ? blk_config(off - 0x100, size) : net_config(off - 0x100, size);
		return 1;
	}
	if (!write) {
		uint32_t v = 0;
		switch (off) {
		case 0x000: v = 0x74726976; break;                /* "virt" */
		case 0x004: v = 2; break;
		case 0x008: v = d->type; break;
		case 0x00c: v = 0x6f647968; break;                /* "hydo" */
		case 0x010: v = d->dfsel == 0 ? (uint32_t)dev_features(i) : d->dfsel == 1 ? (uint32_t)(dev_features(i) >> 32) : 0; break;
		case 0x034: v = d->qsel < (uint32_t)nq ? VQ_SIZE_MAX : 0; break;
		case 0x044: v = q->ready; break;
		case 0x060: v = d->isr_shown; break;
		case 0x070: v = d->status; break;
		case 0x0fc: v = d->config_gen; break;
		}
		*val = v;
		return 1;
	}
	uint32_t v = (uint32_t)*val;
	switch (off) {
	case 0x014: d->dfsel = v; break;
	case 0x020: if (d->drfsel < 2) d->drv_features = (d->drv_features & ~(0xffffffffULL << (32 * d->drfsel))) | ((uint64_t)v << (32 * d->drfsel)); break;
	case 0x024: d->drfsel = v; break;
	case 0x030: d->qsel = v; break;
	case 0x038: if (d->qsel < (uint32_t)nq) q->num = v; break;
	case 0x044: if (d->qsel < (uint32_t)nq) q->ready = v & 1; break;
	case 0x050:                                          /* QueueNotify: service the queue now */
		VS.kicks++;
		if (i == blk_idx) blk_notify(d, now); else net_notify(d, v, now);
		break;
	case 0x064: d->isr_shown &= ~v; break;               /* InterruptACK */
	case 0x070: if (v == 0) vdev_reset(i); else d->status = v; break;
	case 0x080: q->desc  = (q->desc  & ~0xffffffffULL) | v; break;
	case 0x084: q->desc  = (q->desc  & 0xffffffffULL) | ((uint64_t)v << 32); break;
	case 0x090: q->avail = (q->avail & ~0xffffffffULL) | v; break;
	case 0x094: q->avail = (q->avail & 0xffffffffULL) | ((uint64_t)v << 32); break;
	case 0x0a0: q->used  = (q->used  & ~0xffffffffULL) | v; break;
	case 0x0a4: q->used  = (q->used  & 0xffffffffULL) | ((uint64_t)v << 32); break;
	}
	return 1;
}

void virtio_poll(uint64_t now)
{
	if (net_idx >= 0) net_poll(now);
	for (uint32_t i = 0; i < V.ndev; i++) {
		struct vdev *d = &V.dev[i];
		if (d->armed && now >= d->due) { d->isr_shown |= d->isr; d->isr = 0; d->armed = 0; }
	}
}
int virtio_level(int i) { return V.dev[i].isr_shown != 0; }
uint64_t virtio_next_due(uint64_t now)
{
	uint64_t best = 0;
	for (uint32_t i = 0; i < V.ndev; i++)
		if (V.dev[i].armed && (!best || V.dev[i].due < best)) best = V.dev[i].due;
	if (net_idx >= 0) { uint64_t n = net_next_due(now); if (n && (!best || n < best)) best = n; }
	return best;
}

/* ---------------- fingerprint + snapshot ---------------- */
static uint64_t fnv(const void *p, size_t n, uint64_t h)
{ const uint8_t *b = p; for (size_t i = 0; i < n; i++) { h ^= b[i]; h *= 0x100000001b3ULL; } return h; }

uint64_t virtio_hash(uint64_t h)
{
	h = fnv(&V, sizeof V, h);
	if (disk) h = fnv(disk, V.blk.disk_size, h);
	for (uint64_t k = 0; k < V.blk.n_unsync; k++) {
		h = fnv(&cache[k].doff, 8, h); h = fnv(&cache[k].len, 4, h); h = fnv(&cache[k].valid, 4, h);
		h = fnv(cache[k].data, cache[k].len, h);
	}
	if (net_tx) h = fnv(net_tx, V.net.tx_len, h);
	return h;
}

struct vsave { struct vstate S; uint8_t *disk, *tx; struct unsync *cache; };
void *virtio_save(void)
{
	struct vsave *s = calloc(1, sizeof *s);
	s->S = V;
	if (disk) { s->disk = malloc(V.blk.disk_size); memcpy(s->disk, disk, V.blk.disk_size); }
	if (net_tx) { s->tx = malloc(NET_TX_CAP); memcpy(s->tx, net_tx, NET_TX_CAP); }
	if (V.blk.n_unsync) {
		s->cache = calloc(V.blk.n_unsync, sizeof *s->cache);
		for (uint64_t k = 0; k < V.blk.n_unsync; k++) {
			s->cache[k] = cache[k];
			s->cache[k].data = malloc(cache[k].len); memcpy(s->cache[k].data, cache[k].data, cache[k].len);
		}
	}
	return s;
}
void virtio_restore(const void *p)
{
	const struct vsave *s = p;
	for (uint64_t k = 0; k < V.blk.n_unsync; k++) free(cache[k].data);
	V = s->S;
	if (disk) memcpy(disk, s->disk, V.blk.disk_size);
	if (net_tx) memcpy(net_tx, s->tx, NET_TX_CAP);
	if (V.blk.n_unsync > cache_cap) { cache_cap = V.blk.n_unsync; cache = realloc(cache, cache_cap * sizeof *cache); }
	for (uint64_t k = 0; k < V.blk.n_unsync; k++) {
		cache[k] = s->cache[k];
		cache[k].data = malloc(s->cache[k].len); memcpy(cache[k].data, s->cache[k].data, s->cache[k].len);
	}
}

void virtio_finish(void)
{
	const char *o = getenv("HVM_DISK_OUT");
	if (o && disk) {
		FILE *f = fopen(o, "wb"); if (!f) { perror(o); return; }
		fwrite(disk, 1, V.blk.disk_size, f); fclose(f);          /* stable media only: the volatile cache is lost */
	}
	o = getenv("HVM_NET_TX_OUT");
	if (o && net_tx) { FILE *f = fopen(o, "wb"); if (f) { fwrite(net_tx, 1, V.net.tx_len, f); fclose(f); } }
}

void virtio_stats(const char *tag)
{
	if (!V.ndev) return;
	printf("%s: virtio kicks %llu, blk requests %llu (reads %llu, writes %llu, flushes %llu), faults %llu, "
	       "writes held in the volatile cache %llu (committed by flush %llu, still held %llu), net rx %llu tx %llu\n",
	       tag, VS.kicks, VS.reqs, VS.reads, VS.writes, VS.flushes, V.blk.fault_count + V.net.fault_count, VS.held, VS.committed,
	       V.blk.n_unsync, VS.rx, VS.tx);
}

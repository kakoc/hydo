/*
 * virtio_net.c -- a minimal virtio-net device (hvx M23/M24 model): MAC + VERSION_1 only,
 * no checksum/GSO offloads, no control queue, and no F_STATUS (so Linux starts the link
 * up). Inbound frames come from a script (HVM_NET_RX) and are delivered PACED, one every
 * V.net.pace virtual ticks into a posted receive buffer: a driver posts its receive ring
 * at open, before the interface is fully up, so delivering at the receive kick would
 * drop the frames (the M23 wall). Outbound frames are captured (HVM_NET_TX_OUT).
 * Optional faults per packet, the hvx M24 rolls: drop, corrupt, duplicate.
 * Most stacks need only the guest's loopback, which needs no device at all.
 */
#include "virtio_int.h"

#define HDR_LEN 12
#define RX_SALT 0x0052000000000000ULL
#define TX_SALT 0x0054000000000000ULL

uint64_t net_config(uint64_t off, int size)
{
	uint64_t v = 0;
	for (int k = 0; k < size && k < 8; k++) {
		uint64_t o = off + k, b = 0;
		if (o < 6) b = V.net.mac[o];
		else if (o == 8) b = 1;                          /* max_virtqueue_pairs = 1 */
		v |= b << (8 * k);
	}
	return v;
}

enum nf { NF_NONE, NFD, NFC, NFU };
static enum nf decide(int dir, uint64_t idx, uint32_t *cpos, uint8_t *cxor)
{
	*cpos = 0; *cxor = 0;
	if (!V.net.classes) return NF_NONE;
	uint64_t h = fault_mix((V.net.seed ^ (dir ? TX_SALT : RX_SALT)) + (idx + 1) * 0x9e3779b97f4a7c15ULL);
	if ((uint32_t)(h % 100) >= V.net.prob) return NF_NONE;
	enum nf c[3]; int n = 0;                             /* hvx order DROP, (DELAY: RX only, not modeled), CORRUPT, DUPLICATE */
	if (V.net.classes & NF_DROP) c[n++] = NFD;
	if (V.net.classes & NF_CORRUPT) c[n++] = NFC;
	if (V.net.classes & NF_DUP) c[n++] = NFU;
	if (!n) return NF_NONE;
	enum nf s = c[(h >> 8) % (uint64_t)n];
	if (s == NFC) { *cpos = (uint32_t)(h >> 16); *cxor = (uint8_t)((h >> 32) | 1); }
	return s;
}

static int rx_one(struct vdev *d, const uint8_t *frame, uint32_t flen)
{
	struct vq *q = &d->q[0];
	uint16_t avail_idx, head, ui;
	if (!q->ready || !q->num || !grw(q->avail + 2, &avail_idx, 2, 0) || (uint16_t)q->last_avail == avail_idx) return 0;
	if (!grw(q->avail + 4 + 2 * (q->last_avail % q->num), &head, 2, 0)) return 0;
	struct virtq_desc desc;
	if (!grw(q->desc + (uint64_t)head * 16, &desc, 16, 0)) return 0;
	uint8_t hdr[HDR_LEN] = { 0 };
	hdr[10] = 1;                                         /* num_buffers */
	uint32_t used = 0;
	if ((desc.flags & VQ_F_WRITE) && desc.len >= HDR_LEN + flen) {
		grw(desc.addr, hdr, HDR_LEN, 1); grw(desc.addr + HDR_LEN, (void *)frame, flen, 1); used = HDR_LEN + flen;
	}
	if (grw(q->used + 2, &ui, 2, 0)) {
		uint32_t ue[2] = { head, used };
		grw(q->used + 4 + 8 * (ui % q->num), ue, 8, 1); ui++; grw(q->used + 2, &ui, 2, 1);
	}
	q->last_avail = (uint16_t)(q->last_avail + 1);
	return 1;
}

void net_poll(uint64_t now)
{
	struct vdev *d = &V.dev[net_idx];
	if (V.net.rx_head >= V.net.rx_n || now < V.net.rx_next_at || !d->q[0].ready) return;
	uint16_t avail_idx;
	if (!grw(d->q[0].avail + 2, &avail_idx, 2, 0) || (uint16_t)d->q[0].last_avail == avail_idx) return;
	uint8_t f[NET_FRAME_MAX];
	uint32_t n = net_rx_len[V.net.rx_head], cpos; uint8_t cx;
	memcpy(f, net_rx[V.net.rx_head], n);
	V.net.rx_head++;
	V.net.rx_next_at = now + V.net.pace;
	enum nf fc = NF_NONE;
	if (V.net.classes) { fc = decide(0, V.net.rx_seq++, &cpos, &cx); if (fc != NF_NONE) V.net.fault_count++; }
	if (fc == NFD) return;
	if (fc == NFC && n) f[cpos % n] ^= cx;
	int got = rx_one(d, f, n);
	if (fc == NFU) got |= rx_one(d, f, n);
	if (got) { VS.rx++; vdev_complete(d, now, 0); }
}

uint64_t net_next_due(uint64_t now)
{
	(void)now;
	struct vdev *d = &V.dev[net_idx];
	if (V.net.rx_head >= V.net.rx_n || !d->q[0].ready) return 0;
	uint16_t avail_idx;
	if (!grw(d->q[0].avail + 2, &avail_idx, 2, 0) || (uint16_t)d->q[0].last_avail == avail_idx) return 0;
	return V.net.rx_next_at;
}

static void tx_capture(const uint8_t *f, uint32_t n)
{
	if (V.net.tx_len + 2 + n > NET_TX_CAP) return;
	net_tx[V.net.tx_len] = n >> 8; net_tx[V.net.tx_len + 1] = n;
	memcpy(net_tx + V.net.tx_len + 2, f, n);
	V.net.tx_len += 2 + n;
	VS.tx++;
}

void net_notify(struct vdev *d, int qi, uint64_t now)
{
	if (qi != 1) return;                                 /* receive-ring kicks: delivery is paced (net_poll) */
	struct vq *q = &d->q[1];
	uint16_t avail_idx;
	if (!q->ready || !q->num || !grw(q->avail + 2, &avail_idx, 2, 0)) return;
	int any = 0;
	while ((uint16_t)q->last_avail != avail_idx) {
		uint16_t head, di, ui;
		if (!grw(q->avail + 4 + 2 * (q->last_avail % q->num), &head, 2, 0)) break;
		uint8_t f[NET_FRAME_MAX]; uint32_t n = 0, skip = HDR_LEN;
		di = head;
		for (uint32_t g = 0; g < q->num + 2; g++) {
			struct virtq_desc desc;
			if (!grw(q->desc + (uint64_t)di * 16, &desc, 16, 0)) break;
			if (!(desc.flags & VQ_F_WRITE)) {
				uint32_t off = 0, len = desc.len;
				if (skip) { uint32_t s = skip < len ? skip : len; skip -= s; off += s; len -= s; }
				if (len && n + len <= NET_FRAME_MAX) { grw(desc.addr + off, f + n, len, 0); n += len; }
			}
			if (!(desc.flags & VQ_F_NEXT)) break;
			di = desc.next;
		}
		uint32_t cpos; uint8_t cx; enum nf fc = NF_NONE;
		if (V.net.classes) { fc = decide(1, V.net.tx_seq++, &cpos, &cx); if (fc != NF_NONE) V.net.fault_count++; }
		if (fc == NFC && n) f[cpos % n] ^= cx;
		if (fc != NFD) { tx_capture(f, n); if (fc == NFU) tx_capture(f, n); }
		if (grw(q->used + 2, &ui, 2, 0)) {
			uint32_t ue[2] = { head, 0 };
			grw(q->used + 4 + 8 * (ui % q->num), ue, 8, 1); ui++; grw(q->used + 2, &ui, 2, 1);
		}
		q->last_avail = (uint16_t)(q->last_avail + 1);
		any = 1;
	}
	if (any) vdev_complete(d, now, 0);
}

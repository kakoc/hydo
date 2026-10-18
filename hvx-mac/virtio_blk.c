/*
 * virtio_blk.c -- the virtio-blk device and its seeded fault injection, ported from
 * hvx/hvx.c (virtio_blk_process, vblk_fault_decide, the M26 volatile write cache).
 *
 * Same seed -> same schedule as hvx: a request's 64-bit roll is
 * fault_roll(seed, request index) over EVERY request (reads, writes, flushes), a fault
 * applies when (roll % 100) < prob, and the class is chosen among the enabled,
 * applicable classes in the fixed order DELAY, ERROR, DROP, TORN, DROP_UNFLUSHED by
 * ((roll >> 8) % n). REORDER permutes a kick's used-ring entries with the per-kick roll
 * fault_mix((seed ^ 0xD15EA5E5C0FFEE11) + (kick+1)*GOLDEN). DELAY latencies are in exits
 * of virtual time (hvx: instructions); completion = kick time + (16 + summed delays) exits.
 *
 * Differences from hvx, both making the model more faithful, not less: the volatile
 * cache has no size cap (hvx holds at most 256 writes of <= 8 KiB and persists anything
 * larger), and request types other than IN/OUT/FLUSH answer UNSUPP instead of being
 * treated as writes.
 */
#include "virtio_int.h"
#include <stdlib.h>

#define SECTOR 512
#define T_IN 0
#define T_OUT 1
#define T_FLUSH 4
#define S_OK 0
#define S_IOERR 1
#define S_UNSUPP 2
#define REORDER_MAX 16

enum bf { BF_NONE, BF_DELAY, BF_ERROR, BF_DROP, BF_TORN, BF_UNFLUSH };

static enum bf decide(uint64_t idx, uint32_t type, uint32_t *extra)
{
	*extra = 0;
	if (!V.blk.classes) return BF_NONE;
	uint64_t h = fault_roll(V.blk.seed, idx);
	if ((uint32_t)(h % 100) >= V.blk.prob) return BF_NONE;
	enum bf c[5]; int n = 0;
	if (V.blk.classes & F_DELAY) c[n++] = BF_DELAY;
	if (V.blk.classes & F_ERROR) c[n++] = BF_ERROR;
	if ((V.blk.classes & F_DROP) && type == T_OUT) c[n++] = BF_DROP;
	if ((V.blk.classes & F_TORN) && type == T_OUT) c[n++] = BF_TORN;
	if ((V.blk.classes & F_UNFLUSHED) && type == T_OUT) c[n++] = BF_UNFLUSH;
	if (!n) return BF_NONE;
	enum bf s = c[(h >> 8) % (uint64_t)n];
	if (s == BF_DELAY) { uint32_t md = V.blk.max_delay ? V.blk.max_delay : 1; *extra = 1 + (uint32_t)((h >> 16) % md); }
	return s;
}

uint64_t blk_features(void) { return (V.blk.classes & F_UNFLUSHED) ? (1ULL << 9) /* F_FLUSH */ : 0; }
uint64_t blk_config(uint64_t off, int size)
{
	uint64_t cap = V.blk.disk_size / SECTOR;                       /* config offset 0: capacity in sectors (u64) */
	if (off == 0) return size == 8 ? cap : (uint32_t)cap;
	if (off == 4) return (uint32_t)(cap >> 32);
	return 0;
}

static void cache_clear(void)
{
	for (uint64_t k = 0; k < V.blk.n_unsync; k++) free(cache[k].data);
	V.blk.n_unsync = 0;
}
void blk_reset(void) { cache_clear(); }

static int disk_rw(uint64_t gpa, uint64_t doff, uint32_t len, int disk_to_guest)
{
	if (doff > V.blk.disk_size || doff + len > V.blk.disk_size) return 0;
	uint8_t *tmp = disk + doff;
	return grw(gpa, tmp, len, disk_to_guest);
}
/* the volatile write cache (M26): a held write is lost unless a FLUSH commits it */
static int cache_stash(uint64_t doff, uint64_t gpa, uint32_t len)
{
	if (!len || doff > V.blk.disk_size || doff + len > V.blk.disk_size) return 0;
	uint8_t *data = malloc(len);
	if (!grw(gpa, data, len, 0)) { free(data); return 0; }
	if (V.blk.n_unsync == cache_cap) { cache_cap = cache_cap ? 2 * cache_cap : 64; cache = realloc(cache, cache_cap * sizeof *cache); }
	cache[V.blk.n_unsync++] = (struct unsync){ doff, len, 1, data };
	VS.held++;
	return 1;
}
static void cache_overlay(uint64_t gpa, uint64_t doff, uint32_t len)   /* read-your-writes, later writes win */
{
	for (uint64_t k = 0; k < V.blk.n_unsync; k++) {
		if (!cache[k].valid) continue;
		uint64_t es = cache[k].doff, ee = es + cache[k].len, os = es > doff ? es : doff, oe = ee < doff + len ? ee : doff + len;
		if (os < oe) grw(gpa + (os - doff), cache[k].data + (os - es), oe - os, 1);
	}
}
static void cache_invalidate(uint64_t doff, uint32_t len)              /* a newer persisted write supersedes held bytes */
{
	for (uint64_t k = 0; k < V.blk.n_unsync; k++)
		if (cache[k].valid && doff < cache[k].doff + cache[k].len && cache[k].doff < doff + len) cache[k].valid = 0;
}
static void cache_commit(void)
{
	for (uint64_t k = 0; k < V.blk.n_unsync; k++)
		if (cache[k].valid) { memcpy(disk + cache[k].doff, cache[k].data, cache[k].len); VS.committed++; }
	cache_clear();
}

void blk_notify(struct vdev *d, uint64_t now)
{
	struct vq *q = &d->q[0];
	uint16_t avail_idx;
	if (!q->ready || !q->num || !grw(q->avail + 2, &avail_idx, 2, 0)) return;
	int any = 0, do_reorder = (V.blk.classes & F_REORDER) != 0, n_pend = 0;
	uint64_t kick_extra = 0;
	struct { uint32_t id, len; } pend[REORDER_MAX];

	while ((uint16_t)q->last_avail != avail_idx) {
		uint16_t slot = q->last_avail % q->num, head, di;
		if (!grw(q->avail + 4 + 2 * slot, &head, 2, 0)) break;
		uint32_t type = 0, used_len = 0, extra = 0;
		uint64_t sector = 0, doff = 0, status_gpa = 0;
		uint8_t status = S_OK;
		int have_status = 0, first = 1;
		enum bf fc = BF_NONE;
		di = head;
		for (uint32_t g = 0; g < q->num + 2; g++) {
			struct virtq_desc desc;
			if (!grw(q->desc + (uint64_t)di * 16, &desc, 16, 0)) break;
			int has_next = desc.flags & VQ_F_NEXT;
			if (first) {
				uint8_t hdr[16];
				if (grw(desc.addr, hdr, 16, 0)) { memcpy(&type, hdr, 4); memcpy(&sector, hdr + 8, 8); doff = sector * SECTOR; }
				VS.reqs++;
				if (type == T_IN) VS.reads++; else if (type == T_OUT) VS.writes++; else if (type == T_FLUSH) VS.flushes++;
				if (V.blk.classes) {
					fc = decide(V.blk.req_seq++, type, &extra);
					if (fc != BF_NONE) V.blk.fault_count++;
					if (fc == BF_ERROR) status = S_IOERR;
					else if (fc == BF_DELAY) kick_extra += extra;
				}
				if (type == T_FLUSH && fc != BF_ERROR) cache_commit();
				else if (type != T_IN && type != T_OUT && type != T_FLUSH && status == S_OK) status = S_UNSUPP;
			} else if (!has_next) {
				status_gpa = desc.addr; have_status = 1;
			} else if (type == T_IN || type == T_OUT) {
				int ok = 1;
				if (fc == BF_ERROR) {
					status = S_IOERR;
				} else if (type == T_OUT && fc == BF_DROP) {
					cache_invalidate(doff, desc.len);        /* lying disk: acked, never persisted */
				} else if (type == T_OUT && fc == BF_UNFLUSH) {
					if (!cache_stash(doff, desc.addr, desc.len)) { ok = disk_rw(desc.addr, doff, desc.len, 0); cache_invalidate(doff, desc.len); }
				} else if (type == T_OUT && fc == BF_TORN) {
					uint64_t torn_end = sector * SECTOR + SECTOR;
					if (doff < torn_end) ok = disk_rw(desc.addr, doff, doff + desc.len <= torn_end ? desc.len : (uint32_t)(torn_end - doff), 0);
				} else if (type == T_IN) {
					ok = disk_rw(desc.addr, doff, desc.len, 1);
					if (ok) cache_overlay(desc.addr, doff, desc.len);
					used_len += desc.len;
				} else {
					ok = disk_rw(desc.addr, doff, desc.len, 0);
					cache_invalidate(doff, desc.len);
				}
				if (!ok) status = S_IOERR;
				doff += desc.len;
			}
			first = 0;
			if (!has_next) break;
			di = desc.next;
		}
		if (have_status) { grw(status_gpa, &status, 1, 1); used_len += 1; }
		if (do_reorder && n_pend < REORDER_MAX) {
			pend[n_pend].id = head; pend[n_pend].len = used_len; n_pend++;
		} else {
			uint16_t ui;
			if (grw(q->used + 2, &ui, 2, 0)) {
				uint32_t ue[2] = { head, used_len };
				grw(q->used + 4 + 8 * (ui % q->num), ue, 8, 1);
				ui++; grw(q->used + 2, &ui, 2, 1);
			}
		}
		q->last_avail = (uint16_t)(q->last_avail + 1);
		any = 1;
	}
	if (n_pend) {
		uint64_t pr = fault_mix((V.blk.seed ^ 0xD15EA5E5C0FFEE11ULL) + (V.blk.kick_seq + 1) * 0x9e3779b97f4a7c15ULL);
		int order[REORDER_MAX];
		for (int i = 0; i < n_pend; i++) order[i] = i;
		for (int i = n_pend - 1; i > 0; i--) { pr = fault_mix(pr); int j = pr % (uint64_t)(i + 1), t = order[i]; order[i] = order[j]; order[j] = t; }
		uint16_t ui;
		if (grw(q->used + 2, &ui, 2, 0)) {
			for (int i = 0; i < n_pend; i++) {
				uint32_t ue[2] = { pend[order[i]].id, pend[order[i]].len };
				grw(q->used + 4 + 8 * (ui % q->num), ue, 8, 1); ui++;
			}
			grw(q->used + 2, &ui, 2, 1);
		}
	}
	if (any) {
		vdev_complete(d, now, kick_extra);
		if (V.blk.classes) V.blk.kick_seq++;
	}
}

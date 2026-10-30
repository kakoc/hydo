/* virtio_int.h -- state shared by virtio.c, virtio_blk.c and virtio_net.c (not part of hvm.c's interface). */
#ifndef HVM_VIRTIO_INT_H
#define HVM_VIRTIO_INT_H
#include "virtio.h"
#include <string.h>

#define NDEV_MAX 2
#define VQ_MAX   2
#define VQ_SIZE_MAX 256

enum { VDEV_NONE = 0, VDEV_NET = 1, VDEV_BLK = 2 };

struct vq {
	uint32_t num, ready;
	uint64_t desc, avail, used;
	uint32_t last_avail;
};
struct vdev {                         /* virtio-mmio v2 transport */
	uint32_t type;                    /* virtio device id (1 net, 2 blk) */
	uint32_t status, dfsel, drfsel, qsel;
	uint64_t drv_features;
	struct vq q[VQ_MAX];
	uint32_t isr;                     /* interrupt cause latched at a kick, raised at `due` */
	uint32_t isr_shown;               /* InterruptStatus as the guest sees it; the line is (isr_shown != 0) */
	uint32_t armed;
	uint64_t due;                     /* virtual time at which `isr` becomes visible */
	uint32_t config_gen;
};
struct vblk {                         /* fault policy + PRNG counters: identical meaning to hvx's struct hvx_vblk */
	uint64_t seed;
	uint32_t classes, prob, max_delay, pad;
	uint64_t req_seq, kick_seq, fault_count;
	uint64_t disk_size;
	uint64_t n_unsync;                /* entries in the volatile write cache */
};
#define NET_RX_MAX 64
#define NET_FRAME_MAX 1536
struct vnet {
	uint8_t mac[6], pad[2];
	uint32_t rx_n, rx_head;           /* scripted frames, and how many were consumed */
	uint64_t rx_next_at;              /* virtual time of the next paced delivery */
	uint64_t pace;
	uint64_t seed;
	uint32_t classes, prob;
	uint64_t rx_seq, tx_seq, fault_count;
	uint64_t tx_len;                  /* bytes in the TX capture */
};
/* Everything deterministic that is not a buffer; hashed as bytes, so keep it free of padding garbage (memset). */
struct vstate {
	uint32_t ndev, pad;
	uint64_t step;
	struct vdev dev[NDEV_MAX];
	struct vblk blk;
	struct vnet net;
};
struct vstats { uint64_t kicks, reqs, reads, writes, flushes, faults, held, committed, rx, tx; };

struct unsync { uint64_t doff; uint32_t len, valid; uint8_t *data; };

extern struct vstate V;
extern struct vstats VS;
extern uint8_t *disk;                 /* the stable-media image (flushed bytes) */
extern struct unsync *cache;          /* the volatile write cache, insertion order */
extern uint64_t cache_cap;
extern uint8_t net_rx[NET_RX_MAX][NET_FRAME_MAX];
extern uint32_t net_rx_len[NET_RX_MAX];
extern uint8_t *net_tx;
#define NET_TX_CAP (1u << 20)
extern int blk_idx, net_idx;          /* device index of each, -1 when absent */

int grw(uint64_t gpa, void *buf, uint64_t len, int to_guest);
void vdev_complete(struct vdev *d, uint64_t now, uint64_t extra_exits);

/* the hvx fault PRNG (hvx.c fault_mix / fault_roll): splitmix64 finalizer */
static inline uint64_t fault_mix(uint64_t x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}
static inline uint64_t fault_roll(uint64_t seed, uint64_t idx) { return fault_mix(seed + (idx + 1) * 0x9e3779b97f4a7c15ULL); }

/* hvx_abi.h HVX_FAULT_* bits, same values */
#define F_DELAY     (1u << 0)
#define F_ERROR     (1u << 1)
#define F_DROP      (1u << 2)
#define F_REORDER   (1u << 3)
#define F_TORN      (1u << 4)
#define F_UNFLUSHED (1u << 5)
/* hvx_abi.h HVX_NFAULT_* bits */
#define NF_DROP     (1u << 0)
#define NF_CORRUPT  (1u << 2)
#define NF_DUP      (1u << 3)

struct virtq_desc { uint64_t addr; uint32_t len; uint16_t flags, next; };
#define VQ_F_NEXT  1
#define VQ_F_WRITE 2

void blk_notify(struct vdev *d, uint64_t now);
void blk_reset(void);
uint64_t blk_features(void);
uint64_t blk_config(uint64_t off, int size);
void net_notify(struct vdev *d, int q, uint64_t now);
void net_poll(uint64_t now);
uint64_t net_next_due(uint64_t now);
uint64_t net_config(uint64_t off, int size);
#endif

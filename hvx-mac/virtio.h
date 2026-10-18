/*
 * virtio.h -- virtio-mmio block and network devices for hvm.c, with hvx's seeded
 * fault injection (CLAUDE.md M20-M26 semantics, ported from hvx/hvx.c).
 *
 * Everything is driven only by guest MMIO exits and the virtual clock, so a run
 * stays a pure function of its inputs: a kick services the queue at once, and its
 * completion interrupt is raised when the virtual clock reaches
 * kick_time + latency (the M16 discipline). All device state, the disk image and
 * the disk's volatile write cache are in the snapshot and the fingerprint.
 *
 * Configured from the environment (read by virtio_setup):
 *   HVM_DISK=<image>            attach a virtio-blk disk (loaded into memory)
 *   HVM_DISK_OUT=<file>         at exit, write the DURABLE image (flushed bytes only:
 *                               the volatile write cache is not included = power cut)
 *   HVM_FAULT=<seed>            disk fault policy (inert when unset)
 *   HVM_FAULT_CLASSES=<list>    delay,error,drop,reorder,torn,unflushed (default error)
 *                               or a number (hvx HVX_FAULT_* bits)
 *   HVM_FAULT_PROB=<0..100>     default 100
 *   HVM_FAULT_MAXDELAY=<n>      default 8 (exits)
 *   HVM_NET=1                   attach a virtio-net device (MAC 52:54:00:12:34:56)
 *   HVM_NET_RX=<file>           scripted inbound frames: repeated [u16 big-endian len][frame]
 *   HVM_NET_RX_PACE=<ticks>     deliver one frame per this many virtual ticks (default 4800000 = 0.2 s)
 *   HVM_NET_TX_OUT=<file>       at exit, write captured outbound frames in the same format
 *   HVM_NET_FAULT=<seed> (+ HVM_NET_FAULT_CLASSES drop,corrupt,dup  HVM_NET_FAULT_PROB)
 */
#ifndef HVM_VIRTIO_H
#define HVM_VIRTIO_H
#include <stdint.h>
#include <stddef.h>

#define VIRTIO_MMIO_BASE  0x0a000000ULL   /* device i at VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_SIZE */
#define VIRTIO_MMIO_SIZE  0x200
#define VIRTIO_SPI_BASE   16              /* device i uses SPI 16 + i (INTID 48 + i) */

void virtio_setup(void);                                     /* from the environment, before the device tree */
void virtio_bind(uint8_t *ram, uint64_t ram_base, uint64_t ram_size, uint64_t step);
int  virtio_ndev(void);
void virtio_dev_info(int i, uint64_t *base, int *spi);
int  virtio_mmio(uint64_t ipa, int write, int size, uint64_t *val, uint64_t now);  /* 1 if it was ours */
void virtio_poll(uint64_t now);                              /* raise completions that are due */
int  virtio_level(int i);                                    /* device i's interrupt line */
uint64_t virtio_next_due(uint64_t now);                      /* earliest future event, 0 = none */
int  virtio_attached(void);
uint64_t virtio_hash(uint64_t h);                            /* device state + disk + cache + captured TX */
void *virtio_save(void);                                     /* snapshot */
void virtio_restore(const void *s);
void virtio_finish(void);                                    /* write HVM_DISK_OUT / HVM_NET_TX_OUT */
void virtio_stats(const char *tag);
#endif

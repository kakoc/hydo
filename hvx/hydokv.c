/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hydokv - the software-under-test (SUT) for hvx milestone 25: a tiny, real,
 * raw-block key/value store with ONE deliberate, documented, realistic
 * durability bug. It is the "application" whose committed data a SEEDED hvx disk
 * fault silently loses -- the flagship fault-injection demonstration.
 * =============================================================================
 *
 * WHAT IT IS
 * ----------
 * A single-threaded, dependency-free KV store that keeps each record in its own
 * 512-byte sector on a RAW block device (/dev/vda) -- no filesystem, no mkfs, so
 * it fits the 1 MiB hvx disk and adds no new machinery. It does all I/O with
 * O_DIRECT so every read is a REAL device read (never served from the Linux page
 * cache): this is load-bearing for the oracle (a dropped physical write is
 * invisible to a cached read, so without O_DIRECT the durability check would be
 * fake). At most one I/O is outstanding at a time, matching hvx's
 * one-completion-per-kick device model and keeping the SUT itself deterministic.
 *
 * THE DURABILITY BUG (honest, and a real class)
 * ---------------------------------------------
 * `put(key,val)` writes the record and then treats pwrite() success as "durably
 * committed" -- it records the key in its in-memory index and (here) prints it as
 * committed -- BEFORE issuing any durability barrier. hydokv only barriers
 * (fdatasync) once every BATCH puts: an "amortize the fsync" optimization. A put
 * acked in the current, not-yet-barriered batch sits in a power-loss window: if
 * the machine loses power the write can be gone, yet the store already reported
 * it committed. THIS IS THE BUG: a correct store issues the barrier (or uses
 * FUA / O_DSYNC) *before* acking the commit. This is exactly the FoundationDB /
 * Jepsen "unsynchronized write lost on reboot" durability class.
 *
 * The bug is genuine, not a cheat: hydokv is provably CORRECT when nothing is
 * lost (run it with faults disabled and every committed key reads back -- the
 * hvx harness proves this, phase F1). It only fails when a realistic power-loss
 * fault discards an un-barriered write (hvx HVX_FAULT_DROP, phase F2).
 *
 * FAULT <-> REAL-WORLD MAPPING
 * ----------------------------
 * hydokv knows NOTHING about faults. Below it, the hvx hypervisor's seeded
 * virtio-blk fault engine models power loss: HVX_FAULT_DROP acks VIRTIO_BLK_S_OK
 * to the guest (so pwrite() succeeds and the app believes the write landed) but
 * does NOT persist the sector -- precisely "an un-synced write lost on power
 * failure." HVX_FAULT_TORN clips a multi-sector write to its first sector -- a
 * crash mid-write. On real hardware fdatasync forces the device's volatile write
 * cache to stable media; the DROP fault models the case where that never
 * happened for an acked write.
 *
 * WORKLOAD + ORACLE (the "test")
 * ------------------------------
 *   populate: put(key_i, val_i) for i in [0,K); keys/values are a deterministic
 *             function of i (no wall-clock, no RNG), and each acked put is
 *             recorded in a reference model (ground truth of what was committed).
 *   crash:    close + reopen /dev/vda O_DIRECT (drops any cache; models a crash
 *             and a fresh instance recovering only from the on-disk data).
 *   oracle:   for every committed key, get(key) via O_DIRECT and assert it equals
 *             the committed value. Invariant = READ-COMMITTED-DURABILITY: a key
 *             the store said was committed must be readable with its committed
 *             value after a cache-drop. Directly falsified by a lost write.
 *
 * It prints a machine-checkable verdict the hvx harness watches for:
 *   HYDO-ORACLE: OK keys=<K> durable                       (all committed keys survived)
 *   HYDO-ORACLE: VIOLATION key=<k> expected=<v> got=<g>    (a committed key was lost)
 * followed by the stop marker:
 *   hydo-flagship-marker: oracle complete
 *
 * DETERMINISM
 * -----------
 * Fixed K, fixed keys (0..K-1), values a pure function of the key, no timestamps,
 * no getrandom/urandom, single-threaded, one outstanding I/O. Everything hydokv
 * does is a pure function of its (fixed) inputs, so two runs are bit-identical --
 * which the hvx full-state fingerprint proves.
 *
 * Build (static, on the KVM host, musl): see build-linux.sh (staged into
 * build/initramfs-kv.cpio.gz, run by its /init).
 */
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>

#define DEV        "/dev/vda"
#define SECTOR     512u        /* device logical block size (hvx advertises 512) */
#define RECSZ      SECTOR      /* one record per sector */
#define NKEYS      48u         /* committed keys (records at sectors 1..NKEYS) */
#define BATCH      8u          /* the buggy store barriers only every BATCH puts */
#define MAGIC      0x4859444F4B565231ULL   /* "HYDOKVR1" -- a fixed nonzero record tag */

/* Record layout inside a 512-byte sector (fixed, little-endian):
 *   [0]  magic (u64)   [8]  key (u64)   [16] value (u64)   [24] seq (u64)
 *   [32..512) fill byte = 0xC0 + (key & 0x3f)  (deterministic, nonzero) */
#define R_MAGIC 0
#define R_KEY   8
#define R_VALUE 16
#define R_SEQ   24
#define R_FILL  32

/* Deterministic value for a key -- a pure function, no randomness. */
static uint64_t val_of(uint32_t key)
{
	uint64_t hi = 0xA5A5A5A5ULL;
	uint32_t lo = key * 2654435761u + 0x1234u;	/* Knuth mult; nonzero, varied */
	return (hi << 32) | lo;
}

static void put_u64(uint8_t *b, size_t off, uint64_t v)
{
	size_t i;
	for (i = 0; i < 8; i++)
		b[off + i] = (uint8_t)(v >> (8 * i));
}
static uint64_t get_u64(const uint8_t *b, size_t off)
{
	uint64_t v = 0;
	size_t i;
	for (i = 0; i < 8; i++)
		v |= (uint64_t)b[off + i] << (8 * i);
	return v;
}

/* An O_DIRECT-aligned scratch buffer (one page; we use the first RECSZ bytes). */
static uint8_t *aligned_block(void)
{
	void *p = NULL;
	if (posix_memalign(&p, 4096, 4096) != 0 || !p) {
		fprintf(stderr, "hydokv: posix_memalign failed\n");
		_exit(3);
	}
	return (uint8_t *)p;
}

int main(void)
{
	uint8_t *wbuf = aligned_block();
	uint8_t *rbuf = aligned_block();
	uint64_t ref[NKEYS];		/* reference model: value the store ACKED as committed */
	uint8_t did_commit[NKEYS];	/* was key i acknowledged as committed? */
	uint32_t i;
	int fd;
	uint32_t committed = 0, lost = 0;
	int first_viol = -1;
	uint64_t first_exp = 0, first_got = 0;

	memset(ref, 0, sizeof(ref));
	memset(did_commit, 0, sizeof(did_commit));

	printf("hydokv: raw-block KV store on %s (O_DIRECT, %u keys, sector=%u, batch=%u)\n",
	       DEV, NKEYS, SECTOR, BATCH);
	fflush(stdout);

	/* ---- populate: write K records, acking each on pwrite() success ---- */
	fd = open(DEV, O_RDWR | O_DIRECT);
	if (fd < 0) {
		printf("hydokv: open(%s, O_RDWR|O_DIRECT) failed: %s\n", DEV, strerror(errno));
		printf("HYDO-ORACLE: ERROR open failed\n");
		printf("hydo-flagship-marker: oracle complete\n");
		fflush(stdout);
		return 2;
	}
	for (i = 0; i < NKEYS; i++) {
		uint64_t v = val_of(i);
		off_t off = (off_t)(1 + i) * SECTOR;
		ssize_t n;

		memset(wbuf, 0, RECSZ);
		put_u64(wbuf, R_MAGIC, MAGIC);
		put_u64(wbuf, R_KEY, i);
		put_u64(wbuf, R_VALUE, v);
		put_u64(wbuf, R_SEQ, committed);
		memset(wbuf + R_FILL, (int)(0xC0 + (i & 0x3f)), RECSZ - R_FILL);

		n = pwrite(fd, wbuf, RECSZ, off);
		if (n != (ssize_t)RECSZ) {
			/* A real I/O error (not a silent drop). Not this demo's bug; report it. */
			printf("hydokv: pwrite(key=%u) failed: n=%zd %s\n", i, n, strerror(errno));
			continue;
		}
		/* *** THE DURABILITY BUG ***
		 * We ACK the commit HERE, on pwrite() success, and record it in the
		 * reference model -- BEFORE the durability barrier below. The barrier is
		 * amortized to once per BATCH puts, so this ack sits in a not-yet-durable
		 * window. A correct store would barrier (or use FUA/O_DSYNC) before acking. */
		ref[i] = v;
		did_commit[i] = 1;
		committed++;

		if ((committed % BATCH) == 0)
			fdatasync(fd);	/* batched barrier (no-op on a cacheless device;
					 * on real HW it forces the write cache to media) */
	}
	fdatasync(fd);			/* final barrier -- still AFTER we acked every put */
	printf("hydokv: committed %u keys (acked on write success, barrier batched every %u)\n",
	       committed, BATCH);
	fflush(stdout);

	/* ---- crash / recover: close + reopen so reads hit the real device ----
	 * Models the process dying and a fresh instance recovering from disk only.
	 * O_DIRECT already bypasses the page cache; the reopen makes the "crash"
	 * vivid and drops any per-fd state. */
	close(fd);
	fd = open(DEV, O_RDONLY | O_DIRECT);
	if (fd < 0) {
		printf("hydokv: reopen(%s, O_RDONLY|O_DIRECT) failed: %s\n", DEV, strerror(errno));
		printf("HYDO-ORACLE: ERROR reopen failed\n");
		printf("hydo-flagship-marker: oracle complete\n");
		fflush(stdout);
		return 2;
	}
	printf("hydokv: reopened %s after crash/recover; verifying committed keys\n", DEV);
	fflush(stdout);

	/* ---- oracle: every committed key must read back with its committed value ---- */
	for (i = 0; i < NKEYS; i++) {
		off_t off = (off_t)(1 + i) * SECTOR;
		ssize_t n;
		uint64_t magic, key, value;

		if (!did_commit[i])
			continue;	/* the store never acked this key -- not an invariant */

		memset(rbuf, 0, RECSZ);
		n = pread(fd, rbuf, RECSZ, off);
		if (n != (ssize_t)RECSZ) {
			printf("hydokv: pread(key=%u) failed: n=%zd %s\n", i, n, strerror(errno));
		}
		magic = get_u64(rbuf, R_MAGIC);
		key   = get_u64(rbuf, R_KEY);
		value = get_u64(rbuf, R_VALUE);

		if (magic != MAGIC || key != i || value != ref[i]) {
			/* A key the store said was COMMITTED is not durably present. */
			lost++;
			if (first_viol < 0) {
				first_viol = (int)i;
				first_exp  = ref[i];
				first_got  = value;
			}
		}
	}
	close(fd);

	/* ---- verdict ---- */
	if (lost == 0) {
		printf("HYDO-ORACLE: OK keys=%u durable\n", committed);
	} else {
		printf("HYDO-ORACLE: VIOLATION key=%d expected=0x%016llx got=0x%016llx\n",
		       first_viol, (unsigned long long)first_exp, (unsigned long long)first_got);
		printf("hydokv: %u of %u committed keys were LOST (a committed write vanished)\n",
		       lost, committed);
	}
	printf("hydo-flagship-marker: oracle complete\n");
	fflush(stdout);
	return lost == 0 ? 0 : 1;
}

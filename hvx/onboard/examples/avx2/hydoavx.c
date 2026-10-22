// SPDX-License-Identifier: GPL-2.0
/*
 * hydoavx: an AVX2 workload for the guest-AVX example (guest.cpu: v3).
 *
 *   hydoavx <step>
 *
 * Four threads each take a slice of a deterministic array (a function of <step> only) and
 * compute, with AVX2 + FMA intrinsics, (a) an integer hash with VPMULLD / VPADDD / VPXOR /
 * VPSRLD and (b) a float FMA dot product, then the same with plain scalar code; any
 * difference is reported. Each thread folds its results into one shared accumulator under
 * a mutex as `acc = acc * M + x` (order dependent), so the final value also records the
 * order in which the threads finished -- under hydo it must reproduce bit for bit.
 * Prints one line: "hydoavx step=<n> avx2=<0|1> ok=<0|1> hash=<..> dot=<..> acc=<..>".
 * Exit status 0 iff every AVX2 result equals the scalar result.
 */
#include <immintrin.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define N	(1 << 18)	/* elements */
#define THREADS	4
#define ROUNDS	16

static uint32_t ia[N];
static float fa[N], fb[N];
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static uint64_t acc = 0x9e3779b97f4a7c15ULL;
static int bad;
static uint32_t all_hash;
static double all_dot;

struct job { int id; uint32_t hash; float dot; int ok; };

__attribute__((target("avx2,fma")))
static void avx_part(int lo, int hi, uint32_t *h_out, float *d_out)
{
	__m256i h = _mm256_set1_epi32(0x811c9dc5), m = _mm256_set1_epi32(0x01000193);
	__m256 d = _mm256_setzero_ps();
	int i, r, k;
	uint32_t lanes[8];
	float fl[8];

	for (r = 0; r < ROUNDS; r++)
		for (i = lo; i < hi; i += 8) {
			__m256i x = _mm256_loadu_si256((const __m256i *)(ia + i));
			h = _mm256_xor_si256(h, _mm256_add_epi32(x, _mm256_set1_epi32(r)));
			h = _mm256_mullo_epi32(h, m);
			h = _mm256_xor_si256(h, _mm256_srli_epi32(h, 15));
			if (r == 0)
				d = _mm256_fmadd_ps(_mm256_loadu_ps(fa + i), _mm256_loadu_ps(fb + i), d);
		}
	_mm256_storeu_si256((__m256i *)lanes, h);
	_mm256_storeu_ps(fl, d);
	*h_out = 0;
	for (k = 0; k < 8; k++)
		*h_out = *h_out * 31 + lanes[k];
	*d_out = 0;
	for (k = 0; k < 8; k++)
		*d_out += fl[k];
}

static void scalar_part(int lo, int hi, uint32_t *h_out, float *d_out)
{
	uint32_t h[8];
	float d[8];
	int i, r, k;

	for (k = 0; k < 8; k++) { h[k] = 0x811c9dc5; d[k] = 0; }
	for (r = 0; r < ROUNDS; r++)
		for (i = lo; i < hi; i += 8)
			for (k = 0; k < 8; k++) {
				h[k] ^= ia[i + k] + (uint32_t)r;
				h[k] *= 0x01000193;
				h[k] ^= h[k] >> 15;
				if (r == 0)
					d[k] = __builtin_fmaf(fa[i + k], fb[i + k], d[k]);
			}
	*h_out = 0;
	for (k = 0; k < 8; k++)
		*h_out = *h_out * 31 + h[k];
	*d_out = 0;
	for (k = 0; k < 8; k++)
		*d_out += d[k];
}

static void *worker(void *p)
{
	struct job *j = p;
	int lo = j->id * (N / THREADS), hi = lo + N / THREADS;
	uint32_t hs;
	float ds;
	uint64_t x;

	avx_part(lo, hi, &j->hash, &j->dot);
	scalar_part(lo, hi, &hs, &ds);
	j->ok = hs == j->hash && memcmp(&ds, &j->dot, sizeof(ds)) == 0;
	memcpy(&x, &j->dot, sizeof(j->dot));
	x = (x << 32) ^ j->hash ^ ((uint64_t)j->id << 60);
	pthread_mutex_lock(&mu);
	acc = acc * 0x100000001b3ULL + x;	/* order dependent */
	if (!j->ok) bad = 1;
	all_hash = all_hash * 131 + j->hash;
	all_dot += j->dot;
	pthread_mutex_unlock(&mu);
	return NULL;
}

int main(int argc, char **argv)
{
	unsigned step = argc > 1 ? (unsigned)strtoul(argv[1], NULL, 10) : 0;
	uint32_t s = 0x12345678u ^ (step * 0x9e3779b9u);
	pthread_t t[THREADS];
	struct job j[THREADS];
	int i, avx2;

	__builtin_cpu_init();
	avx2 = __builtin_cpu_supports("avx2") && __builtin_cpu_supports("fma");
	if (!avx2) {
		printf("hydoavx step=%u avx2=0 ok=0 (the CPU does not offer AVX2+FMA)\n", step);
		return 1;
	}
	for (i = 0; i < N; i++) {
		s = s * 1664525u + 1013904223u;
		ia[i] = s;
		fa[i] = (float)(s >> 8) / 16777216.0f;
		fb[i] = (float)((s * 2654435761u) >> 8) / 16777216.0f - 0.5f;
	}
	for (i = 0; i < THREADS; i++) {
		j[i].id = i;
		pthread_create(&t[i], NULL, worker, &j[i]);
	}
	for (i = 0; i < THREADS; i++)
		pthread_join(t[i], NULL);
	printf("hydoavx step=%u avx2=1 ok=%d hash=%08x dot=%.6f acc=%016llx\n", step, !bad,
	       all_hash, all_dot, (unsigned long long)acc);
	return bad;
}

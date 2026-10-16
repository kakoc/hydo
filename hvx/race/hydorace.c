// SPDX-License-Identifier: GPL-2.0
/*
 * hydorace - a calibration race for timing exploration under hvx.
 *
 * Two threads sell seats from a shared counter with a deliberate check-then-act
 * race: each reads `seats`, works for W iterations, then writes back seats-1. If
 * the guest kernel preempts a thread inside that window and the other thread
 * sells a seat meanwhile, one sale is lost and more tickets are sold than there
 * were seats. Between sales each thread does OUT iterations of unrelated work, so
 * the race window is a fraction W/(W+OUT) of the run: the smaller W, the rarer
 * the bug. On one vCPU the only way to hit it is a preemption landing in the
 * window, which is what timing perturbation is meant to find.
 *
 * Usage: hydorace W OUT SEATS
 * Prints: HYDO-RACE: W=.. OUT=.. seats=.. sold=.. lost=.. t0=.. t1=..
 * (lost > 0 is the bug.)
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

static volatile long seats;
static long W, OUT;

static void work(long n)
{
	for (volatile long i = 0; i < n; i++)
		;
}

static void *seller(void *arg)
{
	long sold = 0;

	(void)arg;
	for (;;) {
		long s;

		work(OUT);
		s = seats;			/* check ... */
		if (s <= 0)
			break;
		work(W);			/* ... the race window ... */
		seats = s - 1;			/* ... act */
		sold++;
	}
	return (void *)sold;
}

int main(int argc, char **argv)
{
	pthread_t t[2];
	void *r0, *r1;
	long total, nseats;

	if (argc < 4) {
		fprintf(stderr, "usage: %s W OUT SEATS\n", argv[0]);
		return 2;
	}
	W = atol(argv[1]);
	OUT = atol(argv[2]);
	nseats = atol(argv[3]);
	seats = nseats;
	pthread_create(&t[0], NULL, seller, NULL);
	pthread_create(&t[1], NULL, seller, NULL);
	pthread_join(t[0], &r0);
	pthread_join(t[1], &r1);
	total = (long)r0 + (long)r1;
	printf("HYDO-RACE: W=%ld OUT=%ld seats=%ld sold=%ld lost=%ld t0=%ld t1=%ld\n",
	       W, OUT, nseats, total, total - nseats, (long)r0, (long)r1);
	return 0;
}

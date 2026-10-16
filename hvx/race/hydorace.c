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
 * Once-per-run mode (ONCE > 0): the race window exists only while selling seat number ONCE
 * (counting down); every other sale has no window. The bug can then happen at most once per
 * run, at one place. LEAD seats before that, the program prints "HYDO-RACE-NEAR" so a host can
 * snapshot just before the window and try many timings from there.
 *
 * Usage: hydorace W OUT SEATS [ONCE [LEAD]]
 * Prints: HYDO-RACE: W=.. OUT=.. seats=.. sold=.. lost=.. t0=.. t1=..
 * (lost > 0 is the bug.)
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static volatile long seats;
static long W, OUT, ONCE, LEAD;
static volatile int near_printed;

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
		/* the marker is printed OUTSIDE the check-then-act window: printing can sleep,
		 * which inside the window would itself lose a sale */
		if (ONCE && seats == ONCE + LEAD && !__sync_lock_test_and_set(&near_printed, 1) &&
		    write(1, "HYDO-RACE-NEAR\n", 15) < 0)
			return NULL;
		s = seats;			/* check ... */
		if (s <= 0)
			break;
		if (!ONCE || s == ONCE)
			work(W);		/* ... the race window ... */
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
	ONCE = argc > 4 ? atol(argv[4]) : 0;
	LEAD = argc > 5 ? atol(argv[5]) : 10;
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

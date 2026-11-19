# The guest's clock on hvx

This note explains how an hvx guest keeps time, and why its clock fell behind in long busy runs.
It also explains why hydo boots a guest built at the fingerprint epoch 5 with its TSC as the clock
(Milestone 323). The reference for users is the section "The guest's timer tick on hvx" of
`hydo/onboard/MANIFEST.md`. The measurements are in Milestone 319 and Milestone 323 of
`docs/HISTORY.md`.

## The guest's time-stamp counter

The guest's time-stamp counter (the TSC) runs at 1.6 GHz by the guest's CPUID. hvx makes it
count the guest's work, so that a run repeats exactly. The module moves it in four places:

- by 3,200,000 (2 ms) at each tick of the PMU branch clock, every 2^20 conditional branches of
  the guest;
- by one step at each read of the TSC (`RDTSC`), which the module emulates;
- by one step at each counted exit while the guest has a timer armed;
- to the timer's deadline when the guest waits (`HLT`).

The step is a value the engine sets for each VM. It is 65,536 (41 µs) for an image built before
epoch 5.

## How the guest kernel keeps time

Linux 6.6 picks the clocksource with the highest rating and lets its watchdog compare it with
another one. In an hvx guest the watchdog compares the TSC with `refined-jiffies`, the count of
the timer ticks. At 6.25 s of uptime the TSC had moved 619 ms while the ticks counted 504 ms. The
watchdog then marked the TSC unstable ("skewed 115391808 ns (115 ms) over watchdog
'refined-jiffies' interval of 503923392 ns"). The kernel switched to `refined-jiffies` at 10.02 s.

`refined-jiffies` counts a tick as 999,848 ns. Its periodic tick on the TSC-deadline timer sets
each next tick 1,000,000 ns after the one before. It programs the deadline as the difference from
the clock's present time. The two counts part by 152 ns a tick. After t seconds of uptime the next
tick is due 1 ms + 0.152 ms × t ahead on the TSC. A busy guest has to run through that time for
each tick of its clock, so its clock slows down with its uptime.

## The four ways compared

A probe image ran fixed CPU work (500 million loop passes) after each minute of idle time, for 10
minutes of uptime. It printed how far `CLOCK_MONOTONIC` moved against the TSC during the work, and
what 100,000 reads of `clock_gettime` saw. Each way ran twice, with one fingerprint each time.

| | Today's default | `clocksource=jiffies` | `tsc=reliable` | `tsc=reliable`, step 1 µs |
|---|---|---|---|---|
| The clock after the boot | `refined-jiffies` | `jiffies` | `tsc` | `tsc` |
| The clock against the TSC during busy work, at 1 minute | 0.082 | 0.443 | 1.0001 | 1.000003 |
| The same at 10 minutes | 0.010 | 0.443 | 1.0001 | 1.000003 |
| The steps a program sees | 999,848 ns | 1 ms | 81,920 ns | 2,000 ns |
| Guest time that 100,000 reads take | 6 ms | 6 ms | 18.0 s | 0.21 s |
| ClickHouse seed 20 (M319) | `TIMEOUT workload` | `VIOLATION [converge]`, 560 s | `ERROR service start` | `VIOLATION [converge]`, 457 s |
| The pinned reproducer's stack, host seconds | 79 and 41 | 90 and 62 | 176 and 152 | 104 and 60 |

`clocksource=tsc tsc=reliable` behaved as `tsc=reliable` alone, because the TSC has the highest
rating of the guest's clocksources. The stack's verdicts were the same in every column.

## The choice

hydo boots an image built at the fingerprint epoch 5 with `tsc=reliable` and a TSC step of 1,600
(1 µs). The guest's clocks are then the TSC itself, at every uptime, and a program reads them in
steps of 2 µs. `clocksource=jiffies` keeps the ticks exact, but a busy guest's clock still runs at
0.44 of the TSC, because a tick waits for the next branch-clock tick. A program that measures a
short time with it sees 0 or 1 ms.

`tsc=reliable` with the old step makes each read of the clock cost 82 µs of guest time: one step
for the read and one for its exit. ClickHouse reads its clocks often during its start, so its start
took more than the readiness deadline of 300 s of guest time. With the step of 1 µs a read costs
2 µs, close to the 2.7 µs of a read on the Mac engine, and ClickHouse started and reached its
finding.

The fine step is used only with the PMU branch clock. On the exit-count clock alone the step is all
that moves the TSC. A guest whose only exits are its timer's would then wait 64 times longer for
each tick, so such a VM keeps the old step.

## What it changes

- Every x86_64 image built at epoch 5 gets `/etc/hydo/exact-tick` with `tsc`
  (`hydo_image::defaults`, `exact_tick`). So every hvx fingerprint of such an image moves. The
  guest kernel runs its timers in high-resolution mode. Its boot reaches the TSC clock at 2.2 s
  of guest time; before, it reached `refined-jiffies` at 10 s.
- An image built before epoch 5 has no such file and boots with the same command line, the same
  TSC step, and the same fingerprints. `guest.exact_tick: false` keeps an epoch-5 image so too.
- Milestone 319's images record `jiffies` and keep `clocksource=jiffies`.
- Clock steps (`T=`), tick jitter (`tj=`), extra ticks (`xt=`), a shorter tick period (`pp=`) and
  two vCPUs ran on an epoch-5 Redis image, each twice with one fingerprint.

## The Mac and KVM engines

Both engines read one counter for the clock and the timer, so they have no such gap:

- On the Mac engine the guest's clocksource is `arch_sys_counter`, and its timer is the arch timer
  on the same counter, in high-resolution mode. The probe's fixed work took 5 µs of guest time at
  1 minute and at 10 minutes of uptime: the engine's clock counts exits, and the work makes none.
  A read costs 2,666 ns of guest time.
- The KVM engine's guest kernel has the clocksource `hydo` (1 GHz) and a one-shot timer, both in
  nanoseconds of the engine's exit clock (`hv/kvm/linux/hydo-kvm-guest.patch`). The TSC is hidden
  in CPUID, so no watchdog compares two clocks. No KVM run measured it.

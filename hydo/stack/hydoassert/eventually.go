package hydoassert

// ---- Eventually: bounded liveness ----
//
// An Eventually property says "after an anchor point, the condition becomes true within a
// deadline". hvx makes guest time deterministic, so a deadline measured on the guest's
// monotonic clock is deterministic too: the same schedule gives the same verdict, and the
// same elapsed time, on every run.
//
//	var drained = hydoassert.Eventually("orders.drained",
//	        "every pending order is resolved within 10 s after the faults heal", 10*time.Second)
//	...
//	drained.Await(func() bool { return pending() == 0 }, time.Second, nil)   // blocks
//
// or, when the program itself sees the moment the condition becomes true:
//
//	w := drained.Begin()            // the anchor (e.g. "the faults are healed")
//	...
//	w.Done(hydoassert.D{"n": n})    // ok if within the deadline, a violation if late
//	w.Expire(nil)                   // the program gives up: a violation
//
// One evaluation record per episode, with "elapsed_ms" and "within_ms" in the details:
// ok=true when the condition became true by the deadline, ok=false when the deadline passed
// first. As for the other types, each outcome is emitted at most once per process.

import "time"

// TEventually is the wire type of a bounded-liveness property.
const TEventually Type = "eventually"

// Liveness is a declared Eventually property.
type Liveness struct {
	a      *Assertion
	within time.Duration
}

// Eventually declares a bounded-liveness property: after an anchor, cond holds within `within`.
func Eventually(id, msg string, within time.Duration) *Liveness {
	return &Liveness{a: declare(TEventually, id, msg), within: within}
}

// Watch is one episode of a Liveness property, started at its anchor.
type Watch struct {
	l     *Liveness
	start time.Time
	done  bool
}

// Begin starts an episode now (the anchor).
func (l *Liveness) Begin() *Watch { return &Watch{l: l, start: time.Now()} }

func (w *Watch) finish(ok bool, d D) bool {
	if w.done {
		return ok
	}
	w.done = true
	if d == nil {
		d = D{}
	}
	d["elapsed_ms"] = time.Since(w.start).Milliseconds()
	d["within_ms"] = w.l.within.Milliseconds()
	w.l.a.record(ok, d)
	return ok
}

// Done records that the condition became true now: ok if within the deadline, else a
// violation (it came true, but too late). Returns whether it was in time.
func (w *Watch) Done(d D) bool { return w.finish(time.Since(w.start) <= w.l.within, d) }

// Expire records that the deadline passed with the condition still false (a violation).
func (w *Watch) Expire(d D) { w.finish(false, d) }

// Await starts an episode now, polls cond every `poll` until it is true or the deadline has
// passed, records the outcome and returns it. The last poll happens at or after the deadline,
// so a condition that becomes true exactly at the deadline still counts.
func (l *Liveness) Await(cond func() bool, poll time.Duration, d D) bool {
	w := l.Begin()
	for {
		if cond() {
			return w.Done(d)
		}
		if time.Since(w.start) > l.within {
			w.Expire(d)
			return false
		}
		time.Sleep(poll)
	}
}

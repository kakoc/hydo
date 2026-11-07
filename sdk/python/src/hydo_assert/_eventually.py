"""Bounded liveness: ``eventually`` properties (the Go SDK's eventually.go).

An eventually property says "after an anchor, the condition becomes true within a deadline".
hvx makes guest time deterministic, so a deadline measured on the guest's monotonic clock is
deterministic too: the same schedule gives the same verdict and the same elapsed time.

One evaluation record per episode, with ``elapsed_ms`` and ``within_ms`` (integers) in the
details: ok=true when the condition became true by the deadline, ok=false when the deadline
passed first (or it became true too late). Each outcome is emitted at most once per process.
"""

import asyncio
import datetime
import inspect
import threading
import time

from . import _core

_NS_PER_MS = 1000000


def _now_ns():
    return time.monotonic_ns()


def _to_ns(d):
    """Seconds (int/float) or a datetime.timedelta, as integer nanoseconds."""
    if isinstance(d, datetime.timedelta):
        return (d // datetime.timedelta(microseconds=1)) * 1000
    return int(round(d * 1000000000))


class Liveness(object):
    """A declared eventually property (create it with eventually())."""

    __slots__ = ("_assertion", "within_ns")

    def __init__(self, id, msg, within):
        self._assertion = _core.declare(_core.EVENTUALLY, id, msg)
        self.within_ns = _to_ns(within)

    @property
    def id(self):
        return self._assertion.id

    def begin(self):
        """Start an episode now (the anchor)."""
        return Watch(self)

    def await_cond(self, cond, poll, details=None):
        """Start an episode now, call ``cond()`` every ``poll`` seconds until it is true or
        the deadline has passed, record the outcome and return it. Blocks the thread. The last
        check happens at or after the deadline, so a condition that becomes true exactly at
        the deadline still counts."""
        w = Watch(self)
        poll_s = _to_ns(poll) / 1e9
        while True:
            if cond():
                return w.done(details)
            if _now_ns() - w.start_ns > self.within_ns:
                w.expire(details)
                return False
            time.sleep(poll_s)

    async def await_async(self, cond, poll, details=None):
        """As await_cond, for asyncio code: ``cond`` may be a plain function or return an
        awaitable (an ``async def``), and the wait between checks is ``asyncio.sleep``."""
        w = Watch(self)
        poll_s = _to_ns(poll) / 1e9
        while True:
            r = cond()
            if inspect.isawaitable(r):
                r = await r
            if r:
                return w.done(details)
            if _now_ns() - w.start_ns > self.within_ns:
                w.expire(details)
                return False
            await asyncio.sleep(poll_s)


class Watch(object):
    """One episode of a Liveness property, started at its anchor."""

    __slots__ = ("liveness", "start_ns", "_done", "_lock")

    def __init__(self, liveness):
        self.liveness = liveness
        self.start_ns = _now_ns()
        self._done = False
        self._lock = threading.Lock()

    def _finish(self, ok, elapsed_ns, details):
        with self._lock:
            if self._done:
                return ok
            self._done = True
        d = _core.details_dict(details)
        d["elapsed_ms"] = elapsed_ns // _NS_PER_MS
        d["within_ms"] = self.liveness.within_ns // _NS_PER_MS
        self.liveness._assertion._record(ok, d)
        return ok

    def done(self, details=None):
        """The condition became true now: recorded ok if within the deadline, else a violation
        (true, but too late). Returns whether it was in time. Only the first done/expire of an
        episode is recorded."""
        el = _now_ns() - self.start_ns
        return self._finish(el <= self.liveness.within_ns, el, details)

    def expire(self, details=None):
        """The deadline passed with the condition still false (a violation)."""
        self._finish(False, _now_ns() - self.start_ns, details)


def eventually(id, msg, within):
    """Declare a bounded-liveness property: after an anchor, the condition holds within
    ``within`` (seconds, or a datetime.timedelta)."""
    return Liveness(id, msg, within)

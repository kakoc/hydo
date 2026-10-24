"""Assertions, the catalog and the lifecycle line (the Go SDK's hydoassert.go)."""

import os
import threading

from . import _json, _sink

ALWAYS = "always"
ALWAYS_OR_UNREACHABLE = "always_or_unreachable"
SOMETIMES = "sometimes"
REACHABLE = "reachable"
UNREACHABLE = "unreachable"
EVENTUALLY = "eventually"

# One process-wide lock, as Go's `mu`: held while the catalog grows, while a record is
# deduplicated, formatted and written, and while the BUGGIFY configuration is loaded. It is
# never held across a blocking call or an `await`. Reentrant, so an assertion evaluated from a
# signal handler that interrupted a record cannot deadlock.
_mu = threading.RLock()
_catalog = []


def _after_fork():
    global _mu
    _mu = threading.RLock()


class Assertion(object):
    """One declared property. Create it with always(), sometimes(), ... (it joins the catalog)."""

    __slots__ = ("type", "id", "msg", "_seen")

    def __init__(self, t, id, msg):
        self.type = t
        self.id = id
        self.msg = msg
        self._seen = [False, False]  # outcome already emitted by this process: [false, true]

    def __repr__(self):
        return "Assertion(%r, %r, %r)" % (self.type, self.id, self.msg)

    def _record(self, ok, details):
        with _mu:
            i = 1 if ok else 0
            if self._seen[i]:
                return
            self._seen[i] = True
            _emit_eval(self.type, self.id, ok, self.msg, details)

    def check(self, cond, details=None):
        """Evaluate an always / always_or_unreachable / sometimes property. For sometimes only a
        true outcome is recorded. Returns bool(cond), so it can be used inline."""
        ok = bool(cond)
        if self.type == SOMETIMES and not ok:
            return ok
        self._record(ok, details)
        return ok

    def hit(self, details=None):
        """Mark a reachable location (recorded ok=true) or an unreachable one (recorded
        ok=false: reaching it is a violation) as reached."""
        self._record(self.type != UNREACHABLE, details)

    def cmp(self, left, op, right, details=None):
        """Evaluate ``left <op> right`` (op one of < <= == != >= >) as check(), with left, op
        and right added to the details. An unknown op evaluates to false."""
        try:
            if op == "<":
                ok = left < right
            elif op == "<=":
                ok = left <= right
            elif op == "==":
                ok = left == right
            elif op == "!=":
                ok = left != right
            elif op == ">=":
                ok = left >= right
            elif op == ">":
                ok = left > right
            else:
                ok = False
        except TypeError:
            ok = False
        d = details_dict(details)
        d["left"], d["op"], d["right"] = left, op, right
        return self.check(ok, d)

    # Named comparison helpers (the same records as cmp with that op).
    def greater_than(self, left, right, details=None):
        return self.cmp(left, ">", right, details)

    def greater_than_or_equal_to(self, left, right, details=None):
        return self.cmp(left, ">=", right, details)

    def less_than(self, left, right, details=None):
        return self.cmp(left, "<", right, details)

    def less_than_or_equal_to(self, left, right, details=None):
        return self.cmp(left, "<=", right, details)


def details_dict(details):
    """A fresh dict of ``details`` (None -> {}; a mapping is copied, so the caller's object is
    never changed; any other value -> {"value": it})."""
    if details is None:
        return {}
    if isinstance(details, _json.Mapping):
        return dict(details)
    return {"value": details}


def declare(t, id, msg):
    a = Assertion(t, id, msg)
    with _mu:
        _catalog.append(a)
    return a


def always(id, msg):
    """Declare a property that must hold whenever it is evaluated (and be evaluated at least once)."""
    return declare(ALWAYS, id, msg)


def always_or_unreachable(id, msg):
    """Declare a property that must hold whenever evaluated; never being evaluated is fine."""
    return declare(ALWAYS_OR_UNREACHABLE, id, msg)


def sometimes(id, msg):
    """Declare a property that must be true at least once across an exploration."""
    return declare(SOMETIMES, id, msg)


def reachable(id, msg):
    """Declare a code location that must be reached at least once."""
    return declare(REACHABLE, id, msg)


def unreachable(id, msg):
    """Declare a code location that must never be reached."""
    return declare(UNREACHABLE, id, msg)


def _emit_eval(t, id, ok, msg, details):
    if details is not None and not isinstance(details, _json.Mapping):
        details = {"value": details}
    try:
        line = (
            'HYDO-ASSERT {"t":' + _json.quote(t)
            + ',"id":' + _json.quote(id)
            + ',"ok":' + ("true" if ok else "false")
            + ',"msg":' + _json.quote(msg)
            + ((',"details":' + _json.encode(details)) if details else "")
            + "}"
        )
    except _json.Unencodable:
        return  # as Go: json.Marshal fails, the record is not written
    _sink.write_line(line)


def catalog():
    """Write one HYDO-ASSERT-DECL line per declared assertion, in declaration order, so
    properties that are never evaluated can still be reported. Call it once, early, from the
    process that should carry the catalog into the console."""
    with _mu:
        for a in _catalog:
            _sink.write_line(
                'HYDO-ASSERT-DECL {"t":' + _json.quote(a.type)
                + ',"id":' + _json.quote(a.id)
                + ',"msg":' + _json.quote(a.msg) + "}"
            )


def setup_complete():
    """Write the lifecycle line HYDO-SETUP-COMPLETE: the system is provisioned and ready,
    faults may begin, and this is a good snapshot/branch point."""
    _sink.write_line("HYDO-SETUP-COMPLETE")


def emit_line(tag, value):
    """Write one line ``<tag> <json>`` through the same sink with one write (an extension point
    for other record kinds; the Rust SDK has the same call). Returns False if ``value`` holds a
    NaN or an infinity (nothing is written)."""
    try:
        line = tag + " " + _json.encode(value)
    except _json.Unencodable:
        return False
    with _mu:
        _sink.write_line(line)
    return True


def assertions():
    """The declared assertions, in declaration order (a copy)."""
    with _mu:
        return list(_catalog)


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork)
    os.register_at_fork(after_in_child=_sink._after_fork)

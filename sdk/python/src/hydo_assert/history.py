"""Operation histories for hydo's host-side checkers (hvx/HISTORIES.md).

A workload client records every operation it performs -- the invocation before the request,
the completion after the reply -- and the host checks the whole history after the run
(linearizability of a register / CAS object per key, Elle-style list-append). The lines are byte
for byte those of the Go recorder (hvx/stack/hydohist)::

    HYDO-HIST <history> <process> <invoke|ok|fail|info> <time-ns> <f> <arg>...

Use::

    from hydo_assert import history as hh

    kv = hh.History("kv")
    op = kv.invoke(3, "write", "k1", hh.enc(v))     # process 3, before the request
    try:
        rev = put("k1", v)
        op.ok("k1", hh.ver(v, rev))                 # the version the store assigned, if any
    except Rejected:
        op.fail()                                   # definitely had no effect
    except Exception:
        op.info()                                   # a timeout: unknown outcome
    op = kv.invoke(3, "read", "k1")
    op.ok("k1", hh.value_or_nil(get("k1")))         # None = absent

One record = one line = one ``os.write`` to ``/dev/console``; ``HYDO_HIST_OUT=<path>|stderr|off``
overrides it (the assertion records keep ``HYDO_ASSERT_OUT``). A path is opened
``O_WRONLY|O_APPEND|O_CREAT|O_NOCTTY`` (mode 0644), once, at the first record; if it cannot be
opened the records go to standard error. The host orders operations by the position of their
records, so record the invocation before sending and the completion after receiving. A process
has at most one operation in flight: give each concurrent client (thread, task) its own process
number. The time field is CLOCK_MONOTONIC in ns (``time.monotonic_ns()``, deterministic under
hydo); the checkers do not order by it.

Text: tokens are written as UTF-8. A ``str`` is encoded with the ``surrogateescape`` rule, so a
string that came from bytes that way (``os.fsdecode``, ``sys.argv``, ``os.environ``, or the result
of ``enc`` on bytes) gives back its original bytes; any other lone surrogate is written as U+FFFD.
``bytes`` are accepted wherever a ``str`` is and are taken as they are, so invalid UTF-8 can be
recorded exactly as Go records it.

This module is not imported by ``hydo_assert`` itself: importing the assertion SDK writes and
opens nothing new.
"""

import operator
import os
import threading
import time

__all__ = [
    "NIL",
    "History",
    "Op",
    "enc",
    "ver",
    "value_or_nil",
    "list_value",
    "append_mop",
    "read_mop",
    "read_result_mop",
]

#: The value of an absent key (register model); the empty list is ``[]``.
NIL = "nil"

_INT64_MIN = -(1 << 63)
_INT64_MAX = (1 << 63) - 1


def _utf8(s):
    """The bytes of a str: UTF-8 with surrogateescape; other lone surrogates become U+FFFD."""
    try:
        return s.encode("utf-8", "surrogateescape")
    except UnicodeEncodeError:
        out = bytearray()
        for ch in s:
            o = ord(ch)
            if 0xDC80 <= o <= 0xDCFF:
                out.append(o - 0xDC00)
            elif 0xD800 <= o <= 0xDFFF:
                out += b"\xef\xbf\xbd"
            else:
                out += ch.encode("utf-8")
        return bytes(out)


def _int(x, what):
    """An int64 in decimal, as Go's strconv.FormatInt writes it (bool is refused)."""
    try:
        if isinstance(x, bool):
            raise TypeError
        x = operator.index(x)
    except TypeError:
        raise TypeError("hydo_assert.history: %s must be an int, got %s" % (what, type(x).__name__)) from None
    if not _INT64_MIN <= x <= _INT64_MAX:
        raise ValueError("hydo_assert.history: %s %d is outside int64" % (what, x))
    return str(int(x))


def _token(x, what="argument"):
    """A record argument: a str (one token already), bytes (taken as they are) or an int."""
    if isinstance(x, str):
        return x
    if isinstance(x, (bytes, bytearray, memoryview)):
        return bytes(x).decode("utf-8", "surrogateescape")
    return _int(x, what)


# Bytes 0x00-0x7f: the byte itself, or %XX for '%', '@', whitespace and control characters
# (<= ' '), DEL and the list / txn separators ',[]:' (Go's Enc). Bytes >= 0x80 stay.
_TABLE = []
for _c in range(256):
    if _c == 0x25 or _c == 0x40 or _c <= 0x20 or _c == 0x7F or _c in b",[]:":
        _TABLE.append(("%%%02X" % _c).encode("ascii"))
    else:
        _TABLE.append(bytes((_c,)))
del _c


def enc(s):
    """One token from a value, as Go's ``hydohist.Enc`` makes it from the value's bytes.

    '%', '@', whitespace, control characters, DEL and ``,[]:`` become ``%XX``; other bytes
    (all of non-ASCII UTF-8, and invalid bytes given as ``bytes``) stay. ``""`` is ``"%"`` and
    ``"nil"`` is ``"%6Eil"`` (the token ``nil`` means absent). ``s`` is a str, bytes or an int.
    """
    if isinstance(s, str):
        b = _utf8(s)
    elif isinstance(s, (bytes, bytearray, memoryview)):
        b = bytes(s)
    else:
        b = _int(s, "enc value").encode("ascii")
    if b == b"":
        return "%"
    if b == b"nil":
        return "%6Eil"
    t = _TABLE
    return b"".join([t[c] for c in b]).decode("utf-8", "surrogateescape")


def ver(v, version):
    """Value ``v`` (encoded) with the version the store assigned it (an int64): ``"v@83"``."""
    return enc(v) + "@" + _int(version, "version")


def value_or_nil(v):
    """``enc(v)``, or ``NIL`` when ``v`` is None (Go's ``ValOrNil(v, ok)`` with ok = v is not None)."""
    return NIL if v is None else enc(v)


def list_value(*elems):
    """A list value ``"[e1,e2,...]"`` (list-append reads), each element encoded.

    Go and Rust call it ``List`` / ``list``; here a function named ``list`` would hide the
    builtin for every ``from hydo_assert.history import *`` user.
    """
    return "[" + ",".join([enc(e) for e in elems]) + "]"


def append_mop(k, e):
    """The list-append txn micro-op ``"a:<k>:<e>"``."""
    return "a:" + enc(k) + ":" + enc(e)


def read_mop(k):
    """The list-append txn micro-op ``"r:<k>"`` (invocation)."""
    return "r:" + enc(k)


def read_result_mop(k, *l):
    """The list-append txn micro-op ``"r:<k>:[...]"`` (completion)."""
    return "r:" + enc(k) + ":" + list_value(*l)


# ---------------------------------------------------------------------------------------------
# Sink: opened once, at the first record.

_lock = threading.Lock()
_ready = False
_off = False
_fd = 2


def _init():
    global _ready, _off, _fd
    p = os.environ.get("HYDO_HIST_OUT", "")
    _off, _fd = False, 2
    if p == "off":
        _off = True
    elif p != "stderr":
        if p == "":
            p = "/dev/console"
        try:
            _fd = os.open(p, os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOCTTY", 0), 0o644)
        except OSError:
            _fd = 2
    _ready = True


def _write(name, proc, typ, f, args):
    if not _ready:
        with _lock:
            if not _ready:
                _init()
    if _off:
        return
    parts = [name, proc, typ, None, f]
    for a in args:
        parts.append(a if a != "" else "%")  # the empty string (enc("") gives it too)
    with _lock:
        # The time is read under the lock, so the times of the records on one console never
        # decrease (Go reads it just before taking its lock; the format is the same).
        parts[3] = str(time.monotonic_ns())
        data = _utf8("HYDO-HIST " + " ".join(parts) + "\n")
        try:
            while data:  # one write(); a short write (never on the console) is continued
                n = os.write(_fd, data)
                if n <= 0:
                    break
                data = data[n:]
        except OSError:
            pass  # like Go's f.Write: a failed console write is ignored


def _reset():
    """Forget the sink (tests). The next record re-reads HYDO_HIST_OUT."""
    global _ready, _off, _fd
    with _lock:
        if _ready and not _off and _fd > 2:
            try:
                os.close(_fd)
            except OSError:
                pass
        _ready, _off, _fd = False, False, 2


def _after_fork():
    # The child inherits the descriptor; a lock held by another thread at the fork is not.
    global _lock
    _lock = threading.Lock()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork)


class History(object):
    """One named history (a check's ``history:``; ``[A-Za-z0-9_.-]+``)."""

    __slots__ = ("name",)

    def __init__(self, name):
        self.name = str(name)

    def invoke(self, proc, f, *args):
        """Record the invocation of ``f(args...)`` by process ``proc`` and return the operation.

        ``proc`` is an int or a token without spaces. ``args`` are tokens: encode free-form
        values with ``enc`` (``ver``, ``list_value`` and ``NIL`` help); ints are written in
        decimal, bytes as they are.
        """
        op = Op(self, _token(proc, "process"), str(f), tuple(_token(a) for a in args))
        _write(self.name, op.proc, "invoke", op.f, op.args)
        return op

    def record(self, proc, typ, f, *args):
        """Write one raw record (for programs that keep their own bookkeeping)."""
        _write(self.name, _token(proc, "process"), str(typ), str(f), [_token(a) for a in args])

    def __repr__(self):
        return "History(%r)" % (self.name,)


class Op(object):
    """An invoked operation waiting for its completion."""

    __slots__ = ("history", "proc", "f", "args")

    def __init__(self, history, proc, f, args):
        self.history = history
        self.proc = proc
        self.f = f
        self.args = args

    def ok(self, *args):
        """A successful completion: the results in the invocation's layout (a read lists
        ``<key> <value>`` pairs, a txn its micro-ops with read results); with no args the
        invocation's are repeated."""
        _write(self.history.name, self.proc, "ok", self.f, [_token(a) for a in args] if args else self.args)

    def fail(self):
        """The operation definitely had no effect."""
        _write(self.history.name, self.proc, "fail", self.f, self.args)

    def info(self):
        """The outcome is unknown (it may take effect at any later time, or never)."""
        _write(self.history.name, self.proc, "info", self.f, self.args)

    def __repr__(self):
        return "Op(%r, %r, %r, %r)" % (self.history.name, self.proc, self.f, self.args)

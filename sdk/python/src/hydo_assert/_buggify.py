"""BUGGIFY: code-level fault points (FoundationDB style), as in the Go SDK (Milestone 56).

A fault point is a named place where a real dependency can misbehave. The program asks it::

    LOST_REPLY = hydo_assert.buggify("kv.put.lost_reply", "the write lands but the reply is lost")
    ...
    if LOST_REPLY.fire():
        return  # do the work, drop the reply

Activation is two-level, as in FoundationDB (P_BUGGIFIED_SECTION_ACTIVATED = 0.25,
P_BUGGIFIED_SECTION_FIRES = 0.25): for a run each point is ENABLED with probability
enable/1000; an enabled point FIRES on its k-th evaluation with probability fire/1000. Both
decisions are pure functions of the seed, the point's id and k (splitmix64 and FNV-1a, the
same arithmetic as the Go SDK), so a run stays a pure function of its schedule.

Off unless configured: ``$HYDO_BUGGIFY``, or else the file ``$HYDO_BUGGIFY_FILE`` (default
``/tmp/hydo-buggify``), holds ``<seed-hex>[,<enable-permille>[,<fire-permille>]]``. Under hydo
the schedule token ``B=<...>`` is written to that file by the guest init. The configuration is
read once, at the first evaluation of any point, which also writes::

    HYDO-BUGGIFY {"seed":"...","enable_pm":250,"fire_pm":250,"enabled":["id",...]}

Each point is declared as a sometimes property ``buggify.<id>`` ("fault point fired: <msg>"),
recorded at its first firing with ``{"hit": k}``.
"""

import os
import re

from . import _core, _json, _sink

_M = (1 << 64) - 1
_GOLDEN = 0x9E3779B97F4A7C15

_points = []
_loaded = False
_on = False
_seed = 0
_enable = 250
_fire = 250

# Go's unicode.IsSpace set (strings.TrimSpace), which differs from str.strip()'s.
_GO_SPACE = (
    "\t\n\x0b\x0c\r \x85\xa0        "
    "        　"
)
_HEXRE = re.compile(r"[0-9A-Fa-f]+\Z")
_DECRE = re.compile(r"[0-9]+\Z")


def _mix(z):  # splitmix64 finalizer
    z = (z + _GOLDEN) & _M
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & _M
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & _M
    return z ^ (z >> 31)


def _key(id):  # FNV-1a over the id's UTF-8 bytes
    h = 14695981039346656037
    for b in bytearray(id.encode("utf-8", "surrogateescape")):
        h ^= b
        h = (h * 1099511628211) & _M
    return h


def _parse_u64(s, rx, base):
    """strconv.ParseUint(s, base, 64): no sign, no prefix, no spaces, <= 2^64-1."""
    if not rx.match(s):
        return None
    v = int(s, base)
    return v if v <= _M else None


class Point(object):
    """One declared fault point (create it with buggify())."""

    __slots__ = ("id", "_assertion", "_hits")

    def __init__(self, id, msg):
        self.id = id
        self._assertion = _core.declare(_core.SOMETIMES, "buggify." + id, "fault point fired: " + msg)
        self._hits = 0

    def __repr__(self):
        return "Point(%r)" % (self.id,)

    def _enabled(self):
        return _mix(_seed ^ _key(self.id)) % 1000 < _enable

    def fire(self, details=None):
        """Whether the point misbehaves on this evaluation. Always False when BUGGIFY is not
        configured; then it has no other effect either."""
        with _core._mu:
            if not _loaded:
                _load()
            if not _on or not self._enabled():
                return False
            self._hits += 1
            k = self._hits
            fire = _mix((_mix(_seed ^ _key(self.id)) + k * _GOLDEN) & _M) % 1000 < _fire
        if fire:
            d = _core.details_dict(details)
            d["hit"] = k
            self._assertion._record(True, d)
        return fire


def buggify(id, msg):
    """Declare a fault point (it also joins the catalog as sometimes ``buggify.<id>``)."""
    p = Point(id, msg)
    with _core._mu:
        _points.append(p)
    return p


def _load():
    """Read "<seed-hex>[,<enable-permille>[,<fire-permille>]]" (called under _core._mu)."""
    global _loaded, _on, _seed, _enable, _fire
    _loaded = True
    cfg = os.environ.get("HYDO_BUGGIFY", "")
    if cfg == "":
        f = os.environ.get("HYDO_BUGGIFY_FILE", "") or "/tmp/hydo-buggify"
        try:
            with open(f, "rb") as fh:
                cfg = fh.read().decode("utf-8", "surrogateescape")
        except (OSError, IOError):
            pass
    parts = cfg.strip(_GO_SPACE).split(",")
    seed = _parse_u64(parts[0], _HEXRE, 16)
    if seed is None:
        return  # no configuration: every point stays off
    _on, _seed = True, seed
    if len(parts) > 1:
        v = _parse_u64(parts[1], _DECRE, 10)
        if v is not None:
            _enable = v
    if len(parts) > 2:
        v = _parse_u64(parts[2], _DECRE, 10)
        if v is not None:
            _fire = v
    enabled = [p.id for p in _points if p._enabled()]
    _sink.write_line(
        'HYDO-BUGGIFY {"seed":' + _json.quote(format(_seed, "x"))
        + ',"enable_pm":' + str(_enable)
        + ',"fire_pm":' + str(_fire)
        + ',"enabled":' + _json.encode(enabled) + "}"
    )


def _reset():
    """Forget the loaded configuration and every point's evaluation count (tests)."""
    global _loaded, _on, _seed, _enable, _fire
    with _core._mu:
        _loaded, _on, _seed, _enable, _fire = False, False, 0, 250, 250
        for p in _points:
            p._hits = 0

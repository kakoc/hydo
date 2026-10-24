"""A JSON encoder that writes exactly what Go's encoding/json writes for the same data.

The Go SDK (hvx/stack/hydoassert) encodes its records with json.Marshal. To be byte-identical:

- object keys are sorted by their UTF-8 bytes (Go sorts map keys that way);
- strings are escaped as Go escapes them: ``"`` ``\\`` ``\\b`` ``\\f`` ``\\n`` ``\\r`` ``\\t``,
  other control characters as ``\\u00xx``, the HTML characters ``<`` ``>`` ``&`` as
  ``\\u003c`` ``\\u003e`` ``\\u0026``, U+2028 and U+2029 as ``\\u2028`` ``\\u2029``; everything
  else (all other non-ASCII text) is written as UTF-8. A lone surrogate (Python's stand-in for
  an undecodable byte, e.g. from ``surrogateescape``) is written as ``\\ufffd``, as Go up to
  1.26 writes each invalid UTF-8 byte (Go 1.27's encoder writes a raw U+FFFD instead; the
  guest images are built with the pinned Go 1.26.5);
- floats use Go's format: the shortest digits that round-trip, plain notation when
  1e-6 <= |f| < 1e21 (or f == 0), else exponent notation ``1e+21`` / ``1.5e-7``;
- a NaN or an infinity cannot be encoded (Go's Marshal fails): Unencodable is raised and the
  caller drops the whole record.

Python-only value types (Go has no equivalent, so no Go program can produce them): a tuple is
an array; ``bytes``/``bytearray`` are base64 strings (as Go encodes ``[]byte``); a set or
frozenset is an array sorted by the encoded elements (so the output does not depend on hash
order); any other object is encoded as the string ``str(obj)``. Mapping keys that are ints are
written in decimal (as Go writes integer map keys); other non-string keys as ``str(key)``.
"""

import base64
import decimal
import math

try:  # Python 3.3+
    from collections.abc import Mapping
except ImportError:  # pragma: no cover
    from collections import Mapping


class Unencodable(ValueError):
    """A value Go's json.Marshal would refuse (NaN, +Inf, -Inf)."""


_HEX = "0123456789abcdef"


def _build_escape_table():
    t = {}
    for c in range(0x20):
        t[c] = "\\u00" + _HEX[c >> 4] + _HEX[c & 0xF]
    t[0x08] = "\\b"
    t[0x0C] = "\\f"
    t[0x0A] = "\\n"
    t[0x0D] = "\\r"
    t[0x09] = "\\t"
    t[0x22] = '\\"'
    t[0x5C] = "\\\\"
    t[0x3C] = "\\u003c"
    t[0x3E] = "\\u003e"
    t[0x26] = "\\u0026"
    t[0x2028] = "\\u2028"
    t[0x2029] = "\\u2029"
    for c in range(0xD800, 0xE000):
        t[c] = "\\ufffd"
    return t


_ESCAPES = _build_escape_table()


def quote(s):
    """The JSON string literal of ``s``, as Go writes it (with HTML escaping)."""
    return '"' + s.translate(_ESCAPES) + '"'


def _format_float(f):
    if math.isnan(f) or math.isinf(f):
        raise Unencodable("NaN or infinity")
    if f == 0.0:
        return "-0" if math.copysign(1.0, f) < 0 else "0"
    sign, digits, exp = decimal.Decimal(repr(f)).as_tuple()
    digits = list(digits)
    while len(digits) > 1 and digits[-1] == 0:  # repr may carry a trailing ".0"
        digits.pop()
        exp += 1
    ds = "".join(str(d) for d in digits)
    n = len(ds)
    e10 = exp + n - 1  # the value is d.ddd x 10^e10
    neg = "-" if sign else ""
    a = abs(f)
    if a < 1e-6 or a >= 1e21:
        mant = ds[0] + ("." + ds[1:] if n > 1 else "")
        if e10 < 0:
            return neg + mant + "e-" + str(-e10)  # Go cleans e-07 to e-7
        return neg + mant + "e+" + ("%02d" % e10)
    if exp >= 0:
        return neg + ds + "0" * exp
    if e10 >= 0:
        return neg + ds[: e10 + 1] + "." + ds[e10 + 1:]
    return neg + "0." + "0" * (-e10 - 1) + ds


def _key_str(k):
    if isinstance(k, str):
        return k
    if isinstance(k, bool):
        return "true" if k else "false"
    if isinstance(k, int):
        return str(int(k))
    return str(k)


def _utf8(s):
    """The bytes Go would compare: a surrogate from surrogateescape is its original byte."""
    try:
        return s.encode("utf-8", "surrogateescape")
    except UnicodeEncodeError:
        return s.encode("utf-8", "surrogatepass")


def encode(v):
    """The JSON text of ``v``, as Go's encoding/json writes the equivalent Go value."""
    out = []
    _enc(v, out)
    return "".join(out)


def _enc(v, out):
    if v is None:
        out.append("null")
    elif v is True:
        out.append("true")
    elif v is False:
        out.append("false")
    elif isinstance(v, str):
        out.append(quote(v))
    elif isinstance(v, bool):  # a bool subclass (numpy-like); after the singletons
        out.append("true" if v else "false")
    elif isinstance(v, int):
        out.append(str(int(v)))
    elif isinstance(v, float):
        out.append(_format_float(float(v)))
    elif isinstance(v, Mapping):
        items = sorted(((_key_str(k), x) for k, x in v.items()), key=lambda kv: _utf8(kv[0]))
        out.append("{")
        first = True
        for k, x in items:
            if not first:
                out.append(",")
            first = False
            out.append(quote(k))
            out.append(":")
            _enc(x, out)
        out.append("}")
    elif isinstance(v, (list, tuple)):
        out.append("[")
        for i, x in enumerate(v):
            if i:
                out.append(",")
            _enc(x, out)
        out.append("]")
    elif isinstance(v, (bytes, bytearray, memoryview)):
        out.append('"' + base64.b64encode(bytes(v)).decode("ascii") + '"')
    elif isinstance(v, (set, frozenset)):
        elems = sorted((encode(x) for x in v), key=_utf8)
        out.append("[" + ",".join(elems) + "]")
    else:
        out.append(quote(str(v)))

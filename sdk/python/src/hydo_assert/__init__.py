"""hydo-assert: the Python assertion SDK of hydo, the deterministic hypervisor.

A Python program running under hydo uses this package to say what must be true and what should
happen at least once. It writes the same console records as the Rust, Go and JavaScript SDKs,
byte for byte, with the same deduplication rules; the wire format and verdict rules are in
hydo/ASSERTIONS.md. Standard library only; Python 3.8+.

Declare properties at module level, so the whole catalog is known once the module is imported::

    import hydo_assert as ha

    ACKED   = ha.always("kv.acked_durable", "every acknowledged write reads back with its value")
    REPLAY  = ha.sometimes("kv.recovered", "the server replayed a non-empty log")
    TORN    = ha.reachable("kv.torn_tail", "the log ended in a partial line")
    BAD     = ha.unreachable("kv.bad_command", "the server got a command it does not know")
    SIZE    = ha.always("kv.log_size", "the log stays under its cap")
    BACK    = ha.eventually("kv.back", "the server answers within 5 s", 5.0)
    LOST    = ha.buggify("kv.put.lost_reply", "the write lands but the reply is lost")

    ha.catalog()                                 # once, early: the HYDO-ASSERT-DECL lines
    ACKED.check(got == want, {"key": k, "got": got})
    SIZE.cmp(n, "<=", cap)                       # op: < <= == != >= >
    TORN.hit()
    if LOST.fire():
        return
    BACK.await_cond(ping, poll=0.1)              # or: await BACK.await_async(aping, 0.1)
    ha.setup_complete()                          # HYDO-SETUP-COMPLETE

Records go to /dev/ttyS0; ``HYDO_ASSERT_OUT=<path>|stderr|off|logging`` overrides that.
"""

from ._core import (
    Assertion,
    always,
    always_or_unreachable,
    catalog,
    reachable,
    setup_complete,
    sometimes,
    unreachable,
)
from ._buggify import Point, buggify
from ._eventually import Liveness, Watch, eventually

#: The details type: any mapping (a plain dict). The Go SDK calls it D.
D = dict

__version__ = "0.2.0"

__all__ = [
    "Assertion",
    "D",
    "Liveness",
    "Point",
    "Watch",
    "always",
    "always_or_unreachable",
    "buggify",
    "catalog",
    "eventually",
    "reachable",
    "setup_complete",
    "sometimes",
    "unreachable",
]

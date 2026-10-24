"""Where records go: the guest console, opened once.

``HYDO_ASSERT_OUT`` (read once, at the first record) selects the target, as in the Go SDK:

- unset or empty: ``/dev/ttyS0`` (the hvx guest console);
- a path: that existing file, opened ``O_WRONLY|O_APPEND|O_NOCTTY`` (never created);
- ``stderr``: standard error (file descriptor 2), i.e. plain logging outside hydo;
- ``off``: nowhere;
- ``logging`` (Python only): each record line is logged at INFO on the logger ``hydo_assert``.

If the file cannot be opened the records go to standard error. Every record is written with
one ``write()`` call (a loop only if the kernel accepts part of it), so it is never interleaved
with other output on the console.
"""

import os
import threading

_lock = threading.Lock()
_ready = False
_mode = "fd"  # "fd", "off" or "logging"
_fd = 2


def _init():
    global _ready, _mode, _fd
    p = os.environ.get("HYDO_ASSERT_OUT", "")
    if p == "off":
        _mode = "off"
    elif p == "stderr":
        _mode, _fd = "fd", 2
    elif p == "logging":
        _mode = "logging"
    else:
        if p == "":
            p = "/dev/ttyS0"
        try:
            _fd = os.open(p, os.O_WRONLY | os.O_APPEND | getattr(os, "O_NOCTTY", 0))
            _mode = "fd"
        except OSError:
            _mode, _fd = "fd", 2
    _ready = True


def _ensure():
    if not _ready:
        with _lock:
            if not _ready:
                _init()


def enabled():
    """False when HYDO_ASSERT_OUT=off (records are dropped)."""
    _ensure()
    return _mode != "off"


def write_line(line):
    """Write one record line (``line`` without the trailing newline) to the sink."""
    _ensure()
    if _mode == "off":
        return
    if _mode == "logging":
        import logging

        logging.getLogger("hydo_assert").info("%s", line)
        return
    data = (line + "\n").encode("utf-8", "surrogatepass")
    try:
        while data:
            n = os.write(_fd, data)
            if n <= 0:
                break
            data = data[n:]
    except OSError:
        pass  # like Go's f.Write: a failed console write is not the program's problem


def _reset():
    """Forget the sink (tests). The next record re-reads HYDO_ASSERT_OUT."""
    global _ready, _mode, _fd
    with _lock:
        if _ready and _mode == "fd" and _fd > 2:
            try:
                os.close(_fd)
            except OSError:
                pass
        _ready, _mode, _fd = False, "fd", 2


def _after_fork():
    global _lock
    _lock = threading.Lock()

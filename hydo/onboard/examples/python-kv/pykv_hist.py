"""pykv's client with an operation history (Milestone 96): the Python SDK's recorder
(hydo_assert.history) writes one HYDO-HIST line per invocation and per completion, and the
host checks the history kv as one register per key (hvx/HISTORIES.md; manifest
hydo-mac-hist.yaml, `checks:`). The requests are pykv.py's own client (pykv.request).

    pykv_hist.py put <port> <process> <key> <value>    one PUT; exit 0 when acknowledged
    pykv_hist.py readall <port> <process> <n>          GET k:0 .. k:<n-1> (the final read-back)
"""
import sys

import pykv
from hydo_assert import history

KV = history.History("kv")


def main(argv):
    cmd, port, proc = argv[1], int(argv[2]), int(argv[3])
    if cmd == "put":
        k, v = argv[4], argv[5]
        op = KV.invoke(proc, "write", history.enc(k), history.enc(v))
        if pykv.request(port, "PUT %s %s" % (k, v)) == "OK":
            op.ok()
            return 0
        op.info()       # no answer: the PUT may have been applied, or not
        return 1
    if cmd == "readall":
        bad = 0
        for i in range(int(argv[4])):
            k = "k:%d" % i
            op = KV.invoke(proc, "read", history.enc(k))
            r = pykv.request(port, "GET " + k)
            if r is not None and r.startswith("VAL "):
                op.ok(history.enc(k), history.enc(r[4:]))
            elif r == "NONE":
                op.ok(history.enc(k), history.NIL)
            else:
                op.info()
                bad = 1
        return bad
    print("usage: pykv_hist.py put|readall ...", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))

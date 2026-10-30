"""pykv: a tiny key/value server and its client, written with hydo's Python SDK (hydo_assert).

    pykv.py serve <dir> <port>        asyncio server; append-only log <dir>/log, one "k\\tv" per line
    pykv.py put <port> <key> <value>  exit 0 = acknowledged ("OK"), 1 = no acknowledgement (2 s deadline)
    pykv.py ping <port>               exit 0 = the server answers
    pykv.py check <port> <acked-file> the oracle: every acknowledged PUT reads back with its value
                                      (exit 0 holds, 1 violated, 2 server unreachable)

The server fsyncs the log before it acknowledges, unless /tmp/pykv-fsync says "never" (the
manifest's knob S). Its properties are the program's own HYDO-ASSERT records; the manifest adds
none. Two BUGGIFY fault points (a lost reply after the write, a slow write) fire when the
schedule carries B=<seed> (the manifest's knob B writes /tmp/hydo-buggify).
"""

import asyncio
import os
import socket
import sys

import hydo_assert as ha

# ---- the catalog (declaration order) ----
ACKED = ha.always("kv.acked_durable", "every acknowledged PUT reads back with its value")
LOG_PARSES = ha.always_or_unreachable("kv.log_parses", "every complete log line parses on replay")
LOG_SIZE = ha.always("kv.log_size", "the log holds at most 10000 entries")
RECOVERED = ha.sometimes("kv.recovered", "the server replayed a non-empty log at start")
TORN = ha.reachable("kv.torn_tail", "the log ended in a partial line (a write cut by a crash)")
NO_REPLY = ha.sometimes("kv.put_no_reply", "a PUT got no acknowledgement")
BAD = ha.unreachable("kv.bad_command", "the server got a command it does not know")
BACK = ha.eventually("kv.back", "the server answers within 5 s of the oracle's start", 5.0)
BG_LOST = ha.buggify("kv.put.lost_reply", "the PUT is logged but its reply is lost")
BG_SLOW = ha.buggify("kv.put.slow", "the PUT waits 3 s before it is logged")

MAX_ENTRIES = 10000


# ---- server ----

class Store(object):
    def __init__(self, d):
        self.path = os.path.join(d, "log")
        self.data = {}
        self.entries = 0
        self.replay()
        self.f = open(self.path, "ab", buffering=0)

    def replay(self):
        try:
            with open(self.path, "rb") as f:
                raw = f.read()
        except FileNotFoundError:
            raw = b""
        complete, _, tail = raw.rpartition(b"\n")
        for line in complete.split(b"\n") if complete else []:
            k, sep, v = line.partition(b"\t")
            LOG_PARSES.check(bool(sep) and bool(k), {"line": line.decode("utf-8", "replace")})
            if sep:
                self.data[k.decode()] = v.decode()
                self.entries += 1
        if tail:  # a partial last line: drop it (it was never acknowledged)
            TORN.hit({"bytes": len(tail)})
            with open(self.path, "r+b") as f:
                f.truncate(len(raw) - len(tail))
        RECOVERED.check(self.entries > 0, {"entries": self.entries})
        print("pykv: replayed %d entries" % self.entries, flush=True)

    def put(self, k, v):
        self.f.write(("%s\t%s\n" % (k, v)).encode())
        try:
            with open("/tmp/pykv-fsync") as fh:
                mode = fh.read().strip()
        except OSError:
            mode = "always"
        if mode != "never":
            os.fsync(self.f.fileno())
        self.data[k] = v
        self.entries += 1
        LOG_SIZE.less_than_or_equal_to(self.entries, MAX_ENTRIES)


async def serve(d, port):
    st = Store(d)

    async def handle(reader, writer):
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                parts = line.decode().rstrip("\n").split(" ")
                if parts[0] == "PUT" and len(parts) == 3:
                    if BG_SLOW.fire({"key": parts[1]}):
                        await asyncio.sleep(3)
                    st.put(parts[1], parts[2])
                    if BG_LOST.fire({"key": parts[1]}):
                        break  # logged and fsynced, but the client sees the connection close
                    writer.write(b"OK\n")
                elif parts[0] == "GET" and len(parts) == 2:
                    v = st.data.get(parts[1])
                    writer.write(("VAL %s\n" % v).encode() if v is not None else b"NONE\n")
                elif parts[0] == "PING":
                    writer.write(b"PONG\n")
                else:
                    BAD.hit({"command": parts[0]})
                    writer.write(b"ERR\n")
                await writer.drain()
        finally:
            writer.close()

    server = await asyncio.start_server(handle, "127.0.0.1", port)
    print("pykv: listening on %d" % port, flush=True)
    ha.setup_complete()
    async with server:
        await server.serve_forever()


# ---- client ----

def request(port, line, timeout=2.0):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout) as s:
            s.sendall((line + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                b = s.recv(4096)
                if not b:
                    return None
                buf += b
            return buf.decode().rstrip("\n")
    except OSError:
        return None


def main(argv):
    cmd = argv[1]
    if cmd == "serve":
        asyncio.run(serve(argv[2], int(argv[3])))
        return 0
    port = int(argv[2])
    if cmd == "ping":
        return 0 if request(port, "PING", 1.0) == "PONG" else 1
    if cmd == "put":
        r = request(port, "PUT %s %s" % (argv[3], argv[4]))
        if r == "OK":
            return 0
        NO_REPLY.check(True, {"key": argv[3], "reply": r})
        return 1
    if cmd == "check":
        ha.catalog()  # the oracle runs in every run's console, so it carries the catalog
        if not BACK.await_cond(lambda: request(port, "PING", 1.0) == "PONG", 0.2):
            print("server unreachable")
            return 2
        with open(argv[3]) as f:
            acked = [l.strip() for l in f if l.strip()]
        for i in acked:
            got = request(port, "GET k:%s" % i)
            if not ACKED.check(got == "VAL v%s" % i, {"key": "k:" + i, "got": got}):
                print("key k:%s lost (got %r)" % (i, got))
                return 1
        print("acked=%d all present" % len(acked))
        return 0
    print("usage: pykv.py serve|put|ping|check ...", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))

"""A payment app for Saleor's transaction webhooks, for the native runs (Python standard library).

It answers as hydo's fake `payments` does in hydo/hydo.yaml: every charge and refund succeeds,
and each idempotency key gets its first answer again. PAYAPP_DELAY (seconds, default 0) delays
every answer, as a payment provider that is slow to answer. Every request is printed, so the log
is the provider's own record of what it was asked to do.

    python3 payapp.py 9001
"""

import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DELAY = float(os.environ.get("PAYAPP_DELAY", "0"))
# PAYAPP_LOSE=<route>: the first request of that route takes effect, but its answer is a 500 (as
# hydo's H=payments.<route>@...:status=500,effect=yes)
LOSE = os.environ.get("PAYAPP_LOSE", "")
lost = set()
RESULT = {"initialize": "CHARGE_SUCCESS", "charge": "CHARGE_SUCCESS",
          "refund": "REFUND_SUCCESS", "cancel": "CANCEL_SUCCESS"}
seen = {}
lock = threading.Lock()
seq = [0]


class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        route = self.path.strip("/")
        key = body.get("idempotencyKey", "")
        amount = (body.get("action") or {}).get("amount")
        with lock:
            replay = (route, key) in seen
            if not replay:
                seq[0] += 1
                seen[(route, key)] = {"pspReference": "%s_%d" % (route[:2], seq[0]),
                                      "result": RESULT.get(route, "CHARGE_SUCCESS"), "amount": amount}
            answer = seen[(route, key)]
        print("PAYAPP %s key=%s amount=%s replay=%s at=%.3f" % (route, key, amount, replay, time.time()),
              flush=True)
        time.sleep(DELAY)
        status = 200
        with lock:
            if route == LOSE and route not in lost:
                lost.add(route)
                status = 500
        out = json.dumps(answer if status == 200 else {"error": "lost"}).encode()
        print("PAYAPP answer %s %d" % (route, status), flush=True)
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *a):
        pass


ThreadingHTTPServer(("0.0.0.0", int(sys.argv[1])), H).serve_forever()

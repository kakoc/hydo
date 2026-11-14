"""A small Lago API client for the scripts of this folder (standard library only).

  python3 lago.py seed <tag> <billing_time> <subscription_at|-> <customer>...
      the billable metric api_calls (the sum of `units`), the monthly plan in arrears (10.00 USD
      and 0.10 USD per unit), and per customer one subscription sub-<customer> with one event of
      12 units; billing_time is calendar or anniversary, and `-` leaves subscription_at to Lago
  python3 lago.py terminate <external_subscription_id>     DELETE /api/v1/subscriptions/<id>
  python3 lago.py status <external_subscription_id>        the subscription, terminated or not
  python3 lago.py invoices <customer>...                   the customer's invoices with their fees

LAGO_API (http://127.0.0.1:13000) and LAGO_KEY (the organization's key of compose.yaml) change
where the requests go.
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

API = os.environ.get("LAGO_API", "http://127.0.0.1:13000")
KEY = os.environ.get("LAGO_KEY", "hydo-test-api-key")


def req(method, path, body=None, timeout=1200):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(API + path, data=data, method=method)
    r.add_header("Authorization", "Bearer " + KEY)
    r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read() or b"{}")
        except ValueError:
            return e.code, {}


def must(method, path, body=None):
    code, data = req(method, path, body)
    if code != 200 and not (code == 422 and "value_already_exist" in json.dumps(data)):
        sys.exit(f"{method} {path}: HTTP {code} {data}")
    return data


def seed(tag, billing_time, subscription_at, customers):
    must("POST", "/api/v1/billable_metrics", {"billable_metric": {
        "name": "API calls", "code": "api_calls", "aggregation_type": "sum_agg", "field_name": "units"}})
    _, metrics = req("GET", "/api/v1/billable_metrics?per_page=100")
    mid = {m["code"]: m["lago_id"] for m in metrics["billable_metrics"]}["api_calls"]
    must("POST", "/api/v1/plans", {"plan": {
        "name": "Monthly", "code": "monthly", "interval": "monthly", "amount_cents": 1000,
        "amount_currency": "USD", "pay_in_advance": False,
        "charges": [{"billable_metric_id": mid, "charge_model": "standard", "pay_in_advance": False,
                     "properties": {"amount": "0.10"}}]}})
    for c in customers:
        must("POST", "/api/v1/customers", {"customer": {"external_id": c, "name": c, "currency": "USD"}})
        sub = {"external_customer_id": c, "plan_code": "monthly", "external_id": "sub-" + c,
               "billing_time": billing_time}
        if subscription_at != "-":
            sub["subscription_at"] = subscription_at
        must("POST", "/api/v1/subscriptions", {"subscription": sub})
        must("POST", "/api/v1/events", {"event": {
            "transaction_id": f"{tag}-{c}-1", "external_subscription_id": "sub-" + c,
            "code": "api_calls", "properties": {"units": 12}}})
    print("seeded", " ".join(customers))


def terminate(sub):
    t0 = time.time()
    code, data = req("DELETE", f"/api/v1/subscriptions/{sub}")
    s = data.get("subscription", {})
    err = "" if s else f" {data.get('code') or data.get('error') or ''}"
    print(f"DELETE {sub}: HTTP {code} after {time.time() - t0:.1f} s, status={s.get('status')} "
          f"terminated_at={s.get('terminated_at')}{err}")


def status(sub):
    code, data = req("GET", f"/api/v1/subscriptions/{sub}?status=terminated")
    s = data.get("subscription", {})
    print(f"GET {sub}: HTTP {code}, status={s.get('status')} terminated_at={s.get('terminated_at')}")


def invoices(customers):
    for c in customers:
        _, data = req("GET", f"/api/v1/invoices?external_customer_id={c}&per_page=100")
        for inv in sorted(data.get("invoices", []), key=lambda i: i["created_at"]):
            _, full = req("GET", f"/api/v1/invoices/{inv['lago_id']}")
            full = full["invoice"]
            fees = []
            for f in full.get("fees", []):
                fees.append(f"{f['item']['type']}:{f['item'].get('code')} units={f['units']} "
                            f"{f.get('from_date') or f.get('properties', {}).get('from_datetime')}.."
                            f"{f.get('to_date') or f.get('properties', {}).get('to_datetime')}")
            reasons = [s.get("invoicing_reason") for s in full.get("invoice_subscriptions", [])]
            print(f"{c} {full['status']} {full['invoice_type']} created={full['created_at']} "
                  f"total={full['total_amount_cents']} reason={reasons or ''}")
            for f in fees:
                print("    " + f)
        if not data.get("invoices"):
            print(f"{c}: no invoice")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd, *args = sys.argv[1:]
    if cmd == "seed" and len(args) >= 4:
        seed(args[0], args[1], args[2], args[3:])
    elif cmd == "terminate" and len(args) == 1:
        terminate(args[0])
    elif cmd == "status" and len(args) == 1:
        status(args[0])
    elif cmd == "invoices" and args:
        invoices(args)
    else:
        sys.exit(__doc__)

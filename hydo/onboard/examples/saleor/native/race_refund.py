"""Send N refund requests for one transaction at the same moment (threads behind a barrier).

    python3 race_refund.py <order number> <app|gift> <amount> [N]

Run in the API container (stack.sh exec); it uses the test client's token and queries.
"""

import sys
import threading

sys.path.insert(0, "/hydo/workload")
import helper_client as c  # noqa: E402

number, which, amount = sys.argv[1], sys.argv[2], sys.argv[3]
n = int(sys.argv[4]) if len(sys.argv) > 4 else 2
d = c.q("query($n:[String!]){ orders(first:1, filter:{numbers:$n}){ edges{ node{ id "
        "transactions{ id pspReference chargedAmount{amount} } } } } }", {"n": [number]})
trs = [t for t in d["orders"]["edges"][0]["node"]["transactions"]
       if t["chargedAmount"]["amount"] > 0 and (t["pspReference"] or "").startswith("in_") == (which == "app")]
tid = trs[0]["id"]
print("transaction", tid, "charged", trs[0]["chargedAmount"]["amount"], "refunds", n, "x", amount)
barrier = threading.Barrier(n)
out = []


def one(i):
    barrier.wait()
    r = c.post("mutation($id:ID!,$a:PositiveDecimal!){transactionRequestAction(id:$id,actionType:REFUND,"
               "amount:$a){errors{field code}}}", {"id": tid, "a": amount}, c.staff(), 40)
    out.append((i, r["transactionRequestAction"]["errors"]))


ts = [threading.Thread(target=one, args=(i,)) for i in range(n)]
for t in ts:
    t.start()
for t in ts:
    t.join()
print("answers", sorted(out))
d = c.q("query($id:ID!){ transaction(id:$id){ chargedAmount{amount} refundedAmount{amount} } }", {"id": tid})
print("after: charged", d["transaction"]["chargedAmount"]["amount"],
      "refunded", d["transaction"]["refundedAmount"]["amount"])

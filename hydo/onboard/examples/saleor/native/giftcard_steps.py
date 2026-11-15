"""Single steps for probe-giftcard-detach.sh, with the test client's queries (run in the API container).

    python3 giftcard_steps.py prepare <op> <card index>   a checkout paid in full by one gift card
    python3 giftcard_steps.py prepare-unpaid <op>          a checkout with one line, not paid
    python3 giftcard_steps.py authorize <checkout id> <card index>
    python3 giftcard_steps.py finish <checkout id>     checkoutComplete
"""

import sys
import time

sys.path.insert(0, "/hydo/workload")
import helper_client as c  # noqa: E402


def new_checkout(sku):
    d = c.q('query($s:[String!]){ productVariants(first:5, filter:{sku:$s}, channel:"default-channel")'
            '{ edges{ node{ id } } } }', {"s": [sku]})
    vid = d["productVariants"]["edges"][0]["node"]["id"]
    d = c.q("mutation($i:CheckoutCreateInput!){checkoutCreate(input:$i){checkout{id shippingMethods{id name}} "
            "errors{field code}}}", {"i": {"channel": "default-channel", "email": "same@example.com",
                                          "lines": [{"variantId": vid, "quantity": 1}],
                                          "shippingAddress": c.ADDRESS, "billingAddress": c.ADDRESS}})["checkoutCreate"]
    c.errors_of(d)
    cid = d["checkout"]["id"]
    m = [x["id"] for x in d["checkout"]["shippingMethods"] if x["name"] == "Standard"][0]
    c.errors_of(c.q("mutation($id:ID!,$m:ID!){checkoutDeliveryMethodUpdate(id:$id,deliveryMethodId:$m)"
                    "{errors{field code}}}", {"id": cid, "m": m})["checkoutDeliveryMethodUpdate"])
    return cid


def authorize(cid, card, key):
    total = c.q("query($id:ID!){checkout(id:$id){totalPrice{gross{amount}}}}", {"id": cid})
    amount = total["checkout"]["totalPrice"]["gross"]["amount"]
    code = c.gift_card_codes()[int(card)]
    d = c.q("mutation($id:ID!,$g:String!,$a:PositiveDecimal!,$k:String!,$data:JSON){transactionInitialize("
            "id:$id,paymentGateway:{id:$g,data:$data},amount:$a,idempotencyKey:$k){transactionEvent{type message} "
            "errors{field code}}}", {"id": cid, "g": c.GIFT_CARD_GATEWAY, "a": amount, "k": key,
                                     "data": {"code": code}})["transactionInitialize"]
    c.errors_of(d)
    print("authorized", code[-4:], amount, d["transactionEvent"]["type"], flush=True)


cmd = sys.argv[1]
if cmd == "prepare":
    cid = new_checkout("sku-2")
    authorize(cid, sys.argv[3], "%s-gift-%d" % (sys.argv[2], time.time_ns()))
    print("CHECKOUT", cid)
elif cmd == "prepare-unpaid":
    cid = new_checkout("sku-1")
    print("CHECKOUT", cid)
elif cmd == "authorize":
    authorize(sys.argv[2], sys.argv[3], "b-gift-%d" % time.time_ns())
elif cmd == "finish":
    d = c.q("mutation($id:ID!){checkoutComplete(id:$id){order{number total{gross{amount}} chargeStatus "
            "authorizeStatus} errors{field code message}}}", {"id": sys.argv[2]}, timeout=60)["checkoutComplete"]
    if d["errors"]:
        print("complete refused", d["errors"])
    else:
        o = d["order"]
        print("ORDER", o["number"], o["total"]["gross"]["amount"], o["chargeStatus"], o["authorizeStatus"])

"""The test's Saleor client (Python standard library only; run with the API image's Python).

    python3 helper_client.py setup <payment app url>   the shop: channel, stock, vouchers,
                                                       gift cards, the payment app
    python3 helper_client.py checkout <op> <seed> [gift]   one purchase, from a new checkout to an order
    python3 helper_client.py complete <op> <checkout id>  checkoutComplete alone (CLIENT_NO_COMPLETE=1
                                                       makes `checkout` stop before it)
    python3 helper_client.py refund <op> <order> <amount> [app|gift]
    python3 helper_client.py report                    one line per checkout token with an order

It talks to the API at $SALEOR_URL (default http://api:8000/graphql/) as the staff user that
first_setup creates. Exit status: 0 acknowledged, 1 refused (Saleor answered with an error, so
the operation had no effect), 3 unknown (no answer, or an answer that does not say).
Every line it prints starts with the operation's name, so a log reads as a history.
"""

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request

URL = os.environ.get("SALEOR_URL", "http://api:8000/graphql/")
EMAIL = "staff@example.com"
PASSWORD = "hydo-staff-password"
APP_ID = "hydo.payments"
TOKEN_FILE = os.environ.get("SALEOR_TOKEN_FILE", "/app/media/hydo-client-token")
CLIENT_PERMISSIONS = ("MANAGE_CHECKOUTS HANDLE_CHECKOUTS HANDLE_PAYMENTS MANAGE_CHANNELS MANAGE_DISCOUNTS "
                      "MANAGE_GIFT_CARD MANAGE_ORDERS MANAGE_PRODUCTS MANAGE_PRODUCT_TYPES_AND_ATTRIBUTES "
                      "MANAGE_SHIPPING MANAGE_SETTINGS MANAGE_USERS").split()
GIFT_CARD_GATEWAY = "saleor.io.gift-card-payment-gateway"
DEADLINE = float(os.environ.get("CLIENT_DEADLINE", "50"))
START = time.monotonic()

ADDRESS = {
    "firstName": "Ada",
    "lastName": "Lovelace",
    "streetAddress1": "1 Main Street",
    "city": "NEW YORK",
    "postalCode": "10001",
    "country": "US",
    "countryArea": "NY",
}


class Unknown(Exception):
    """No answer, or an answer that does not tell whether the operation took effect."""


class Refused(Exception):
    """Saleor answered with an error: the operation did not take effect."""


class Bad(Exception):
    """A query that Saleor cannot parse: an error of this client, never retried."""


def log(*a):
    print(*a, flush=True)


def post(query, variables=None, token=None, timeout=20):
    body = json.dumps({"query": query, "variables": variables or {}}).encode()
    req = urllib.request.Request(URL, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        text = e.read().decode(errors="replace")[:400]
        if e.code == 400:
            # Saleor refuses a query it cannot parse or validate: the client's own error
            raise Bad("http 400: %s" % text) from e
        raise Unknown("http %d: %s" % (e.code, text[:200])) from e
    except (urllib.error.URLError, OSError, ValueError) as e:
        raise Unknown(str(e)) from e
    if data.get("errors"):
        # a GraphQL error (a crash inside the mutation, a database error) says nothing certain
        raise Unknown("graphql: %s" % json.dumps(data["errors"])[:300])
    return data["data"]


def call(query, variables=None, token=None, timeout=20):
    """post() with retries of an unknown outcome until the deadline (the caller makes the
    request safe to repeat)."""
    last = None
    while time.monotonic() - START < DEADLINE:
        try:
            return post(query, variables, token, timeout)
        except Unknown as e:
            last = e
            time.sleep(1)
    raise Unknown("deadline: %s" % last)


def errors_of(result):
    errs = result.get("errors") or []
    if errs:
        raise Refused(";".join("%s:%s" % (e.get("field"), e.get("code")) for e in errs))


_token = None


def staff():
    """The client's token: the app token that setup saved (Saleor delays repeated staff logins,
    LOGIN_ATTEMPT_DELAYED, so the commands do not log in each time)."""
    global _token
    if _token is None:
        try:
            with open(TOKEN_FILE) as f:
                _token = f.read().strip()
        except OSError:
            d = call(
                "mutation($e:String!,$p:String!){tokenCreate(email:$e,password:$p){token errors{field code}}}",
                {"e": EMAIL, "p": PASSWORD},
            )["tokenCreate"]
            if not d["token"]:
                raise Unknown("no staff token: %s" % d["errors"])
            _token = d["token"]
    return _token


def q(query, variables=None, timeout=20):
    return call(query, variables, staff(), timeout)


# ---------------------------------------------------------------- setup

INIT_QUERY = (
    "subscription { event { ... on TransactionInitializeSession { idempotencyKey "
    "ref: sourceObject { ... on Checkout { id } } action { amount currency actionType } } } }"
)
REFUND_QUERY = (
    "subscription { event { ... on TransactionRefundRequested { idempotencyKey "
    "ref: transaction { id pspReference } action { amount currency actionType } } } }"
)
CHARGE_QUERY = (
    "subscription { event { ... on TransactionChargeRequested { idempotencyKey "
    "ref: transaction { id pspReference } action { amount currency actionType } } } }"
)
CANCEL_QUERY = (
    "subscription { event { ... on TransactionCancelationRequested { idempotencyKey "
    "ref: transaction { id pspReference } action { amount currency actionType } } } }"
)

VARIANTS = [("sku-1", "10.00", 20), ("sku-2", "25.00", 12), ("sku-3", "40.00", 6)]
GIFT_CARDS = 4
GIFT_CARD_BALANCE = "60.00"


def setup(pay_url):
    ch = q('{ channel(slug:"default-channel"){ id } warehouses(first:1){ edges{ node{ id } } } '
           'shippingZones(first:1){ edges{ node{ id } } } }')
    channel = ch["channel"]["id"]
    wh = ch["warehouses"]["edges"][0]["node"]["id"]
    zone = ch["shippingZones"]["edges"][0]["node"]["id"]
    # the channel: transactions pay the orders, no unpaid orders, orders confirmed at once
    errors_of(q("""mutation($id:ID!,$w:ID!,$z:ID!){channelUpdate(id:$id,input:{
        addWarehouses:[$w], addShippingZones:[$z],
        orderSettings:{markAsPaidStrategy:TRANSACTION_FLOW, allowUnpaidOrders:false,
                       automaticallyConfirmAllNewOrders:true},
        paymentSettings:{defaultTransactionFlowStrategy:CHARGE}}){errors{field code}}}""",
                {"id": channel, "w": wh, "z": zone})["channelUpdate"])
    errors_of(q('mutation($z:ID!,$w:ID!,$c:ID!){shippingZoneUpdate(id:$z,input:{countries:["US"],'
                'addWarehouses:[$w],addChannels:[$c]}){errors{field code}}}',
                {"z": zone, "w": wh, "c": channel})["shippingZoneUpdate"])
    d = q('mutation($z:ID!){shippingPriceCreate(input:{name:"Standard",type:PRICE,shippingZone:$z})'
          '{shippingMethod{id} errors{field code}}}', {"z": zone})["shippingPriceCreate"]
    errors_of(d)
    ship = d["shippingMethod"]["id"]
    errors_of(q('mutation($s:ID!,$c:ID!){shippingMethodChannelListingUpdate(id:$s,input:{addChannels:'
                '[{channelId:$c,price:5.00}]}){errors{field code}}}',
                {"s": ship, "c": channel})["shippingMethodChannelListingUpdate"])
    d = q('mutation{productTypeCreate(input:{name:"Goods",kind:NORMAL,isShippingRequired:true})'
          '{productType{id} errors{field code}}}')["productTypeCreate"]
    errors_of(d)
    ptype = d["productType"]["id"]
    d = q('mutation{categoryCreate(input:{name:"All"}){category{id} errors{field code}}}')["categoryCreate"]
    errors_of(d)
    cat = d["category"]["id"]
    d = q('mutation($t:ID!,$c:ID!){productCreate(input:{name:"Mug",productType:$t,category:$c})'
          '{product{id} errors{field code}}}', {"t": ptype, "c": cat})["productCreate"]
    errors_of(d)
    prod = d["product"]["id"]
    errors_of(q('mutation($p:ID!,$c:ID!){productChannelListingUpdate(id:$p,input:{updateChannels:[{'
                'channelId:$c,isPublished:true,visibleInListings:true,isAvailableForPurchase:true}]})'
                '{errors{field code}}}', {"p": prod, "c": channel})["productChannelListingUpdate"])
    for sku, price, qty in VARIANTS:
        d = q('mutation($p:ID!,$s:String!,$w:ID!,$n:Int!){productVariantCreate(input:{product:$p,'
              'sku:$s,name:$s,attributes:[],trackInventory:true,stocks:[{warehouse:$w,quantity:$n}]})'
              '{productVariant{id} errors{field code}}}',
              {"p": prod, "s": sku, "w": wh, "n": qty})["productVariantCreate"]
        errors_of(d)
        v = d["productVariant"]["id"]
        errors_of(q('mutation($v:ID!,$c:ID!,$pr:PositiveDecimal!){productVariantChannelListingUpdate('
                    'id:$v,input:[{channelId:$c,price:$pr}]){errors{field code}}}',
                    {"v": v, "c": channel, "pr": price})["productVariantChannelListingUpdate"])
    # vouchers: a code with a usage limit, single-use codes, and one use per customer (which
    # ends at $VOUCHER_END when it is set)
    end = ',endDate:"%s"' % os.environ["VOUCHER_END"] if os.environ.get("VOUCHER_END") else ""
    vouchers = [
        ('{name:"Five off",type:ENTIRE_ORDER,discountValueType:FIXED,addCodes:["SAVE5"],usageLimit:3}', "5.00"),
        ('{name:"Single use",type:ENTIRE_ORDER,discountValueType:FIXED,singleUse:true,'
         'addCodes:["ONE-A","ONE-B","ONE-C"]}', "8.00"),
        ('{name:"Once per customer",type:ENTIRE_ORDER,discountValueType:PERCENTAGE,'
         'applyOncePerCustomer:true,addCodes:["WELCOME"]%s}' % end, "10"),
    ]
    for inp, value in vouchers:
        d = q("mutation{voucherCreate(input:%s){voucher{id} errors{field code}}}" % inp)["voucherCreate"]
        errors_of(d)
        errors_of(q('mutation($v:ID!,$c:ID!,$d:PositiveDecimal!){voucherChannelListingUpdate(id:$v,'
                    'input:{addChannels:[{channelId:$c,discountValue:$d}]}){errors{field code}}}',
                    {"v": d["voucher"]["id"], "c": channel, "d": value})["voucherChannelListingUpdate"])
    for i in range(GIFT_CARDS):
        d = q('mutation($a:PositiveDecimal!){giftCardCreate(input:{balance:{currency:"USD",amount:$a},'
              'isActive:true,channel:"default-channel"}){giftCard{id code} errors{field code}}}',
              {"a": GIFT_CARD_BALANCE})["giftCardCreate"]
        errors_of(d)
    # the payment app: Saleor calls it synchronously for every payment action
    d = q('mutation($i:String!){appCreate(input:{name:"payments",identifier:$i,permissions:[HANDLE_PAYMENTS]})'
          '{app{id} errors{field code}}}', {"i": APP_ID})["appCreate"]
    errors_of(d)
    app = d["app"]["id"]
    for name, event, query in [
        ("initialize", "TRANSACTION_INITIALIZE_SESSION", INIT_QUERY),
        ("charge", "TRANSACTION_CHARGE_REQUESTED", CHARGE_QUERY),
        ("refund", "TRANSACTION_REFUND_REQUESTED", REFUND_QUERY),
        ("cancel", "TRANSACTION_CANCELATION_REQUESTED", CANCEL_QUERY),
    ]:
        errors_of(q('mutation($a:ID!,$n:String!,$u:String!,$q:String!){webhookCreate(input:{app:$a,name:$n,'
                    'targetUrl:$u,syncEvents:[%s],isActive:true,query:$q}){errors{field code message}}}' % event,
                    {"a": app, "n": name, "u": pay_url.rstrip("/") + "/" + name, "q": query})["webhookCreate"])
    # the client's own app: one token for every later command
    d = q("mutation($p:[PermissionEnum!]){appCreate(input:{name:\"client\",identifier:\"hydo.client\","
          "permissions:$p}){authToken errors{field code}}}", {"p": CLIENT_PERMISSIONS})["appCreate"]
    errors_of(d)
    tmp = TOKEN_FILE + ".tmp"
    with open(tmp, "w") as f:
        f.write(d["authToken"])
        f.flush()
        os.fsync(f.fileno())
    os.rename(tmp, TOKEN_FILE)
    log("setup ok", channel, ship)


# ---------------------------------------------------------------- one purchase

def pick(seed, n, k):
    """the k-th independent choice in range(n) from one seed"""
    return (seed // (7 ** k)) % n


def gift_card_codes():
    d = q("{ giftCards(first:20, sortBy:{field:CREATED_AT, direction:ASC}){ edges{ node{ code } } } }")
    return [e["node"]["code"] for e in d["giftCards"]["edges"]]


def checkout(op, seed, mode="any"):
    """mode any: the seed decides whether a gift card pays; gift: a gift card pays half or all."""
    seed = int(seed)
    lines = [{"variantId": None, "sku": VARIANTS[pick(seed, 3, 0)][0], "quantity": 1 + pick(seed, 2, 1)}]
    if pick(seed, 3, 2) == 0:
        sku2 = VARIANTS[(pick(seed, 3, 0) + 1) % 3][0]
        lines.append({"sku": sku2, "quantity": 1})
    customer = "c%d@example.com" % pick(seed, 4, 3)
    promo = ["", "", "SAVE5", "ONE-A", "ONE-B", "ONE-C", "WELCOME"][pick(seed, 7, 4)]
    use_gift = pick(seed, 3, 5)   # 0: no gift card, 1: a gift card pays half, 2: a gift card pays all
    if mode == "gift":
        use_gift = 1 + pick(seed, 2, 5)
    gift_index = pick(seed, GIFT_CARDS, 6)

    d = q('query($s:[String!]){ productVariants(first:5, filter:{sku:$s}, channel:"default-channel")'
          '{ edges{ node{ id sku } } } }', {"s": [l["sku"] for l in lines]})
    ids = {e["node"]["sku"]: e["node"]["id"] for e in d["productVariants"]["edges"]}
    inp = {
        "channel": "default-channel",
        "email": customer,
        "lines": [{"variantId": ids[l["sku"]], "quantity": l["quantity"]} for l in lines],
        "shippingAddress": ADDRESS,
        "billingAddress": ADDRESS,
    }
    d = q("mutation($i:CheckoutCreateInput!){checkoutCreate(input:$i){checkout{id token "
          "shippingMethods{id name}} errors{field code}}}", {"i": inp})["checkoutCreate"]
    errors_of(d)
    co = d["checkout"]
    cid, token = co["id"], co["token"]
    log(op, "checkout", token, "lines", ",".join("%s*%d" % (l["sku"], l["quantity"]) for l in lines),
        "customer", customer, "promo", promo or "-", "gift", use_gift)
    # From here on the checkout exists: an error is the checkout's, and the operation's outcome is
    # decided by checkoutComplete alone.
    print("TOKEN", token, flush=True)
    d = q("mutation($id:ID!,$m:ID!){checkoutDeliveryMethodUpdate(id:$id,deliveryMethodId:$m)"
          "{errors{field code}}}",
          {"id": cid, "m": [m["id"] for m in co["shippingMethods"] if m["name"] == "Standard"][0]})
    errors_of(d["checkoutDeliveryMethodUpdate"])
    if promo:
        d = q("mutation($id:ID!,$c:String!){checkoutAddPromoCode(id:$id,promoCode:$c){errors{field code}}}",
              {"id": cid, "c": promo})["checkoutAddPromoCode"]
        if d["errors"]:
            log(op, "promo refused", promo, d["errors"])
    total = q("query($id:ID!){checkout(id:$id){totalPrice{gross{amount}}}}", {"id": cid})["checkout"]
    amount = total["totalPrice"]["gross"]["amount"]
    log(op, "total", amount)
    paid = 0.0
    if use_gift:
        code = gift_card_codes()[gift_index]
        part = round(amount / 2, 2) if use_gift == 1 else amount
        d = q("mutation($id:ID!,$g:String!,$a:PositiveDecimal!,$k:String!,$data:JSON){transactionInitialize("
              "id:$id,paymentGateway:{id:$g,data:$data},amount:$a,idempotencyKey:$k){transaction{id "
              "authorizedAmount{amount} chargedAmount{amount}} transactionEvent{type message} "
              "errors{field code}}}",
              {"id": cid, "g": GIFT_CARD_GATEWAY, "a": part, "k": op + "-gift",
               "data": {"code": code}})["transactionInitialize"]
        errors_of(d)
        ev = d["transactionEvent"]
        log(op, "gift card", code[-4:], part, ev["type"])
        if ev["type"] == "AUTHORIZATION_SUCCESS":
            paid += part
    rest = round(amount - paid, 2)
    if rest > 0:
        # A storefront retries a payment that failed without an answer, with the same key: the
        # payment app answers a repeated key with its first answer.
        for attempt in range(3):
            d = q("mutation($id:ID!,$g:String!,$a:PositiveDecimal!,$k:String!){transactionInitialize("
                  "id:$id,paymentGateway:{id:$g},amount:$a,idempotencyKey:$k){transaction{id "
                  "chargedAmount{amount}} transactionEvent{type message} errors{field code}}}",
                  {"id": cid, "g": APP_ID, "a": rest, "k": op + "-pay"})["transactionInitialize"]
            errors_of(d)
            ev = d["transactionEvent"]
            log(op, "payment", rest, ev["type"], (ev.get("message") or "")[:80])
            if ev["type"] == "CHARGE_SUCCESS":
                break
    if os.environ.get("CLIENT_NO_COMPLETE"):
        print("CHECKOUT", cid, flush=True)
        return
    complete(op, cid, token)


def complete(op, cid, token="-"):
    d = q("mutation($id:ID!){checkoutComplete(id:$id){order{id number total{gross{amount}}} "
          "errors{field code message}}}", {"id": cid}, timeout=40)["checkoutComplete"]
    if d["errors"]:
        raise Refused("complete: " + ";".join("%s:%s" % (e["field"], e["code"]) for e in d["errors"]))
    o = d["order"]
    log(op, "order", o["number"], o["total"]["gross"]["amount"])
    print("ORDER", token, o["number"], o["total"]["gross"]["amount"], flush=True)


def refund(op, number, amount, which="app"):
    """Refund `amount` of the order's payment by the payment app (which=app) or by gift card
    (which=gift)."""
    d = q("query($n:[String!]){ orders(first:1, filter:{numbers:$n}){ edges{ node{ id "
          "transactions{ id pspReference chargedAmount{amount} refundedAmount{amount} } } } } }",
          {"n": [number]})["orders"]["edges"]
    if not d:
        raise Refused("no order %s" % number)
    # the payment app's references start with "in_" (its answer to the initialize call)
    trs = [t for t in d[0]["node"]["transactions"]
           if t["chargedAmount"]["amount"] > 0 and (t["pspReference"] or "").startswith("in_") == (which == "app")]
    if not trs:
        raise Refused("order %s has nothing charged by %s" % (number, which))
    t = trs[0]
    log(op, "refund", number, t["id"], amount, "of", t["chargedAmount"]["amount"])
    # A refund request is not repeated after an unknown outcome: Saleor has no key for it.
    d = post("mutation($id:ID!,$a:PositiveDecimal!){transactionRequestAction(id:$id,actionType:REFUND,"
             "amount:$a){transaction{chargedAmount{amount} refundedAmount{amount}} errors{field code}}}",
             {"id": t["id"], "a": amount}, staff(), 40)["transactionRequestAction"]
    errors_of(d)
    tr = d["transaction"]
    log(op, "refunded", tr["refundedAmount"]["amount"], "charged", tr["chargedAmount"]["amount"])


def report():
    """One line per order made from a checkout: token, number, total."""
    after = None
    while True:
        d = q("query($a:String){ orders(first:100, after:$a){ pageInfo{ hasNextPage endCursor } "
              "edges{ node{ number checkoutId total{gross{amount}} } } } }", {"a": after})["orders"]
        for e in d["edges"]:
            n = e["node"]
            token = "-"
            if n["checkoutId"]:
                token = base64.b64decode(n["checkoutId"]).decode().split(":", 1)[1]
            print(token, n["number"], n["total"]["gross"]["amount"])
        if not d["pageInfo"]["hasNextPage"]:
            return
        after = d["pageInfo"]["endCursor"]


def main():
    cmd = sys.argv[1]
    try:
        if cmd == "setup":
            setup(sys.argv[2])
        elif cmd == "checkout":
            checkout(*sys.argv[2:5])
        elif cmd == "complete":
            complete(sys.argv[2], sys.argv[3])
        elif cmd == "refund":
            refund(*sys.argv[2:6])
        elif cmd == "report":
            report()
        else:
            print("unknown command", cmd, file=sys.stderr)
            return 2
    except Refused as e:
        log(cmd, "refused", e)
        return 1
    except Unknown as e:
        log(cmd, "unknown", e)
        return 3
    except Bad as e:
        log(cmd, "client error", e)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())

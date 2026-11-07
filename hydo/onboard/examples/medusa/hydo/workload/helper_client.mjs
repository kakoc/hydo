// The test's storefront and back-office client, run with the shop image's own Node.js:
//   hydo-oci medusa node /hydo/workload/helper_client.mjs <command> [args]
// It talks to Medusa's store and admin HTTP APIs the way a storefront and an operator do.
// Every answer is printed on one line; the exit status says what the client saw:
//   0 acknowledged, 1 failed for certain (a 4xx, or Medusa refused), 3 unknown (no answer, a
//   timeout, a 5xx: the operation may or may not have happened).
//
//   checkout <seed>        one guest checkout of the scarce variant: cart, line item, address,
//                          shipping, payment collection, Stripe payment session, complete.
//                          Prints "cart=<id>" first, then "order=<id>" on success.
//   capture-refund <seed>  as the operator: pick an order (by the seed), capture its payment,
//                          refund part of it. Prints what it did.
//   sql-free reads (for checks) are done with psql, not here.
import fs from "node:fs";

const BASE = process.env.SHOP_URL || "http://127.0.0.1:9000";
const SEED_FILE = process.env.SHOP_SEED || "/data/shop/seed.json";
const T = Number(process.env.SHOP_TIMEOUT_MS || "15000");

class Unknown extends Error {}
class Failed extends Error {}

const seed = JSON.parse(fs.readFileSync(SEED_FILE, "utf8"));

async function call(method, path, body, headers = {}) {
  let res;
  try {
    res = await fetch(BASE + path, {
      method,
      headers: { "content-type": "application/json", "x-publishable-api-key": seed.publishable_key, ...headers },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(T),
    });
  } catch (e) {
    throw new Unknown(`${method} ${path}: ${e.name === "TimeoutError" ? "timeout" : e.message}`);
  }
  const text = await res.text().catch(() => "");
  if (res.status >= 500) throw new Unknown(`${method} ${path}: ${res.status} ${text.slice(0, 160)}`);
  if (res.status >= 400) throw new Failed(`${method} ${path}: ${res.status} ${text.slice(0, 160)}`);
  try { return JSON.parse(text); } catch { throw new Unknown(`${method} ${path}: not JSON`); }
}

function pause(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function checkout(n) {
  // Everything before "complete" is preparation: if it fails, no order can exist.
  let cart;
  try {
    ({ cart } = await call("POST", "/store/carts", { region_id: seed.region_id, email: `buyer${n}@shop.test` }));
    console.log(`cart=${cart.id}`);
    await call("POST", `/store/carts/${cart.id}/line-items`, { variant_id: seed.variant_id, quantity: 1 });
    const address = { first_name: "Test", last_name: `Buyer${n}`, address_1: "Street 1", city: "Copenhagen", country_code: "dk", postal_code: "1000" };
    await call("POST", `/store/carts/${cart.id}`, { shipping_address: address, billing_address: address });
    await call("POST", `/store/carts/${cart.id}/shipping-methods`, { option_id: seed.shipping_option_id });
    const { payment_collection: pc } = await call("POST", "/store/payment-collections", { cart_id: cart.id });
    await call("POST", `/store/payment-collections/${pc.id}/payment-sessions`, { provider_id: "pp_stripe_stripe" });
  } catch (e) {
    console.log(`prepare: ${e.message}`);
    return 1;
  }
  // The customer pays (the test's fake provider answers every payment intent as confirmed) and
  // the storefront completes the cart; a customer whose click got no answer clicks again.
  for (let attempt = 1; ; attempt++) {
    try {
      const r = await call("POST", `/store/carts/${cart.id}/complete`, {});
      if (r.type === "order") { console.log(`order=${r.order.id} attempt=${attempt}`); return 0; }
      console.log(`complete refused: ${JSON.stringify(r.error || r).slice(0, 200)}`);
      return 1;
    } catch (e) {
      console.log(`complete attempt ${attempt}: ${e.message}`);
      if (e instanceof Failed) return 1;
      if (attempt >= 3) return 3;
      await pause(2000);
    }
  }
}

async function adminToken() {
  const r = await call("POST", "/auth/user/emailpass", { email: "admin@shop.test", password: "admin-secret" });
  return { authorization: `Bearer ${r.token}` };
}

async function captureRefund(n) {
  let auth, payment;
  try {
    auth = await adminToken();
    const { orders } = await call("GET", "/admin/orders?limit=50&order=created_at&fields=id,payment_collections.payments.*", undefined, auth);
    if (!orders.length) { console.log("no order yet"); return 1; }
    const order = orders[n % orders.length];
    payment = order.payment_collections?.[0]?.payments?.[0];
    if (!payment) { console.log(`order=${order.id} has no payment`); return 1; }
    console.log(`order=${order.id} payment=${payment.id} amount=${payment.amount}`);
  } catch (e) {
    console.log(`prepare: ${e.message}`);
    return e instanceof Failed ? 1 : 3;
  }
  try {
    if (!payment.captured_at) {
      await call("POST", `/admin/payments/${payment.id}/capture`, {}, auth);
      console.log("captured");
    }
    const r = await call("POST", `/admin/payments/${payment.id}/refund`, { amount: 10 }, auth);
    console.log(`refunded 10 of ${r.payment?.amount ?? "?"}`);
    return 0;
  } catch (e) {
    console.log(`capture/refund: ${e.message}`);
    return e instanceof Failed ? 1 : 3;
  }
}

const [cmd, arg] = process.argv.slice(2);
const n = Number(arg || "0");
let rc;
if (cmd === "checkout") rc = await checkout(n);
else if (cmd === "capture-refund") rc = await captureRefund(n);
else { console.log(`unknown command ${cmd}`); rc = 2; }
process.exit(rc);

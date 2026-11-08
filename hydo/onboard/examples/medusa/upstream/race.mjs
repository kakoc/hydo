// Two refunds of one order at the same moment, through Medusa's admin API.
//   node race.mjs [trials]       (run inside the medusa container: run.sh does it)
// Each trial places a fresh order (one hoodie, the system payment provider), captures its
// payment, then sends two `POST /admin/payments/:id/refund {amount: 10}` requests at once, as two
// operators (or one double click) would. It then reads the order back and prints how many units
// its items hold: always 1 when the refunds are serialized, 2 when both refunds wrote the order's
// items at the same order version.
import fs from "node:fs";

const BASE = "http://127.0.0.1:9000";
const seed = JSON.parse(fs.readFileSync(process.env.SEED_OUT || "/tmp/repro-seed.json", "utf8"));
const trials = Number(process.argv[2] || "20");

async function call(method, path, body, headers = {}) {
  const res = await fetch(BASE + path, {
    method,
    headers: { "content-type": "application/json", "x-publishable-api-key": seed.publishable_key, ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (res.status >= 400) {
    const e = new Error(`${method} ${path}: ${res.status} ${text.slice(0, 200)}`);
    e.status = res.status;
    throw e;
  }
  return JSON.parse(text);
}

async function placeOrder(n) {
  const { cart } = await call("POST", "/store/carts", { region_id: seed.region_id, email: `buyer${n}@shop.test` });
  await call("POST", `/store/carts/${cart.id}/line-items`, { variant_id: seed.variant_id, quantity: 1 });
  const address = { first_name: "Test", last_name: `Buyer${n}`, address_1: "Street 1", city: "Copenhagen", country_code: "dk", postal_code: "1000" };
  await call("POST", `/store/carts/${cart.id}`, { shipping_address: address, billing_address: address });
  await call("POST", `/store/carts/${cart.id}/shipping-methods`, { option_id: seed.shipping_option_id });
  const { payment_collection: pc } = await call("POST", "/store/payment-collections", { cart_id: cart.id });
  await call("POST", `/store/payment-collections/${pc.id}/payment-sessions`, { provider_id: "pp_system_default" });
  const r = await call("POST", `/store/carts/${cart.id}/complete`, {});
  if (r.type !== "order") throw new Error(`complete: ${JSON.stringify(r).slice(0, 200)}`);
  return r.order.id;
}

const { token } = await call("POST", "/auth/user/emailpass", { email: "admin@repro.test", password: "repro-secret" });
const auth = { authorization: `Bearer ${token}` };

async function paidOrder(n) {
  const orderId = await placeOrder(n);
  const { order } = await call("GET", `/admin/orders/${orderId}?fields=id,payment_collections.payments.*`, undefined, auth);
  const payment = order.payment_collections[0].payments[0];
  await call("POST", `/admin/payments/${payment.id}/capture`, {}, auth);
  return { orderId, paymentId: payment.id };
}

const refund = (paymentId) =>
  call("POST", `/admin/payments/${paymentId}/refund`, { amount: 10 }, auth)
    .then(() => "ok", (e) => `refused: ${e.message.replace(/^.*?: /, "").slice(0, 40)}...`);

async function report(label, orderId, outcomes) {
  const { order } = await call("GET", `/admin/orders/${orderId}?fields=id,version,items.*`, undefined, auth);
  const units = order.items.reduce((s, i) => s + Number(i.quantity), 0);
  const bad = units !== 1;
  console.log(`${label}: ${orderId} refunds=[${outcomes.join(" | ")}] version=${order.version} items=${JSON.stringify(order.items.map((i) => [i.id, i.quantity]))} units=${units}${bad ? "  <-- CORRUPTED: 1 unit was ordered" : ""}`);
  return bad;
}

// control: two refunds of one order, one after the other
{
  const { orderId, paymentId } = await paidOrder(0);
  const outcomes = [await refund(paymentId), await refund(paymentId)];
  await report("control (sequential)", orderId, outcomes);
}

let corrupted = 0;
for (let t = 1; t <= trials; t++) {
  const { orderId, paymentId } = await paidOrder(t);
  const outcomes = await Promise.all([refund(paymentId), refund(paymentId)]); // started together
  if (await report(`trial ${t}`, orderId, outcomes)) corrupted++;
}
console.log(`${corrupted} of ${trials} orders corrupted by two concurrent refunds`);

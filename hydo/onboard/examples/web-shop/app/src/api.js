// The shop's API client. Every call has a time limit. A call that fails on the network, or gets
// no answer in time, is tried once more, as many hand-written clients do.
//
// The variant `buggy` (VITE_VARIANT=buggy at build time) has one planted bug here: an order is
// retried without an idempotency key, so an order whose answer was lost is placed twice.

export const BUGGY = import.meta.env.VITE_VARIANT === 'buggy';
// Milestone 393: VITE_SEARCH=1 adds a search box to the product list (App.jsx); the builds
// without it leave it out entirely.
export const SEARCH = import.meta.env.VITE_SEARCH === '1';
const TIMEOUT_MS = 3000;

export class HttpError extends Error {
  constructor(status, path) {
    super(`HTTP ${status} for ${path}`);
    this.name = 'HttpError';
    this.status = status;
  }
}

async function once(path, init) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(path, { ...init, signal: ctl.signal });
    if (!res.ok) throw new HttpError(res.status, path);
    return await res.json();
  } catch (e) {
    if (e.name === 'AbortError') throw new Error(`no answer from ${path} within ${TIMEOUT_MS / 1000} s`);
    throw e;
  } finally {
    clearTimeout(timer);
  }
}

async function call(path, init = {}) {
  try {
    return await once(path, init);
  } catch (e) {
    if (e instanceof HttpError) throw e; // the server answered: no second try
    return await once(path, init); // the network or the time limit: one more try
  }
}

function post(body, headers = {}) {
  return {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body),
  };
}

export const getProducts = () => call('/api/products');

export const search = (q) => call(`/api/search?q=${encodeURIComponent(q)}`);

export const addItem = (product) => call('/api/cart/items', post({ product }));

export function placeOrder(cart) {
  // fixed: one key per cart, so a second try of the same order is the same order
  const headers = BUGGY ? {} : { 'Idempotency-Key': `order-${cart.id}` };
  return call('/api/orders', post({ cart: cart.id, count: cart.items.length, total: cart.total }, headers));
}

export const getOrder = (id) => call(`/api/orders/${encodeURIComponent(id)}`);

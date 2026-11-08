// The test's client for Ghost's Admin API (run with the Ghost image's own Node.js:
// `hydo-oci ghost node /hydo/workload/helper_client.mjs <op>`).
//
//   key                read the Zapier integration's Admin API key from MySQL's answer on stdin
//                      and save it to /data/test/admin-key (first_seed calls it)
//   owner              set the site up with its owner and save a staff session (the operator)
//   member <n>         add the member m<n>@example.com (subscribed to the default newsletter)
//   unsubscribe <n>    take the member m<n>@example.com off every newsletter
//   audience <seed>    member or unsubscribe, chosen by the seed
//   stripe             the operator connects Stripe (API keys), which turns donations on
//   donate <n>         the reader d<n>@example.com starts a donation (a Stripe Checkout session)
//   publish <n>        write a post and publish it with a newsletter to every member
//   retry              the operator's "retry" on every failed newsletter
//
// Every operation prints one line to the console and records what the client saw: <id> ack,
// failed or unknown in $HYDO_OUTCOMES, and acknowledged publishes (p-<n>) in $HYDO_ACKED.
// Exit status: 0 acknowledged, 1 failed or unknown.
import { createHmac } from 'node:crypto';
import { appendFileSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';

// Ghost's url is https (Ghost refuses to boot with Stripe on a plain-http url); in production a
// TLS proxy sits in front and tells Ghost so with X-Forwarded-Proto, which this client sends.
const ORIGIN = 'https://ghost:2368';
const SERVER = 'http://ghost:2368';
const PROXY = { 'X-Forwarded-Proto': 'https' };
const BASE = `${SERVER}/ghost/api/admin`;
const KEYFILE = '/data/test/admin-key';
const SESSIONFILE = '/data/test/session-cookie';
const ORDERFILE = '/data/test/order.log';
const OWNER = { email: 'owner@example.com', password: 'hydo-test-owner-pw-1' };
const TIMEOUT_MS = 20000;

function say(line) {
  try { appendFileSync('/dev/console', `ghost-test: ${line}\n`); } catch { /* no console */ }
  console.log(line);
}

function record(id, outcome) {
  if (process.env.HYDO_OUTCOMES) appendFileSync(process.env.HYDO_OUTCOMES, `${id} ${outcome}\n`);
  if (outcome === 'ack' && id.startsWith('p-') && process.env.HYDO_ACKED) {
    appendFileSync(process.env.HYDO_ACKED, `${id}\n`);
  }
}

// The order the clients saw things in (unsubscribes acknowledged, publications started), one line
// each, for the check that no one who left gets a later issue. Timestamps cannot give this order:
// a clock step (T=) moves Ghost's clock back or forward.
function order(line) {
  try { appendFileSync(ORDERFILE, `${line}\n`); } catch { /* best effort */ }
}

function b64url(buf) {
  return Buffer.from(buf).toString('base64').replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_');
}

// Ghost's Admin API token: a JWT (HS256) signed with the key's hex secret, valid five minutes.
function token() {
  const [id, secret] = readFileSync(KEYFILE, 'utf8').trim().split(':');
  const now = Math.floor(Date.now() / 1000);
  const head = b64url(JSON.stringify({ alg: 'HS256', typ: 'JWT', kid: id }));
  const body = b64url(JSON.stringify({ iat: now, exp: now + 300, aud: '/admin/' }));
  const sig = b64url(createHmac('sha256', Buffer.from(secret, 'hex')).update(`${head}.${body}`).digest());
  return `${head}.${body}.${sig}`;
}

// One request. Returns {status, json} or {status: 0} when no answer came (unknown outcome).
// auth: 'token' (the integration's Admin API key), 'session' (the owner's staff session, which
// some endpoints require) or 'none'.
async function api(method, path, payload, auth = 'token') {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), TIMEOUT_MS);
  const headers = { 'Content-Type': 'application/json', 'Accept-Version': 'v6.0', Origin: ORIGIN, ...PROXY };
  if (auth === 'token') headers.Authorization = `Ghost ${token()}`;
  if (auth === 'session') headers.Cookie = readFileSync(SESSIONFILE, 'utf8').trim();
  try {
    const res = await fetch(`${BASE}${path}`, {
      method,
      headers,
      body: payload === undefined ? undefined : JSON.stringify(payload),
      signal: ctl.signal,
    });
    const text = await res.text();
    let json = null;
    try { json = JSON.parse(text); } catch { /* not JSON */ }
    return { status: res.status, json, text, cookie: res.headers.get('set-cookie') };
  } catch (e) {
    return { status: 0, error: String(e && e.message || e) };
  } finally {
    clearTimeout(t);
  }
}

function why(r) {
  if (r.status === 0) return `no answer (${r.error})`;
  const e = r.json && r.json.errors && r.json.errors[0];
  return `${r.status} ${e ? `${e.type}: ${e.message}${e.context ? ` (${e.context})` : ''}` : (r.text || '').slice(0, 160)}`;
}

async function findMember(email) {
  const r = await api('GET', `/members/?filter=${encodeURIComponent(`email:'${email}'`)}&limit=1`);
  if (r.status !== 200) return { r };
  return { r, member: r.json.members[0] };
}

const ops = {
  async key() {
    // stdin: "<id>\t<secret>" from mysql -N
    const line = readFileSync(0, 'utf8').trim().split('\n')[0] || '';
    const [id, secret] = line.split(/\s+/);
    if (!id || !secret) { say(`key: no admin key in MySQL's answer: ${JSON.stringify(line)}`); return 1; }
    mkdirSync('/data/test', { recursive: true });
    writeFileSync(KEYFILE, `${id}:${secret}\n`);
    say(`key: saved the Zapier integration's admin key ${id}`);
    return 0;
  },

  async owner() {
    // the site's owner (Ghost's setup), then a staff session for the operator's actions
    const s = await api('POST', '/authentication/setup/', {
      setup: [{ name: 'Owner', email: OWNER.email, password: OWNER.password, blogTitle: 'Ghost under hydo' }],
    }, 'none');
    if (s.status !== 201 && s.status !== 200) { say(`owner: setup failed: ${why(s)}`); return 1; }
    const l = await api('POST', '/session/', { username: OWNER.email, password: OWNER.password }, 'none');
    const cookie = (l.cookie || '').split(';')[0];
    if (l.status !== 201 || !cookie) { say(`owner: login failed: ${why(l)}`); return 1; }
    writeFileSync(SESSIONFILE, `${cookie}\n`);
    say('owner: set up, staff session saved');
    return 0;
  },

  async stripe() {
    // the operator connects Stripe with API keys (Ghost then registers its webhook at Stripe)
    const r = await api('PUT', '/settings/', {
      settings: [
        { key: 'stripe_secret_key', value: 'sk_test_hydo0000000000000000' },
        { key: 'stripe_publishable_key', value: 'pk_test_hydo0000000000000000' },
      ],
    }, 'session');
    if (r.status !== 200) { say(`stripe: keys not saved: ${why(r)}`); return 1; }
    // Ghost reconfigures Stripe 600 ms after the change; wait until donations are on
    for (let i = 0; i < 30; i++) {
      await new Promise((res) => setTimeout(res, 1000));
      const s = await api('GET', '/settings/', undefined, 'session');
      const on = s.json && (s.json.settings || []).find((x) => x.key === 'donations_enabled');
      const hook = s.json && (s.json.settings || []).find((x) => x.key === 'members_stripe_webhook_id');
      if (on && on.value === true) { say(`stripe: connected, donations on${hook ? ` (webhook ${hook.value})` : ''}`); return 0; }
    }
    say('stripe: donations not enabled after 30 s');
    return 1;
  },

  async donate(n) {
    // a reader starts a donation; Stripe Checkout (the fake) completes it and calls the webhook
    const id = `d-${n}`;
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), TIMEOUT_MS);
    let r;
    try {
      const res = await fetch(`${SERVER}/members/api/create-stripe-checkout-session/`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Origin: ORIGIN, ...PROXY },
        body: JSON.stringify({ type: 'donation', customerEmail: `d${n}@example.com`, metadata: {} }),
        signal: ctl.signal,
      });
      const text = await res.text();
      let json = null;
      try { json = JSON.parse(text); } catch { /* not JSON */ }
      r = { status: res.status, json, text };
    } catch (e) {
      r = { status: 0, error: String(e && e.message || e) };
    } finally {
      clearTimeout(t);
    }
    if (r.status === 200 && r.json && r.json.url) { record(id, 'ack'); say(`${id} checkout ${r.json.url}`); return 0; }
    record(id, r.status === 0 || r.status >= 500 ? 'unknown' : 'failed');
    say(`${id} no checkout: ${why(r)}`);
    return 1;
  },

  async audience(seed) {
    // a new member joins (even seeds) or one of the first 15 leaves every newsletter (odd)
    const s = Number(seed);
    return s % 2 === 0 ? ops.member(100 + (s % 1000)) : ops.unsubscribe(1 + (Math.floor(s / 2) % 15));
  },

  async member(n) {
    const id = `m-${n}`;
    const r = await api('POST', '/members/', { members: [{ email: `m${n}@example.com`, name: `Member ${n}` }] });
    if (r.status === 201) { record(id, 'ack'); say(`${id} added`); return 0; }
    if (r.status === 0 || r.status >= 500) { record(id, 'unknown'); say(`${id} unknown: ${why(r)}`); return 1; }
    record(id, 'failed'); say(`${id} failed: ${why(r)}`); return 1;
  },

  async unsubscribe(n) {
    const id = `u-${n}`;
    const { r, member } = await findMember(`m${n}@example.com`);
    if (!member) { record(id, r.status === 200 ? 'failed' : 'unknown'); say(`${id} no member: ${why(r)}`); return 1; }
    const u = await api('PUT', `/members/${member.id}/`, { members: [{ newsletters: [] }] });
    if (u.status === 200) { order(`u m${n}@example.com`); record(id, 'ack'); say(`${id} unsubscribed`); return 0; }
    record(id, u.status === 0 || u.status >= 500 ? 'unknown' : 'failed'); say(`${id} ${why(u)}`); return 1;
  },

  async publish(n) {
    const id = `p-${n}`;
    const c = await api('POST', '/posts/?source=html', {
      posts: [{ title: `Issue ${n}`, slug: id, html: `<p>Newsletter issue ${n}.</p>`, status: 'draft' }],
    });
    if (c.status !== 201) {
      record(id, c.status === 0 || c.status >= 500 ? 'unknown' : 'failed');
      say(`${id} draft not created: ${why(c)}`);
      return 1;
    }
    const post = c.json.posts[0];
    order(`p ${id}`);
    const p = await api('PUT', `/posts/${post.id}/?newsletter=default-newsletter&email_segment=all`, {
      posts: [{ status: 'published', updated_at: post.updated_at }],
    });
    if (p.status === 200) {
      const email = p.json.posts[0].email;
      record(id, 'ack');
      say(`${id} published, newsletter ${email ? `${email.id} ${email.status} to ${email.email_count}` : 'none'}`);
      return 0;
    }
    record(id, p.status === 0 || p.status >= 500 ? 'unknown' : 'failed');
    say(`${id} not published: ${why(p)}`);
    return 1;
  },

  async retry() {
    const r = await api('GET', `/emails/?filter=${encodeURIComponent('status:failed')}&limit=all`, undefined, 'session');
    if (r.status !== 200) { say(`retry: cannot list emails: ${why(r)}`); return 1; }
    let bad = 0;
    if ((r.json.emails || []).length === 0) say('retry: no failed newsletter');
    for (const e of r.json.emails || []) {
      const x = await api('PUT', `/emails/${e.id}/retry/`, {}, 'session');
      say(`retry ${e.id}: ${x.status === 200 ? 'retried' : why(x)}`);
      if (x.status !== 200) bad = 1;
    }
    return bad;
  },
};

const [op, arg] = process.argv.slice(2);
if (!ops[op]) { console.error(`usage: helper_client.mjs ${Object.keys(ops).join('|')} [n]`); process.exit(2); }
process.exit(await ops[op](arg));

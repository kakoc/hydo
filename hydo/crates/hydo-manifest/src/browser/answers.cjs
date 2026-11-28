// hydo's watcher of the answers a page shows (Milestone 393, `browser.watch.answers`,
// docs/design/web-apps.md "Answers in another order").
//
// `browser.watch.answers` loads this file beside hook.cjs (NODE_OPTIONS=--require) into the
// tests' or the walker's Node process. It records the requests of the pages that the pattern
// `watch.answers` matches (`<METHOD> <path>`, the path without its query, under
// `watch.requests`), and the body of each answer. When an answer comes after the answer of a
// newer request of the same method and path (a search box asked twice, and the first answer
// came last), it reads the page's visible text 300 ms later and compares it with the two
// answers. Each answer's text values are the strings and numbers of its JSON body. A value of
// only the older answer that the page shows, with no value of only the newer answer, is a stale
// page: the page shows an older answer than the newest one it asked for. It appends one line per
// such comparison to $HYDO_WEB_DIR/answers.jsonl, which answers.mjs judges. Without the setting
// nothing loads this file.
'use strict';
const fs = require('fs');
const path = require('path');

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const watch = JSON.parse(process.env.HYDO_WEB_WATCH || '{}');
let pw;
try {
  pw = require(require.resolve('playwright-core', { paths: [process.cwd()] }));
} catch {
  return; // not a Playwright process of this project
}
if (!watch.answers || !pw._instrumentation || typeof pw._instrumentation.addListener !== 'function') {
  return;
}
const matches = new RegExp(watch.answers);
const prefix = watch.requests || '';
// how long the page has to show an answer, ms of guest time
const SETTLE_MS = 300;
// the values of an answer that are compared: at most this many, each at most this long
const MAX_VALUES = 400;
const MAX_LEN = 200;
fs.mkdirSync(dir, { recursive: true });
const file = path.join(dir, 'answers.jsonl');
function record(ev) {
  fs.appendFileSync(file, JSON.stringify({ t: Date.now(), ...ev }) + '\n');
}

// The text values of a JSON answer: its strings and numbers, as a page prints them.
function values(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch {
    return null;
  }
  const out = new Set();
  const walk = (x) => {
    if (out.size >= MAX_VALUES) return;
    if (typeof x === 'string' || typeof x === 'number') {
      const s = String(x).trim();
      if (s && s.length <= MAX_LEN) out.add(s);
    } else if (Array.isArray(x)) {
      x.forEach(walk);
    } else if (x && typeof x === 'object') {
      Object.values(x).forEach(walk);
    }
  };
  walk(v);
  return out;
}

// The page's visible text, one entry per text node, trimmed (an input's value is no text node:
// a search box shows what was typed, not what was answered).
function shownScript() {
  const out = [];
  if (!document.body) return out;
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = w.nextNode(); n; n = w.nextNode()) {
    const t = n.nodeValue.trim();
    const el = n.parentElement;
    if (!t || !el || el.closest('script,style,noscript')) continue;
    if (!(el.offsetWidth || el.offsetHeight || el.getClientRects().length)) continue;
    out.push(t);
  }
  return out;
}

// The shown entries that a value appears in: the whole entry, or as a word of it.
function shownOf(vals, shown) {
  const hit = [];
  for (const v of vals) {
    const re = new RegExp(`(^|[^\\p{L}\\p{N}_])${v.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}([^\\p{L}\\p{N}_]|$)`, 'u');
    if (shown.some((s) => s === v || re.test(s))) hit.push(v);
  }
  return hit;
}

let reqCount = 0;
let arrivals = 0;
const pending = new Set();

function attach(context) {
  const ids = new WeakMap();
  const state = new Map(); // id -> { key, path, page, arrived, body values }
  context.on('request', (req) => {
    const t = req.resourceType();
    if (t !== 'fetch' && t !== 'xhr') return;
    let u;
    try {
      u = new URL(req.url());
    } catch {
      return;
    }
    if (prefix && !u.pathname.startsWith(prefix)) return;
    const key = `${req.method()} ${u.pathname}`;
    if (!matches.test(key)) return;
    const id = ++reqCount;
    ids.set(req, id);
    state.set(id, { id, key, path: u.pathname + u.search, page: req.frame().page() });
  });
  const gone = (req) => {
    const s = state.get(ids.get(req));
    if (s && !s.order) s.failed = true;
  };
  context.on('requestfailed', gone);
  context.on('response', async (res) => {
    const id = ids.get(res.request());
    const a = id && state.get(id);
    if (!a) return;
    let body;
    try {
      body = await res.text();
    } catch {
      a.failed = true;
      return;
    }
    a.order = ++arrivals;
    a.status = res.status();
    a.values = values(body);
    // the newest request of the same method and path whose answer came before this one
    let newer = null;
    for (const b of state.values()) {
      if (b.key === a.key && b.id > a.id && b.order && b.order < a.order && b.page === a.page) {
        if (!newer || b.id > newer.id) newer = b;
      }
    }
    if (!newer) return;
    const p = judge(a, newer, state);
    pending.add(p);
    p.finally(() => pending.delete(p));
  });
}

async function judge(older, newer, state) {
  await new Promise((r) => setTimeout(r, SETTLE_MS));
  const line = {
    key: older.key,
    older: { id: older.id, path: older.path, status: older.status },
    newer: { id: newer.id, path: newer.path, status: newer.status },
  };
  // a request newer than both that has no answer yet decides what the page shows next
  if ([...state.values()].some((c) => c.key === older.key && c.id > newer.id && !c.order && !c.failed && c.page === older.page)) {
    record({ kind: 'undecided', why: 'a newer request has no answer yet', ...line });
    return;
  }
  if (!older.values || !newer.values) {
    record({ kind: 'undecided', why: 'an answer is not JSON', ...line });
    return;
  }
  const onlyOld = [...older.values].filter((v) => !newer.values.has(v));
  const onlyNew = [...newer.values].filter((v) => !older.values.has(v));
  if (!onlyOld.length || !onlyNew.length) {
    record({ kind: 'undecided', why: 'the two answers hold the same values', ...line });
    return;
  }
  let shown;
  let url = '';
  try {
    url = older.page.url();
    shown = await older.page.evaluate(shownScript);
  } catch {
    record({ kind: 'undecided', why: 'the page went away', ...line });
    return;
  }
  const oldShown = shownOf(onlyOld, shown);
  const newShown = shownOf(onlyNew, shown);
  let where = url;
  try {
    const u = new URL(url);
    where = u.pathname + u.search;
  } catch {
    /* about:blank */
  }
  const kind = oldShown.length && !newShown.length ? 'stale' : newShown.length ? 'newest' : 'undecided';
  record({
    kind,
    ...(kind === 'undecided' ? { why: 'the page shows neither answer' } : {}),
    url: where,
    ...line,
    shows: (kind === 'stale' ? oldShown : newShown).slice(0, 5),
    misses: (kind === 'stale' ? onlyNew : []).slice(0, 5),
  });
}

pw._instrumentation.addListener({
  runAfterCreateBrowserContext: (context) => attach(context),
  runBeforeCloseBrowserContext: async () => {
    await Promise.allSettled([...pending]);
  },
});

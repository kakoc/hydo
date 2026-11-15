// hydo's watcher for Playwright tests (Milestone 253, docs/design/web-apps.md).
//
// `browser:` in hydo.yaml loads this file into every Node process of `playwright test` with
// NODE_OPTIONS=--require. The tests stay as they are: the watcher adds a binding and an init
// script to every browser context the tests create, through the hooks that Playwright's own
// test runner uses for its traces. It appends what the pages did to $HYDO_WEB_DIR/events.jsonl,
// one JSON object per line, with the guest's time. check.mjs judges the lines.
'use strict';
const fs = require('fs');
const path = require('path');

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const watch = JSON.parse(process.env.HYDO_WEB_WATCH || '{}');
const from = [process.cwd()];
let pw;
let runner;
try {
  pw = require(require.resolve('playwright-core', { paths: from }));
  runner = require(require.resolve('@playwright/test', { paths: from }));
} catch {
  return; // not a Playwright process of this project
}
if (!pw._instrumentation || typeof pw._instrumentation.addListener !== 'function') {
  return;
}
fs.mkdirSync(path.join(dir, 'shots'), { recursive: true });
const events = path.join(dir, 'events.jsonl');

function currentTest() {
  try {
    const info = runner.test.info();
    return info.titlePath.join(' › ');
  } catch {
    return '';
  }
}

function record(ev) {
  fs.appendFileSync(events, JSON.stringify({ t: Date.now(), ...ev }) + '\n');
}

function where(url) {
  try {
    const u = new URL(url);
    return u.protocol === 'about:' ? url : u.pathname + u.search;
  } catch {
    return url;
  }
}

// Runs in every page before the page's own scripts: uncaught errors, unhandled promise
// rejections, console.error calls, the watched elements appearing and going, and watched text.
function pageScript(w) {
  const send = (ev) => {
    try {
      window.__hydoWeb(ev);
    } catch (_) {
      /* the page is going away */
    }
  };
  const brief = (x) => {
    if (x instanceof Error) return `${x.name}: ${x.message}`;
    try {
      return typeof x === 'string' ? x : JSON.stringify(x);
    } catch (_) {
      return String(x);
    }
  };
  addEventListener('error', (e) => send({ kind: 'error', what: 'exception', message: e.error ? brief(e.error) : String(e.message) }));
  addEventListener('unhandledrejection', (e) => send({ kind: 'error', what: 'rejection', message: brief(e.reason) }));
  if (w.console !== false) {
    const original = console.error;
    console.error = function (...args) {
      send({ kind: 'error', what: 'console', message: args.map(brief).join(' ').slice(0, 300) });
      return original.apply(this, args);
    };
  }
  const shown = {};
  const visible = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
  const words = (w.text || []).map((s) => [s, new RegExp(`(^|[^A-Za-z0-9_])${s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}([^A-Za-z0-9_]|$)`)]);
  const printed = {};
  let scans = 0;
  function scan() {
    scans++;
    for (const [kind, sels] of [['busy', w.busy || []], ['alert', w.alert || []]]) {
      for (const sel of sels) {
        let el;
        try {
          el = Array.from(document.querySelectorAll(sel)).find(visible);
        } catch (_) {
          el = undefined;
        }
        const key = `${kind} ${sel}`;
        const text = el ? el.innerText.trim().slice(0, 200) : undefined;
        if (text !== shown[key]) {
          // appeared, went, or another text while it stays (a spinner of the next page)
          shown[key] = text;
          send({ kind, selector: sel, on: !!el, text, scan: scans });
        }
      }
    }
    if (words.length && document.body) {
      const all = document.body.innerText || '';
      for (const [s, re] of words) {
        if (!printed[s] && re.test(all)) {
          printed[s] = true;
          const line = all.split('\n').find((l) => re.test(l)) || '';
          send({ kind: 'text', value: s, line: line.trim().slice(0, 200) });
        }
      }
    }
  }
  let queued = false;
  new MutationObserver(() => {
    if (!queued) {
      queued = true;
      queueMicrotask(() => {
        queued = false;
        scan();
      });
    }
  }).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  addEventListener('DOMContentLoaded', scan);
}

const pages = new WeakMap();
let pageCount = 0;

function pageId(page) {
  if (!page) return 0;
  if (!pages.has(page)) pages.set(page, ++pageCount);
  return pages.get(page);
}

function fromPage(page) {
  return { test: currentTest(), page: pageId(page), url: page ? where(page.url()) : '' };
}

function isApi(req) {
  const t = req.resourceType();
  if (t !== 'fetch' && t !== 'xhr') return false;
  return !watch.requests || where(req.url()).startsWith(watch.requests);
}

async function attach(context) {
  await context.exposeBinding('__hydoWeb', (source, ev) => record({ ...fromPage(source.page), ...ev }));
  await context.addInitScript(pageScript, watch);
  const onPage = (page) => {
    record({ ...fromPage(page), kind: 'page' });
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) record({ ...fromPage(page), kind: 'nav' });
    });
  };
  context.pages().forEach(onPage);
  context.on('page', onPage);
  const about = (req, kind, extra) => {
    if (!isApi(req)) return;
    if (!requests.has(req)) requests.set(req, ++requestCount);
    const page = req.frame().page();
    record({ ...fromPage(page), kind, id: requests.get(req), method: req.method(), path: where(req.url()), ...extra });
  };
  context.on('request', (req) => about(req, 'send', {}));
  context.on('requestfailed', (req) => about(req, 'request', { error: req.failure() ? req.failure().errorText : 'failed' }));
  context.on('response', (res) => about(res.request(), 'request', { status: res.status() }));
}

const requests = new WeakMap();
let requestCount = 0;

let shot = 0;
async function finish(context) {
  for (const page of context.pages()) {
    let png;
    try {
      const open = await page.evaluate((w) => {
        const visible = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
        return [...(w.busy || []), ...(w.alert || [])].some((s) => Array.from(document.querySelectorAll(s)).some(visible));
      }, watch);
      if (open) {
        png = path.join(dir, 'shots', `${++shot}.png`);
        await page.screenshot({ path: png });
      }
    } catch {
      png = undefined;
    }
    record({ ...fromPage(page), kind: 'close', shot: png });
  }
}

pw._instrumentation.addListener({
  runAfterCreateBrowserContext: (context) => attach(context),
  runBeforeCloseBrowserContext: (context) => finish(context),
});

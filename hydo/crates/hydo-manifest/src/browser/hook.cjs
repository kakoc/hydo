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
  // a new document starts: the elements of the document before it are gone (Milestone 301: a
  // spinner of a page that the walk left by going back stayed open until the test ended)
  send({ kind: 'doc' });
  const brief = (x) => {
    if (x instanceof Error) return `${x.name}: ${x.message}`;
    try {
      return typeof x === 'string' ? x : JSON.stringify(x);
    } catch (_) {
      return String(x);
    }
  };
  addEventListener('error', (e) => {
    // Chromium reports a ResizeObserver whose callbacks changed the layout again as an error
    // event without an error object; no code threw, and the page goes on (Milestone 258: Memos)
    if (!e.error && /^ResizeObserver loop/.test(String(e.message))) return;
    send({ kind: 'error', what: 'exception', message: e.error ? brief(e.error) : String(e.message) });
  });
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
  await context.exposeBinding('__hydoWeb', (source, ev) => {
    // only the page's own document starts or ends a page's elements, not a frame inside it
    if (ev && ev.kind === 'doc' && source.page && source.frame !== source.page.mainFrame()) return;
    record({ ...fromPage(source.page), ...ev });
  });
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

// A screenshot's file name: the test's title path and the page's number in the test, so that a
// run and its reference name the same page alike (Milestone 254).
const shotsOfTest = new Map();
function shotName(test) {
  const n = (shotsOfTest.get(test) || 0) + 1;
  shotsOfTest.set(test, n);
  const slug = (test || 'no-test').replace(/[^A-Za-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 120);
  return `${slug || 'test'}-${n}.png`;
}

// At a context's close: a screenshot of each page that still shows a busy or error element, or of
// every page with `screens: all` or `compare`. Each goes to shots/ with a line in shots/index.jsonl,
// which the host controller reads after the verdict to copy them into the run directory.
async function finish(context) {
  const every = watch.screens === 'all' || watch.screens === 'compare';
  for (const page of context.pages()) {
    let file;
    const at = fromPage(page);
    try {
      const open = await page.evaluate((w) => {
        const visible = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
        return [...(w.busy || []), ...(w.alert || [])].some((s) => Array.from(document.querySelectorAll(s)).some(visible));
      }, watch);
      if (open || every) {
        file = shotName(at.test);
        await page.screenshot({ path: path.join(dir, 'shots', file) });
        const line = { file, test: at.test, url: at.url, why: open ? 'busy or error element' : 'end of test' };
        fs.appendFileSync(path.join(dir, 'shots', 'index.jsonl'), JSON.stringify(line) + '\n');
      }
    } catch {
      file = undefined;
    }
    record({ ...at, kind: 'close', shot: file });
  }
}

pw._instrumentation.addListener({
  runAfterCreateBrowserContext: (context) => attach(context),
  runBeforeCloseBrowserContext: (context) => finish(context),
});

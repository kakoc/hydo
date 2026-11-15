// hydo's checks over what the pages did (Milestone 253, docs/design/web-apps.md).
//
//   node check.mjs <rule> [ms]
//
// It reads $HYDO_WEB_DIR/events.jsonl, which hook.cjs and reporter.cjs wrote, and prints one
// sentence. Exit 0: the rule holds; 1: it does not; 2: nothing to judge (no test ran).
//
//   tests            every test passed
//   errors           no page threw an uncaught exception, left a promise rejection unhandled, or
//                    called console.error
//   busy <ms>        no busy element (a spinner) stayed on screen for <ms> or more, and none was
//                    still there when its test ended after a failed request that the page did not
//                    send again
//   failures <ms>    every failed request was followed, within <ms>, by an error element on the
//                    page or by the page sending the request again
//   text             no page printed a watched text, such as NaN or undefined
//
// A request fails when it is answered 500 or more, or gets no answer (the network failed, or the
// page gave up waiting). A request cut off by the end of its test, or by the page going to
// another document, is not judged.
import fs from 'node:fs';
import path from 'node:path';

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const [rule, msArg] = process.argv.slice(2);
const limit = Number(msArg || 0);

let lines = [];
try {
  lines = fs.readFileSync(path.join(dir, 'events.jsonl'), 'utf8').split('\n').filter(Boolean);
} catch {
  lines = [];
}
const events = lines.map((l) => JSON.parse(l));
const tests = events.filter((e) => e.kind === 'test');
const pageEvents = events.filter((e) => e.kind !== 'test');
if (tests.length === 0) {
  console.log('no Playwright test ran, so there is nothing to judge');
  process.exit(2);
}

const secs = (ms) => `${(ms / 1000).toFixed(1)} s`;
const during = (e) => (e.test ? ` during the test "${e.test}"` : '');
const req = (e) => `${e.method} ${e.path}`;
const outcome = (e) => (e.status !== undefined ? `answered ${e.status}` : `got no answer (${e.error})`);

// the events of one page, in order: a page is one browser tab of one test
const byPage = new Map();
for (const e of pageEvents) {
  const key = `${e.test}\u0000${e.page}`;
  if (!byPage.has(key)) byPage.set(key, []);
  byPage.get(key).push(e);
}

function closeOf(list) {
  return list.find((e) => e.kind === 'close');
}

// the failed requests of a page that are judged
function failures(list) {
  const close = closeOf(list);
  return list.filter(
    (e) =>
      e.kind === 'request' &&
      (e.status === undefined || e.status >= 500) &&
      !(close && e.t >= close.t) &&
      !(e.error === 'net::ERR_ABORTED' && list.some((n) => n.kind === 'nav' && Math.abs(n.t - e.t) <= 100)),
  );
}

// did the page send the same request again after f was sent, before `end`? (ids count up in the
// order the page sent its requests)
function sentAgain(list, f, end) {
  return list.some((s) => s.kind === 'send' && s.id > f.id && s.t <= end && s.method === f.method && s.path === f.path);
}

function finish(found, okText) {
  if (found.length === 0) {
    console.log(okText);
    process.exit(0);
  }
  const more = found.length > 1 ? ` (${found.length} times in this run; this is the first)` : '';
  console.log(found[0] + more);
  process.exit(1);
}

function extras(list, before) {
  const err = list.filter((e) => e.kind === 'error' && e.t <= before).pop();
  const c = list.find((e) => e.kind === 'close' && e.shot);
  return `${err ? ` Console: ${err.message}` : ''}${c ? ` Screenshot: ${c.shot}` : ''}`;
}

if (rule === 'tests') {
  const bad = tests.filter((t) => t.status !== t.expected && t.status !== 'skipped');
  finish(
    bad.map((t) => `The test "${t.test}" ${t.status} after ${secs(t.ms)}: ${t.error || 'no error message'}`),
    `all ${tests.length} tests passed`,
  );
} else if (rule === 'errors') {
  const what = { exception: 'threw an uncaught exception', rejection: 'left a promise rejection unhandled', console: 'logged an error' };
  const found = pageEvents
    .filter((e) => e.kind === 'error')
    .map((e) => `The page ${e.url} ${what[e.what] || 'failed'}${during(e)}: ${e.message}`);
  finish(found, 'no page threw an error or logged one');
} else if (rule === 'busy') {
  const found = [];
  for (const list of byPage.values()) {
    const fails = failures(list);
    const open = new Map(); // selector -> {start, latest}
    const seen = new Set(); // one finding per busy element, whichever of the selectors matched it
    const judge = (o, end, atClose) => {
      const held = end.t - o.start.t;
      const inside = fails.filter((f) => f.t >= o.start.t && f.t <= end.t);
      const last = inside[inside.length - 1];
      const stuck = atClose && last && !sentAgain(list, last, end.t);
      const once = `${o.start.scan}:${o.start.text}`;
      if ((held >= limit || stuck) && !seen.has(once)) {
        seen.add(once);
        const until = atClose ? ', until the test ended' : '';
        const after = last ? `, after ${req(last)} ${outcome(last)}` : '';
        const alert = list.some((a) => a.kind === 'alert' && a.on && a.t >= o.start.t && a.t <= end.t);
        found.push(
          `The page ${o.latest.url} showed the busy element ${o.start.selector} ("${o.latest.text}") for ${secs(held)}${until}${after}` +
            `${o.start.test ? ` during the test "${o.start.test}"` : ''}${alert ? '' : '; it showed no error'}.${extras(list, end.t)}`,
        );
      }
    };
    for (const e of list) {
      if (e.kind === 'busy' && e.on) {
        if (open.has(e.selector)) open.get(e.selector).latest = e;
        else open.set(e.selector, { start: e, latest: e });
      } else if (e.kind === 'busy' && open.has(e.selector)) {
        judge(open.get(e.selector), e, false);
        open.delete(e.selector);
      } else if (e.kind === 'close') {
        for (const o of open.values()) judge(o, e, true);
        open.clear();
      }
    }
  }
  finish(found, `no busy element stayed for ${secs(limit)}, or until a test ended after a failed request`);
} else if (rule === 'failures') {
  const found = [];
  for (const list of byPage.values()) {
    const close = closeOf(list);
    for (const f of failures(list)) {
      const end = Math.min(f.t + limit, close ? close.t : Infinity);
      const before = list.filter((a) => a.kind === 'alert' && a.t < f.t).pop();
      const shown = (before && before.on) || list.some((a) => a.kind === 'alert' && a.on && a.t >= f.t && a.t <= end);
      if (!shown && !sentAgain(list, f, end)) {
        found.push(
          `The page ${f.url} showed no error when ${req(f)} ${outcome(f)}${during(f)}; nothing appeared in the next ${secs(end - f.t)}.` +
            extras(list, end),
        );
      }
    }
  }
  finish(found, `every failed request was followed by an error on the page, or sent again, within ${secs(limit)}`);
} else if (rule === 'text') {
  const found = pageEvents.filter((e) => e.kind === 'text').map((e) => `The page ${e.url} printed "${e.value}"${during(e)}: "${e.line}"`);
  finish(found, 'no page printed a watched text');
} else {
  console.log(`check.mjs: unknown rule ${rule}`);
  process.exit(2);
}

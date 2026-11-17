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
//   screens <ref> [tests]  every screenshot that hook.cjs took at the end of a test is the file of the
//                    same name in the folder <ref>, pixel for pixel, and every reference has its
//                    screenshot (Milestone 254, `screens: compare`). A screenshot that differs
//                    gets <name>.diff.png beside it: the reference in grey, the changed pixels red
//
// A request fails when it is answered 500 or more, or gets no answer (the network failed, or the
// page gave up waiting). A request cut off by the end of its test, or by the page going to
// another document, is not judged.
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const [rule, msArg] = process.argv.slice(2);
const limit = Number(msArg || 0);
// browser.watch.failures_of (Milestone 258): only these requests must show their failure
const watchCfg = JSON.parse(process.env.HYDO_WEB_WATCH || '{}');
const failuresRe = watchCfg.failures_of ? new RegExp(watchCfg.failures_of) : null;
// Milestone 273: the pattern also matches `<METHOD> <path>`, so that an API whose reads and writes
// share a path (GET and PATCH /api/v2/tasks/4) can name its writes: ^(POST|PUT|PATCH|DELETE) /api/
const failuresOf = failuresRe && { test: (e) => failuresRe.test(e.path) || failuresRe.test(`${e.method} ${e.path}`) };

// The workload makes this folder before its first step. A recovery boot after a power cut runs
// no workload, and the guest's /tmp starts empty: the pages of the first boot were judged there
// (Milestone 258).
if (!fs.existsSync(dir)) {
  console.log('no page ran in this boot (a recovery boot after a power cut runs no workload)');
  process.exit(0);
}
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
  return `${err ? ` Console: ${err.message}` : ''}${c ? ` Screenshot: screens/${c.shot} in the run directory.` : ''}`;
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
      } else if (e.kind === 'doc' || (e.kind === 'nav' && String(e.url).startsWith('about:'))) {
        // a new document (a reload, a back to another page, a link out): the elements of the
        // one before are gone with it, and they end here (Milestone 301)
        for (const o of open.values()) judge(o, e, false);
        open.clear();
      }
    }
  }
  finish(found, `no busy element stayed for ${secs(limit)}, or until a test ended after a failed request`);
} else if (rule === 'failures') {
  const found = [];
  for (const list of byPage.values()) {
    const close = closeOf(list);
    for (const f of failures(list).filter((x) => !failuresOf || failuresOf.test(x))) {
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
  // Milestone 264: a watch.failures_of that matches none of the requests the pages sent judges
  // nothing; the held sentence says so, and hydo turns it into a warning of the run
  // (hydo_explore::results::FAILURES_OF_UNMATCHED)
  let unmatched = '';
  const sent = pageEvents.filter((e) => e.kind === 'send');
  if (failuresOf && sent.length > 0 && !sent.some((e) => failuresOf.test(e))) {
    const ex = sent.find((e) => e.method !== 'GET') || sent[0];
    unmatched =
      `; but watch.failures_of (${watchCfg.failures_of}) matched none of the ${sent.length} requests the pages sent` +
      ` (such as ${req(ex)}), so no failed request was judged`;
  }
  finish(found, `every failed request was followed by an error on the page, or sent again, within ${secs(limit)}${unmatched}`);
} else if (rule === 'text') {
  const found = pageEvents.filter((e) => e.kind === 'text').map((e) => `The page ${e.url} printed "${e.value}"${during(e)}: "${e.line}"`);
  finish(found, 'no page printed a watched text');
} else if (rule === 'screens') {
  finish(...compareScreens(msArg));
} else {
  console.log(`check.mjs: unknown rule ${rule}`);
  process.exit(2);
}

// The screenshots of the run against the references in `ref` (rule `screens`). Deterministic runs
// give the same pixels, so the comparison is exact: any changed pixel is a difference.
function compareScreens(ref) {
  const shots = path.join(dir, 'shots');
  const tests = process.argv[4] || '/opt/hydo/web/tests'; // the Playwright project in the guest
  // pngjs, as Playwright's own screenshot comparison uses it, from the tests' node_modules
  const { PNG } = createRequire(path.join(tests, 'package.json'))('playwright-core/lib/utilsBundle');
  const index = [];
  try {
    for (const l of fs.readFileSync(path.join(shots, 'index.jsonl'), 'utf8').split('\n')) {
      if (l) index.push(JSON.parse(l));
    }
  } catch {
    /* no screenshot */
  }
  const taken = index.filter((e) => !e.file.endsWith('.diff.png'));
  const names = new Set(taken.map((e) => e.file));
  let refs = [];
  try {
    refs = fs.readdirSync(ref).filter((f) => f.endsWith('.png') && !f.endsWith('.diff.png'));
  } catch {
    refs = [];
  }
  const n = (x) => String(x).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const found = [];
  for (const e of taken) {
    const of = `The screenshot screens/${e.file} of the page ${e.url} at the end of the test "${e.test}"`;
    const rp = path.join(ref, e.file);
    if (!fs.existsSync(rp)) {
      found.push(
        `${of} has no reference: if the page looks right, copy screens/${e.file} from this run's directory into the reference folder.`,
      );
      continue;
    }
    const a = fs.readFileSync(path.join(shots, e.file));
    const b = fs.readFileSync(rp);
    if (a.equals(b)) continue;
    const pa = PNG.sync.read(a);
    const pb = PNG.sync.read(b);
    if (pa.width !== pb.width || pa.height !== pb.height) {
      found.push(`${of} is ${pa.width}×${pa.height} pixels and its reference is ${pb.width}×${pb.height}.`);
      continue;
    }
    const diff = new PNG({ width: pa.width, height: pa.height });
    let changed = 0;
    let [x0, y0, x1, y1] = [pa.width, pa.height, -1, -1];
    for (let y = 0; y < pa.height; y++) {
      for (let x = 0; x < pa.width; x++) {
        const i = (y * pa.width + x) * 4;
        const same =
          pa.data[i] === pb.data[i] &&
          pa.data[i + 1] === pb.data[i + 1] &&
          pa.data[i + 2] === pb.data[i + 2] &&
          pa.data[i + 3] === pb.data[i + 3];
        if (same) {
          const g = 255 - Math.round((255 - (pb.data[i] * 0.3 + pb.data[i + 1] * 0.59 + pb.data[i + 2] * 0.11)) * 0.3);
          diff.data.set([g, g, g, 255], i);
        } else {
          changed++;
          diff.data.set([230, 0, 0, 255], i);
          x0 = Math.min(x0, x);
          y0 = Math.min(y0, y);
          x1 = Math.max(x1, x);
          y1 = Math.max(y1, y);
        }
      }
    }
    if (changed === 0) continue; // other bytes, the same pixels
    const dname = e.file.replace(/\.png$/, '.diff.png');
    fs.writeFileSync(path.join(shots, dname), PNG.sync.write(diff));
    if (!index.some((x) => x.file === dname)) {
      const line = { file: dname, test: e.test, url: e.url, why: `the changed pixels of ${e.file}` };
      fs.appendFileSync(path.join(shots, 'index.jsonl'), JSON.stringify(line) + '\n');
    }
    const total = pa.width * pa.height;
    const pct = ((100 * changed) / total).toFixed(changed * 10000 < total ? 3 : 2);
    found.push(
      `${of} differs from its reference in ${n(changed)} of ${n(total)} pixels (${pct} %), inside the box from x=${x0}, y=${y0} to x=${x1}, y=${y1}. ` +
        `The changed pixels are red in screens/${dname}.`,
    );
  }
  for (const f of refs) {
    if (!names.has(f)) {
      found.push(`The reference ${f} has no screenshot in this run: no test ended on that page (a test that stopped early, or a renamed test).`);
    }
  }
  return [found, `all ${taken.length} screenshots are their references, pixel for pixel`];
}

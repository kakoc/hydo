// hydo's walker: a seeded walk through a web app's pages (Milestone 258,
// docs/design/ui-exploration.md, `browser.explore:` in hydo.yaml).
//
//   node walk.mjs <step> [<walk>] [<guest part of the schedule>]
//
// Each workload step runs one part of the walk in a fresh browser that keeps the cookies, the
// local storage and the page of the part before ($HYDO_WEB_DIR/walk/state.json). <walk> is the
// value of the schedule's I= token: <seed hex>[,n<actions>][,x<step>.<k>[-<last>]]...[,rec]; without it
// the walk is seed 0. Every action reads the page's accessibility tree (Playwright's
// ariaSnapshot) and draws what to do from its own stream (seed, step, k): click a link or a
// button, fill a text field with a seeded value, choose an option, press Enter, click twice, go
// back, reload, or wait. The element is chosen by rendezvous hashing of its role, name and place,
// so leaving one action out (x<step>.<k>) leaves the other actions' choices as they were. A time
// or a date in a name ("1 minute ago", "now", "2026-10-16 00:00:42") counts as one word for the
// choice (Milestone 264), so that a clock step, which changes such names, does not change which
// control the walk takes. When a menu or a dialog is open, an action most often acts in it
// (Milestone 267).
//
// The watcher (hook.cjs, loaded with NODE_OPTIONS=--require) watches every page as it watches a
// team's Playwright tests, and check.mjs judges what it saw. This file also writes the walk as a
// Playwright test, repro.spec.ts, which the controller copies into the run directory's screens/.
// With rec, the last step that takes an action also leaves a video and a Playwright trace there,
// and the walk repeats the actions that the same schedule without rec took, which the host writes
// to $HYDO_WEB_DIR/walk-script.txt, instead of choosing them again (Milestone 270). Every run
// prints each action it took as a `walk-act <step>.<k> <json>` line for that script.
//
// Since Milestone 334 every run keeps a filmstrip of the walk: after the page opens and after
// every action a small JPEG of the page (480×270, film-<step>-<k>.jpg) and one line in
// film.jsonl with the action's sentence, the page's path and what the watcher saw go wrong since
// the action before. Every step also records a Playwright trace, which the controller copies out
// only when the run fails (index line `only: failed`).
//
//   node walk.mjs --read-back <acked file>
//
// is the check `browser.read_back` (Milestone 334): it opens the pages that the configuration
// names, in a fresh browser with the walk's session (or after its setup steps, as in a recovery
// boot after a power cut), and looks there for every object whose id the server acknowledged in
// a write ($HYDO_ACKED's `<METHOD>:<path>=<id>` words). An object that no page shows is a
// finding. Exit 0: every object is shown, or none was to be looked for; 1: one is not.
//
// The configuration is $HYDO_WEB_WALK (JSON, written by the expansion of `browser.explore`):
//   start, actions, setup, avoid, within, values, settle_ms, quiet_ms, text, routes, read_back
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
// playwright-core is loaded when a browser starts, so that `--read-back ... --list` runs without it
const chromium = { launch: async (o) => (await import('playwright-core')).chromium.launch(o) };

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const cfg = JSON.parse(process.env.HYDO_WEB_WALK || '{}');
const readingBack = process.argv[2] === '--read-back';
const step = readingBack ? 0 : Number(process.argv[2] || 0);
const walk = parseWalk(readingBack ? '' : process.argv[3] || '');
const guest = readingBack ? '' : process.argv[4] || '';
const ackedFile = (readingBack ? process.argv[3] : process.argv[5]) || '';
const watch = JSON.parse(process.env.HYDO_WEB_WATCH || '{}');
const actions = walk.actions || cfg.actions || 10;
const settle = cfg.settle_ms ?? 700;
const quiet = cfg.quiet_ms ?? 3000;
const start = new URL(cfg.start || 'http://localhost:3000/');
const within = cfg.within || '/';
const avoid = (cfg.avoid || ['log ?out', 'sign ?out']).map((a) => new RegExp(a, 'i'));
// Milestone 390: the steps that sign in again when a page of the walk is the sign-in page
// (`sign_in`, on a path that `signed_out` matches)
const signIn = Array.isArray(cfg.sign_in) && cfg.sign_in.length ? cfg.sign_in : null;
const signedOut = new RegExp(cfg.signed_out || '(^|/)(login|log-in|signin|sign-in|sign_in)(/|$)', 'i');
const title = readingBack ? 'read back' : `walk step ${step}`;
globalThis.__hydoWalkTest = title; // the stub @playwright/test hands it to hook.cjs
const own = path.join(dir, 'walk');
const shots = path.join(dir, 'shots');
fs.mkdirSync(own, { recursive: true });
fs.mkdirSync(shots, { recursive: true });

function parseWalk(v) {
  const w = { seed: 0n, actions: 0, skip: new Set(), rec: false, recStep: undefined };
  if (!v) return w;
  const [seed, ...fields] = v.split(',');
  w.seed = BigInt(`0x${seed}`);
  for (const f of fields) {
    if (f === 'rec') w.rec = true;
    else if (/^rec\d+$/.test(f)) {
      // rec<step>: record that step (Milestone 267: the step of a failure)
      w.rec = true;
      w.recStep = Number(f.slice(3));
    } else if (f.startsWith('n')) w.actions = Number(f.slice(1));
    else if (f.startsWith('x')) {
      // x<step>.<k>, or x<step>.<k>-<last> (Milestone 264)
      const m = /^x(\d+)\.(\d+)(?:-(\d+))?$/.exec(f);
      if (m) for (let k = Number(m[2]); k <= Number(m[3] ?? m[2]); k++) w.skip.add(`${m[1]}.${k}`);
    }
  }
  return w;
}

// ------------------------------------------------------------------ the seeded streams
const M = (1n << 64n) - 1n;
function mix(z) {
  z = (z + 0x9e3779b97f4a7c15n) & M;
  z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & M;
  z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & M;
  return z ^ (z >> 31n);
}
function fnv(s) {
  let h = 0xcbf29ce484222325n;
  for (const b of Buffer.from(s)) {
    h ^= BigInt(b);
    h = (h * 0x100000001b3n) & M;
  }
  return h;
}
class Stream {
  constructor(s) {
    this.s = s;
  }
  next() {
    this.s = (this.s + 0x9e3779b97f4a7c15n) & M;
    return mix(this.s);
  }
  below(n) {
    return Number(this.next() % BigInt(n));
  }
}
const WALK_SALT = 0x5741_4c4b_c0ff_ee58n;
const streamOf = (k) => mix(walk.seed ^ WALK_SALT ^ mix((BigInt(step) << 20n) | BigInt(k)));

// ------------------------------------------------------------------ records
const log = (line) => process.stdout.write(`hydo-web: ${line}\n`);
function event(ev) {
  fs.appendFileSync(path.join(dir, 'events.jsonl'), JSON.stringify({ t: Date.now(), ...ev }) + '\n');
}
function indexLine(file, why, extra = {}) {
  const index = path.join(shots, 'index.jsonl');
  let have = '';
  try {
    have = fs.readFileSync(index, 'utf8');
  } catch {
    have = '';
  }
  if (!have.split('\n').some((l) => l.includes(`"file":${JSON.stringify(file)}`))) {
    fs.appendFileSync(index, JSON.stringify({ file, test: title, url: '', why, ...extra }) + '\n');
  }
}

// ------------------------------------------------------------------ the filmstrip (Milestone 334)
// After the page opens and after every action: a JPEG of the viewport at three eighths of its size
// (480×270, a few kilobytes), taken through the page's DevTools session so that the page itself
// does not change, and one line in film.jsonl. The controller copies both out of the guest after
// the verdict, and at a power cut before the cut, so that `hydo report --html` shows the walk of
// the failing run action by action. A frame that cannot be taken (the page went away) keeps its
// line without a file.
const FILM = path.join(shots, 'film.jsonl');
const EVENTS = path.join(dir, 'events.jsonl');
let eventsRead = 0;
let cdp = null;
const shortPath = (u) => {
  try {
    const x = new URL(u);
    return x.protocol === 'about:' ? u : x.pathname + x.search;
  } catch {
    return String(u || '');
  }
};
function eventsSoFar() {
  try {
    eventsRead = fs.statSync(EVENTS).size;
  } catch {
    eventsRead = 0;
  }
}
// what the watcher (hook.cjs, in this process) recorded since the frame before: the errors, the
// failed requests and the error elements that appeared
function newNotes() {
  let buf;
  try {
    buf = fs.readFileSync(EVENTS);
  } catch {
    return [];
  }
  const text = buf.subarray(eventsRead).toString('utf8');
  const end = text.lastIndexOf('\n');
  if (end < 0) return [];
  eventsRead += Buffer.byteLength(text.slice(0, end + 1));
  const out = [];
  const what = { exception: 'uncaught exception', rejection: 'unhandled promise rejection', console: 'console.error' };
  for (const l of text.slice(0, end).split('\n')) {
    let e;
    try {
      e = JSON.parse(l);
    } catch {
      continue;
    }
    if (e.kind === 'error') out.push(`${what[e.what] || 'error'}: ${String(e.message).slice(0, 160)}`);
    else if (e.kind === 'request' && (e.status === undefined || e.status >= 500)) {
      out.push(`${e.method} ${e.path} ${e.status !== undefined ? `answered ${e.status}` : `got no answer (${e.error})`}`);
    } else if (e.kind === 'alert' && e.on) out.push(`error shown: ${String(e.text || e.selector).slice(0, 160)}`);
  }
  return out.slice(0, 8);
}
async function frame(page, k, what, extra = {}) {
  const name = `film-${readingBack ? 'check' : step}-${typeof k === 'number' ? String(k).padStart(3, '0') : k}.jpg`;
  let file;
  for (let i = 0; i < 2 && !file; i++) {
    try {
      cdp ??= await page.context().newCDPSession(page);
      const m = await cdp.send('Page.getLayoutMetrics');
      const v = m.cssVisualViewport || m.visualViewport;
      const r = await cdp.send('Page.captureScreenshot', {
        format: 'jpeg',
        quality: 50,
        clip: { x: v.pageX, y: v.pageY, width: v.clientWidth, height: v.clientHeight, scale: 0.375 },
      });
      const jpeg = Buffer.from(r.data, 'base64');
      fs.writeFileSync(path.join(shots, name), jpeg);
      file = name;
      extra = { ...extra, bytes: jpeg.length };
    } catch {
      cdp = null; // a new document in another process: a new session, once
    }
  }
  const line = { step: readingBack ? null : step, k, file, what, url: shortPath(page.url()), t: Date.now(), notes: newNotes(), ...extra };
  fs.appendFileSync(FILM, JSON.stringify(line) + '\n');
}

// The walk as Playwright code, for repro.spec.ts: each step's lines in walk/repro-<step>.json.
const code = [];
const q = (s) => JSON.stringify(s);
const locate = (page, c) => (c.name ? page.getByRole(c.role, { name: c.name, exact: true }).nth(c.m) : page.getByRole(c.role).nth(c.j));
function locatorCode(c) {
  return c.name
    ? `page.getByRole(${q(c.role)}, { name: ${q(c.name)}, exact: true }).nth(${c.m})`
    : `page.getByRole(${q(c.role)}).nth(${c.j})`;
}

// ------------------------------------------------------------------ the page's controls
const CLICK = new Set(['link', 'button', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'tab', 'checkbox', 'radio', 'switch', 'option', 'treeitem']);
const FILL = new Set(['textbox', 'searchbox', 'spinbutton']);

function unquote(line) {
  // ariaSnapshot quotes a whole key in '...' when it needs it ('' is one ')
  const m = /^(\s*)- '(.*)'(:.*)?$/.exec(line);
  return m ? `${m[1]}- ${m[2].replace(/''/g, "'")}${m[3] || ''}` : line;
}

// A name as the walk's choice sees it: every time and date as <time> (Milestone 264). A memo's
// age ("now", "1 minute ago", "in 30 days") or a time stamp changes with the guest's clock, and a
// clock step would otherwise change the walk; the locator keeps the real name.
const TIMES = [
  /\b(?:just now|now|today|yesterday|tomorrow)\b/gi,
  /\b(?:less than |about |almost |over )?(?:a few|an?|\d+) (?:second|sec|minute|min|hour|day|week|month|year)s?(?: ago)?\b/gi,
  /\b\d{4}-\d{2}-\d{2}(?:[ T]\d{1,2}:\d{2}(?::\d{2})?)?\b/g,
  /\b\d{1,2}\/\d{1,2}\/\d{2,4}\b/g, // 11/15/2026 (Milestone 267: Memos's archived memos)
  /\b(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]* \d{1,2}(?:, \d{4})?\b/gi,
  /\b\d{1,2}:\d{2}(?::\d{2})?(?: ?[ap]\.?m\.?)?(?![\w])/gi,
];
function timeless(name) {
  let n = name;
  for (const r of TIMES) n = n.replace(r, '<time>');
  return n.replace(/\bin <time>/gi, '<time>');
}

// What opens over a page: a dialog, a menu, a list of options. A user acts in the one that opened
// last (Milestone 267).
const OVERLAY = new Set(['dialog', 'alertdialog', 'menu', 'listbox']);

// The controls of an ariaSnapshot, in document order: {role, name, m (place among the same role
// and name), j (place among the same role), key and km (the name without its times, and the
// place among the same role and key), url, disabled, over (the overlays it is in, numbered in
// document order)}.
function controls(snapshot) {
  const out = [];
  const byKey = new Map();
  const byName = new Map();
  const byRole = new Map();
  const open = []; // the overlays around the line: {indent, id}
  let overlays = 0;
  let last = null;
  for (const raw of snapshot.split('\n')) {
    const line = unquote(raw);
    const u = /^\s*- \/url: (.*)$/.exec(line);
    if (u && last) {
      last.url = u[1].trim().replace(/^"(.*)"$/, '$1');
      continue;
    }
    const m = /^(\s*)- ([a-z]+)(?: "((?:[^"\\]|\\.)*)")?((?: \[[^\]]*\])*)(?::.*)?$/.exec(line);
    if (!m) continue;
    const indent = m[1].length;
    while (open.length && open[open.length - 1].indent >= indent) open.pop();
    const over = open.map((o) => o.id);
    if (OVERLAY.has(m[2])) open.push({ indent, id: ++overlays });
    const role = m[2];
    let name = '';
    try {
      name = m[3] === undefined ? '' : JSON.parse(`"${m[3]}"`);
    } catch {
      name = m[3] || '';
    }
    const j = byRole.get(role) || 0;
    byRole.set(role, j + 1);
    const key = `${role}\u0000${name}`;
    const n = byName.get(key) || 0;
    byName.set(key, n + 1);
    const tk = timeless(name);
    const kk = `${role}\u0000${tk}`;
    const km = byKey.get(kk) || 0;
    byKey.set(kk, km + 1);
    const c = { role, name, m: n, j, key: tk, km, url: undefined, disabled: /\[disabled/.test(m[4] || ''), over };
    out.push(c);
    last = role === 'link' ? c : null;
  }
  return out;
}

function allowed(c) {
  if (c.disabled) return false;
  if (avoid.some((r) => r.test(c.name))) return false;
  if (c.role === 'link' && c.url !== undefined) {
    let u;
    try {
      u = new URL(c.url, start);
    } catch {
      return false;
    }
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return false;
    if (u.origin !== start.origin || !u.pathname.startsWith(within)) return false;
  }
  return true;
}

// A family of controls: the role, and the name with every number as # (the 30 day buttons of a
// calendar are one family, as the rows of a list are). The walk first chooses a family, then a
// control of it, so that a page with many alike controls does not hold the walk on them.
const family = (c) => `${c.role}\u0000${c.key.replace(/\d+/g, '#')}`;
// How often the walk acted on each family in this step. Milestone 258 carried the counts from
// step to step; once two walks parted, their counts parted, and so did every later choice of the
// walk. Since Milestone 267 each step starts with none, so a step that starts on the same page
// makes the same choices.
const uses = {};

// Three times in four the walk prefers the families it used least in the step, so that it
// reaches new pages instead of pressing the first button again; the fourth time any family may
// come again (a repeated click is a user too). Then rendezvous hashing, for the family and then
// the control: the one with the smallest hash of (stream, its key) wins, and it wins as long as
// it is on the page, whatever else is. A control that would not take the action (`ok`, Playwright's trial of it: hidden, covered, disabled) gives way to the next in the
// same order, at most TRIES of them; when none takes it, the result is null and the walk presses
// Escape, as a user does in a dialog that does not answer (Milestone 267: the unnamed buttons of
// Memos's "Link memo" dialog never took a click, and held a walk for 8 actions in a row).
const TRIES = 4;
async function choose(list, r, s, ok) {
  let pool = list;
  if (r.below(4) > 0) {
    const least = Math.min(...list.map((c) => uses[family(c)] || 0));
    pool = list.filter((c) => (uses[family(c)] || 0) === least);
  }
  const fams = [...new Set(pool.map(family))]
    .map((f) => [mix(s ^ fnv(f)), f])
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
    .map(([, f]) => f);
  let first;
  let chosen;
  let tries = 0;
  for (const fam of fams) {
    for (const c of ranked(pool.filter((x) => family(x) === fam), mix(s))) {
      first ??= c;
      if (!ok || tries >= TRIES) break;
      tries++;
      if (await ok(c)) {
        chosen = c;
        break;
      }
    }
    if (chosen || !ok || tries >= TRIES) break;
  }
  const c = ok ? chosen : first;
  if (c) uses[family(c)] = (uses[family(c)] || 0) + 1;
  return c;
}

// The controls by rendezvous hashing, the winner first.
function ranked(list, s) {
  return list
    .map((c) => [mix(s ^ fnv(`${c.role}\u0000${c.key}\u0000${c.km}`)), c])
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
    .map(([, c]) => c);
}

function pick(list, s) {
  let best;
  let bestH;
  for (const c of list) {
    const h = mix(s ^ fnv(`${c.role}\u0000${c.key}\u0000${c.km}`));
    if (bestH === undefined || h < bestH) {
      best = c;
      bestH = h;
    }
  }
  return best;
}

// The front of the page (Milestone 267): the controls of the overlay that opened last (a menu, a
// dialog, a list of options), which a user acts on next. Memos's share link sits behind Share,
// Manage share links, Create new link and its copy button, and an emoji behind Add reaction:
// each opens a few controls on a page of dozens, and a choice among all of them took the four in
// a row in 1 of 40 walks. Within the overlay, the controls that the action before made appear
// come first (the row of the share link that Create new link added). The front is read from the
// page alone. A front of the controls new on the whole page was tried and left out: it depends
// on two pages, and without its clock step Memos's seed 46 then took 38 of 72 actions otherwise.
const sig = (c) => `${c.role}\u0000${c.key}`;
// the controls of the snapshot before, in this step
let before = null;
function front(all) {
  const top = Math.max(0, ...all.flatMap((c) => c.over));
  if (top === 0) return [];
  const fresh = before ? all.filter((c) => !before.has(sig(c))) : [];
  const inside = all.filter((c) => c.over.includes(top));
  const newer = inside.filter((c) => fresh.includes(c));
  return newer.length && newer.length < inside.length ? newer : inside;
}

// Whether a text field takes text: Memos shows each memo's text as a read-only textbox, and a
// fill of it waited 3 s and did nothing (an eighth of Memos's actions before Milestone 267).
async function editable(page, list) {
  const out = [];
  for (const c of list) {
    if (await locate(page, c).isEditable({ timeout: 1000 }).catch(() => false)) out.push(c);
  }
  return out;
}

// ------------------------------------------------------------------ values to type
const TEXT = [
  'hydo',
  'Hello from hydo',
  '',
  ' ',
  'a'.repeat(300),
  'Ünïcödé ✓ 日本語 🙂',
  '<b>bold</b> & "quotes"',
  "it's",
  '0',
  '-1',
  '12.50',
  '#tag',
  '**bold** and a list:\n- one\n- two',
];
const BY_TYPE = {
  email: ['hydo@example.com', 'not-an-email', ''],
  number: ['0', '1', '-1', '999999999', '1.5', ''],
  password: ['hydo-pass-1', ''],
  date: ['2026-01-01', '1970-01-01', '2099-12-31'],
  'datetime-local': ['2026-01-01T00:00', '1970-01-01T00:00', '2099-12-31T23:59'],
  time: ['00:00', '12:30', '23:59'],
  url: ['https://example.com/', 'not a url', ''],
  tel: ['+1 555 0100', '0', ''],
  color: ['#000000', '#ff0000'],
};
const watched = (cfg.text || []).map((t) => t.toLowerCase());
function valueFor(type, r) {
  const pool = (BY_TYPE[type] || [...TEXT, ...(cfg.values || [])]).filter((v) => !watched.some((w) => v.toLowerCase().includes(w)));
  return pool[r.below(pool.length)];
}

// ------------------------------------------------------------------ the actions
const KINDS = [
  ['click', 12],
  ['fill', 6],
  ['commit', 4],
  ['submit', 2],
  ['double', 2],
  ['select', 2],
  ['back', 1],
  ['reload', 1],
  ['home', 1],
  ['wait', 1],
];
const T = { timeout: 3000 };
// the largest video or trace that the controller copies out (hydo-control's screens::FILE_MAX,
// Milestone 334: 640 KiB before, when a run copied at most 1 MiB of files in all)
const RECORDING_MAX = 3 * 1024 * 1024;
// the names of controls that only close an overlay
const DISMISS = /^(close|cancel|dismiss|skip|not now|no thanks|×|✕)$/i;
// the names of buttons that commit what a form holds
const COMMIT = /^(save|submit|create|send|post|update|confirm|ok|done|apply|publish|place)\b/i;

// No control of the chosen ones takes a click: press Escape, which closes a menu or a dialog
async function escape(page) {
  const lines = [];
  const act = { do: 'escape' };
  let what = 'press Escape (no control took the click)';
  try {
    await page.keyboard.press('Escape');
    lines.push("await page.keyboard.press('Escape');");
  } catch (e) {
    const why = String((e && e.message) || e).split('\n')[0].slice(0, 120);
    what += `: not done (${why})`;
    lines.push(`// not done in hydo's run: ${why}`);
  }
  await page.waitForTimeout(settle);
  lines.push(`await page.waitForTimeout(${settle});`);
  return { what, lines, act };
}

// A control as the script of a recording replay names it (Milestone 270): its role and accessible
// name, its place among the controls of that role and name (m) or of that role (j, without a
// name), and the name without its times with its place (key, km)
const ctl = (c) => ({ role: c.role, name: c.name, m: c.m, j: c.j, key: c.key, km: c.km });

async function act(page, k) {
  const r = new Stream(streamOf(k));
  let snap = '';
  try {
    snap = await page.locator('body').ariaSnapshot({ timeout: 5000 });
  } catch {
    snap = '';
  }
  const seen = controls(snap).filter(allowed);
  const ahead = front(seen);
  before = new Set(seen.map(sig));
  const ks = r.next();
  // seven times in eight the action stays in the overlay, when it has a control to act on (an
  // action elsewhere closes it); the draw comes from the action's own stream. There the walk does
  // not leave the page (back, reload, the start page): a user finishes what opened first
  let all = seen;
  let atFront = false;
  if (ahead.length && mix(ks ^ fnv('\u0000front')) % 8n > 0n) {
    const fe = await editable(page, ahead.filter((c) => FILL.has(c.role)));
    if (fe.length || ahead.some((c) => CLICK.has(c.role) || c.role === 'combobox')) {
      all = ahead;
      atFront = true;
    }
  }
  // In an overlay, three times in four the walk leaves out the controls that only close it
  // (Close, Cancel): a dialog's purpose is behind its other controls. Memos's Manage share links
  // dialog has Close and Create new link, and 3 of 4 walks that opened it closed it at once.
  if (atFront && mix(ks ^ fnv('\u0000stay')) % 4n > 0n) {
    const stay = all.filter((c) => !DISMISS.test(c.name.trim()));
    if (stay.some((c) => CLICK.has(c.role) || c.role === 'combobox' || FILL.has(c.role))) all = stay;
  }
  const clickable = all.filter((c) => CLICK.has(c.role));
  const fillable = await editable(page, all.filter((c) => FILL.has(c.role)));
  const have = {
    click: clickable.length > 0,
    fill: fillable.length > 0,
    commit: fillable.length > 0,
    submit: fillable.length > 0,
    double: all.some((c) => c.role === 'button'),
    select: all.some((c) => c.role === 'combobox'),
    back: !atFront,
    reload: !atFront,
    home: !atFront,
    wait: true,
  };
  // whether a control takes a click: Playwright's trial of it, without the click
  const clicks = (c) => locate(page, c).click({ trial: true, timeout: 500 }).then(() => true, () => false);
  // The kind by weighted rendezvous hashing (Milestone 264): each kind the page allows scores
  // -ln(u)/weight, with u from the action's stream and the kind's name, and the lowest wins. So a
  // kind that the page allows or not (a select on one page and not on the next) changes the
  // choice only when it wins; a roll over the sum of the weights moved every other kind with it,
  // and a clock step that changed the page changed the rest of the walk.
  const kinds = KINDS.filter(([kind]) => have[kind]);
  let kind = kinds[0][0];
  let best;
  for (const [kd, w] of kinds) {
    const u = (Number(mix(ks ^ fnv(kd)) >> 11n) + 0.5) / 2 ** 53;
    const score = -Math.log(u) / w;
    if (best === undefined || score < best) {
      best = score;
      kind = kd;
    }
  }
  const s2 = r.next();
  const lines = [];
  let what;
  // the action as the script of a recording replay reads it (Milestone 270)
  const done = { do: kind };
  try {
    if (kind === 'click' || kind === 'double') {
      const list = kind === 'double' ? all.filter((c) => c.role === 'button') : clickable;
      const c = await choose(list, r, s2, clicks);
      if (!c) return escape(page);
      done.c = ctl(c);
      const loc = locatorCode(c);
      what = `${kind === 'double' ? 'click twice' : 'click'} ${c.role} ${q(c.name)}`;
      const el = locate(page, c);
      if (kind === 'double') {
        // two clicks as fast as a user's double click on a submit button: the second may find the
        // button gone or disabled, which is the page handling it
        await el.click(T);
        await el.click({ timeout: 300 }).catch(() => {});
        lines.push(`await ${loc}.click();`, `await ${loc}.click({ timeout: 300 }).catch(() => {});`);
      } else {
        await el.click(T);
        lines.push(`await ${loc}.click();`);
      }
    } else if (kind === 'fill' || kind === 'submit' || kind === 'commit') {
      const c = await choose(fillable, r, s2);
      done.c = ctl(c);
      const el = c.name ? page.getByRole(c.role, { name: c.name, exact: true }).nth(c.m) : page.getByRole(c.role).nth(c.j);
      const type = await el.evaluate((e) => (e.tagName === 'INPUT' ? e.type : e.tagName.toLowerCase()), null, T);
      const v = valueFor(type, r);
      done.value = v;
      const loc = locatorCode(c);
      await el.fill(v, T);
      lines.push(`await ${loc}.fill(${q(v)});`);
      what = `fill ${c.role} ${q(c.name)} with ${q(v.length > 40 ? `${v.slice(0, 40)}…` : v)}`;
      if (kind === 'submit') {
        await el.press('Enter', T);
        lines.push(`await ${loc}.press('Enter');`);
        what += ' and press Enter';
      } else if (kind === 'commit') {
        // fill, then the button that commits it (Save, Submit, Create, Add, ...), read from the
        // page as it is after the fill: a form often enables its button only then
        await page.waitForTimeout(100);
        let after = '';
        try {
          after = await page.locator('body').ariaSnapshot({ timeout: 5000 });
        } catch {
          after = '';
        }
        const commits = controls(after)
          .filter(allowed)
          .filter((x) => x.role === 'button' && COMMIT.test(x.name));
        if (commits.length) {
          const b = pick(commits, r.next());
          done.then = ctl(b);
          const bl = locatorCode(b);
          const be = b.name ? page.getByRole(b.role, { name: b.name, exact: true }).nth(b.m) : page.getByRole(b.role).nth(b.j);
          lines.push('await page.waitForTimeout(100);', `await ${bl}.click();`);
          what += ` and click ${b.role} ${q(b.name)}`;
          await be.click(T);
        }
      }
    } else if (kind === 'select') {
      const c = await choose(all.filter((x) => x.role === 'combobox'), r, s2, clicks);
      if (!c) return escape(page);
      done.c = ctl(c);
      const el = locate(page, c);
      const n = await el.evaluate((e) => (e.tagName === 'SELECT' ? e.options.length : 0), null, T);
      const loc = locatorCode(c);
      if (n > 0) {
        const i = r.below(n);
        done.index = i;
        await el.selectOption({ index: i }, T);
        lines.push(`await ${loc}.selectOption({ index: ${i} });`);
        what = `choose option ${i} of ${c.role} ${q(c.name)}`;
      } else {
        done.do = 'click';
        await el.click(T);
        lines.push(`await ${loc}.click();`);
        what = `click ${c.role} ${q(c.name)}`;
      }
    } else if (kind === 'back') {
      what = 'go back';
      await page.goBack({ timeout: 10000 });
      lines.push('await page.goBack();');
    } else if (kind === 'reload') {
      what = 'reload the page';
      await page.reload({ timeout: 10000 });
      lines.push('await page.reload();');
    } else if (kind === 'home') {
      what = `open ${start.pathname}`;
      await page.goto(start.href, { timeout: 10000 });
      lines.push(`await page.goto(${q(start.pathname + start.search)});`);
    } else {
      const ms = [1000, 2000, 5000][r.below(3)];
      done.ms = ms;
      what = `wait ${ms / 1000} s`;
      await page.waitForTimeout(ms);
      lines.push(`await page.waitForTimeout(${ms});`);
    }
  } catch (e) {
    // the control went away or never became ready: the action is part of the walk all the same
    const why = String((e && e.message) || e).split('\n')[0].slice(0, 120);
    what = `${what || kind}: not done (${why})`;
    lines.push(`// not done in hydo's run: ${why}`);
    done.failed = true;
  }
  await page.waitForTimeout(settle);
  lines.push(`await page.waitForTimeout(${settle});`);
  return { what, lines, act: done };
}

// ------------------------------------------------------------------ the recording replay
// A walk with rec repeats the actions of the run it replays (Milestone 270). Recording costs the
// browser time, so the pages answer later, and a walk that chose its actions again from what the
// pages showed took other actions than the failing run: two of Memos's share-link walks passed
// when recorded (Milestone 267). The schedule `I=<walk>,rec[<step>]` means the actions that the
// same schedule without rec took. Each run prints them, one `walk-act <step>.<k> <json>` line per
// action, and the host writes those lines of the run without rec to SCRIPT before the workload
// starts. An action finds its control by the same role and accessible name, and the same place
// among the controls of that name; a control that is not there is left out with a line that says
// so, and nothing takes its place.
const SCRIPT = path.join(dir, 'walk-script.txt');
// how long a replayed action waits for its control to appear: a recorded page answers later
const FIND_MS = 10000;
function readScript() {
  if (!walk.rec) return null;
  let text;
  try {
    text = fs.readFileSync(SCRIPT, 'utf8');
  } catch {
    return null;
  }
  const out = new Map();
  for (const line of text.split('\n')) {
    const m = /^(\d+\.\d+) (\{.*\})$/.exec(line.trim());
    if (!m) continue;
    try {
      out.set(m[1], JSON.parse(m[2]));
    } catch {
      /* a line cut short: the action counts as not in the script */
    }
  }
  return out;
}
const script = readScript();

const ordinal = (i) => {
  const n = i + 1;
  const t = n % 100;
  const u = n % 10;
  return `${n}${t >= 11 && t <= 13 ? 'th' : u === 1 ? 'st' : u === 2 ? 'nd' : u === 3 ? 'rd' : 'th'}`;
};
const named = (c) => (c.name ? `${c.role} ${q(c.name)}${c.m ? ` (the ${ordinal(c.m)} of that name)` : ''}` : `the ${ordinal(c.j)} ${c.role} without a name`);

// The control of a scripted action: the same role, name and place, once it is visible. A name
// with a time in it ("1 minute ago") may read otherwise, because the clock moved on while the
// browser recorded; then the control with the same name without its times and the same place
// among them is the same control, as the walk's own choice counts them (Milestone 264).
async function findCtl(page, c) {
  try {
    await locate(page, c).waitFor({ state: 'visible', timeout: FIND_MS });
    return c;
  } catch {
    /* not there as it was named */
  }
  if (!c.name || timeless(c.name) === c.name) return null;
  let snap = '';
  try {
    snap = await page.locator('body').ariaSnapshot({ timeout: 5000 });
  } catch {
    return null;
  }
  return controls(snap).find((x) => x.role === c.role && x.key === c.key && x.km === c.km) || null;
}

const LEFT_OUT = `not on the page within ${FIND_MS / 1000} s; the run that this replays acted on it here, so the replay leaves the action out and takes no other control`;

async function replay(page, a) {
  const lines = [];
  const done = { ...a };
  delete done.failed;
  let what = a.do;
  const missing = (c, verb) => {
    done.missing = true;
    what = `${verb} ${named(c)}: ${LEFT_OUT}`;
    lines.push(`// left out in hydo's recording replay: ${named(c)} was not on the page`);
  };
  const renamed = (c, was) => (c.name !== was.name ? ` (named ${q(was.name)} in the run that this replays)` : '');
  try {
    if (a.do === 'click' || a.do === 'double') {
      const verb = a.do === 'double' ? 'click twice' : 'click';
      const c = await findCtl(page, a.c);
      if (!c) missing(a.c, verb);
      else {
        const loc = locatorCode(c);
        const el = locate(page, c);
        what = `${verb} ${c.role} ${q(c.name)}${renamed(c, a.c)}`;
        await el.click(T);
        lines.push(`await ${loc}.click();`);
        if (a.do === 'double') {
          await el.click({ timeout: 300 }).catch(() => {});
          lines.push(`await ${loc}.click({ timeout: 300 }).catch(() => {});`);
        }
      }
    } else if (a.do === 'fill' || a.do === 'submit' || a.do === 'commit') {
      const c = await findCtl(page, a.c);
      if (!c) missing(a.c, 'fill');
      else if (a.value === undefined) {
        what = `fill ${named(c)}: not done (the run that this replays typed no value)`;
      } else {
        const loc = locatorCode(c);
        const el = locate(page, c);
        const v = a.value;
        what = `fill ${c.role} ${q(c.name)}${renamed(c, a.c)} with ${q(v.length > 40 ? `${v.slice(0, 40)}…` : v)}`;
        await el.fill(v, T);
        lines.push(`await ${loc}.fill(${q(v)});`);
        if (a.do === 'submit') {
          await el.press('Enter', T);
          lines.push(`await ${loc}.press('Enter');`);
          what += ' and press Enter';
        } else if (a.do === 'commit') {
          await page.waitForTimeout(100);
          lines.push('await page.waitForTimeout(100);');
          if (a.then) {
            const b = await findCtl(page, a.then);
            if (!b) {
              done.missing = true;
              what += ` and click ${named(a.then)}: ${LEFT_OUT}`;
              lines.push(`// left out in hydo's recording replay: ${named(a.then)} was not on the page`);
            } else {
              lines.push(`await ${locatorCode(b)}.click();`);
              what += ` and click ${b.role} ${q(b.name)}`;
              await locate(page, b).click(T);
            }
          }
        }
      }
    } else if (a.do === 'select') {
      const c = await findCtl(page, a.c);
      if (!c) missing(a.c, 'choose an option of');
      else {
        await locate(page, c).selectOption({ index: a.index }, T);
        lines.push(`await ${locatorCode(c)}.selectOption({ index: ${a.index} });`);
        what = `choose option ${a.index} of ${c.role} ${q(c.name)}`;
      }
    } else if (a.do === 'escape') {
      what = 'press Escape';
      await page.keyboard.press('Escape');
      lines.push("await page.keyboard.press('Escape');");
    } else if (a.do === 'back') {
      what = 'go back';
      await page.goBack({ timeout: 10000 });
      lines.push('await page.goBack();');
    } else if (a.do === 'reload') {
      what = 'reload the page';
      await page.reload({ timeout: 10000 });
      lines.push('await page.reload();');
    } else if (a.do === 'home') {
      what = `open ${start.pathname}`;
      await page.goto(start.href, { timeout: 10000 });
      lines.push(`await page.goto(${q(start.pathname + start.search)});`);
    } else if (a.do === 'wait') {
      what = `wait ${a.ms / 1000} s`;
      await page.waitForTimeout(a.ms);
      lines.push(`await page.waitForTimeout(${a.ms});`);
    } else {
      done.missing = true;
      what = `${a.do}: an action this walker does not know; the replay leaves it out`;
    }
  } catch (e) {
    const why = String((e && e.message) || e).split('\n')[0].slice(0, 120);
    what = `${what}: not done (${why})${a.failed ? ', as in the run that this replays' : ''}`;
    lines.push(`// not done in hydo's recording replay: ${why}`);
    done.failed = true;
  }
  if (a.failed && !done.failed && !done.missing) what += ' (the run that this replays could not do it)';
  await page.waitForTimeout(settle);
  lines.push(`await page.waitForTimeout(${settle});`);
  return { what, lines, act: done };
}

// ------------------------------------------------------------------ signing in again
// Milestone 390: a session that ends during the walk. Before, `setup` ran only in a step without
// a saved session, so a walk whose session ended (Vikunja's seed 9 under disk errors) stayed on
// the sign-in page for the rest of the run. With `sign_in`, a page whose path `signed_out`
// matches, at a step's start or after an action, runs those steps, at most three times a step.
// A sign-in is part of the walk: it takes no action's place, and a recording replay signs in
// where its page asks.
let signIns = 0;
// whether the page is the sign-in page. An app that finds its session ended sends a page that
// just opened there after its first requests, so on a page that just opened the requests end
// first (at most 3 s); after an action the wait after it (settle_ms) has passed.
async function askedToSignIn(page, opened) {
  if (opened) await page.waitForLoadState('networkidle', { timeout: 3000 }).catch(() => {});
  try {
    return signedOut.test(new URL(page.url()).pathname);
  } catch {
    return false;
  }
}
async function signInAgain(page, at) {
  if (!signIn || signIns >= 3) return;
  if (!(await askedToSignIn(page, at === 'start'))) return;
  const p = new URL(page.url()).pathname;
  signIns++;
  const where = typeof at === 'number' ? `after ${step}.${at}` : `at the start of step ${step}`;
  code.push(`// hydo signed in again here (${where}): the page ${p} asked for it`);
  let what;
  try {
    await setup(page, signIn);
    what = `sign in again: the page ${p} asked for it ${where}`;
  } catch (e) {
    what = `sign in again: the page ${p} asked for it ${where}; not done (${String((e && e.message) || e).split('\n')[0].slice(0, 120)})`;
  }
  log(`walk ${step}: ${what}`);
  await frame(page, `sign-in-${signIns}`, what);
}

// ------------------------------------------------------------------ setup steps (sign in)
// Each is one of {open: <path>}, {click: <name>[, role]}, {fill: <label>, with: <value>},
// {press: <key>}, {see: <text>}, {wait: <ms>}.
async function setup(page, steps = cfg.setup || []) {
  for (const s of steps) {
    if (s.open !== undefined) {
      await page.goto(new URL(s.open, start).href, { timeout: 30000 });
      code.push(`await page.goto(${q(s.open)});`);
    } else if (s.click !== undefined) {
      const loc = s.role
        ? page.getByRole(s.role, { name: s.click, exact: true })
        : page.getByRole('button', { name: s.click, exact: true }).or(page.getByRole('link', { name: s.click, exact: true }));
      await loc.first().click({ timeout: 10000 });
      code.push(
        s.role
          ? `await page.getByRole(${q(s.role)}, { name: ${q(s.click)}, exact: true }).first().click();`
          : `await page.getByRole('button', { name: ${q(s.click)}, exact: true }).or(page.getByRole('link', { name: ${q(s.click)}, exact: true })).first().click();`,
      );
    } else if (s.fill !== undefined) {
      await page.getByLabel(s.fill, { exact: true }).or(page.getByPlaceholder(s.fill, { exact: true })).first().fill(String(s.with ?? ''), { timeout: 10000 });
      code.push(`await page.getByLabel(${q(s.fill)}, { exact: true }).or(page.getByPlaceholder(${q(s.fill)}, { exact: true })).first().fill(${q(String(s.with ?? ''))});`);
    } else if (s.press !== undefined) {
      await page.keyboard.press(s.press);
      code.push(`await page.keyboard.press(${q(s.press)});`);
    } else if (s.see !== undefined) {
      await page.getByText(s.see).first().waitFor({ timeout: 15000 });
      code.push(`await page.getByText(${q(s.see)}).first().waitFor();`);
    } else if (s.wait !== undefined) {
      await page.waitForTimeout(Number(s.wait));
      code.push(`await page.waitForTimeout(${Number(s.wait)});`);
    }
  }
}

// ------------------------------------------------------------------ repro.spec.ts
// The H= tokens of the schedule that act on a fake's route map to page.route; every other fault
// becomes a comment at the step where hydo injected it.
function faultsOf(text) {
  const out = [];
  for (const tok of text.split(/\s+/)) {
    const m = /^([A-Z])=(.*)$/.exec(tok);
    if (!m || m[1] === 'I') continue;
    // the step of the fault: <target>@<at>..., or T=<at>:... and C=<at>
    const at = /@(\d+)/.exec(m[2]) || /^(\d+)(:|$)/.exec(m[2]);
    out.push({ tok, letter: m[1], value: m[2], at: at ? Number(at[1]) : 0 });
  }
  return out;
}
function routeOf(f) {
  // H=<fake>.<route>@<from>-<to>:<kind>[=<v>]
  const m = /^([a-z][a-z0-9_]*)(?:\.([a-z][a-z0-9_]*))?@(\d+)-(\d+):(.*)$/.exec(f.value);
  if (!m) return null;
  const r = m[2] ? (cfg.routes || {})[`${m[1]}.${m[2]}`] : { path: '/', method: '' };
  if (!r) return null;
  const glob = `**${r.path.replace(/\{[^}]*\}/g, '*')}${m[2] ? '' : '**'}`;
  const how = m[5];
  const method = r.method ? `if (route.request().method() !== ${q(r.method)}) return route.fallback(); ` : '';
  let body;
  const kv = Object.fromEntries(how.split(',').map((p) => p.split('=')));
  // effect=yes: the server acts before the fault, as hydo's fake does
  const acted = kv.effect === 'yes' ? 'route.fetch().then(() => ' : '(';
  if ('status' in kv) body = `${acted}route.fulfill({ status: ${Number(kv.status)} }))`;
  else if ('delay' in kv) body = `new Promise((r) => setTimeout(r, ${Number(kv.delay)})).then(() => route.continue())`;
  else if ('drop' in kv) body = 'route.fetch().then(() => new Promise(() => {}))'; // the server acts, the answer never comes
  else if ('reset' in kv) body = `${acted}route.abort('connectionreset'))`;
  else return null;
  // p=<percent>: hydo's fake chose which requests fail, by the schedule's own stream; here every
  // matching request fails
  const note = kv.p ? ` (hydo failed ${kv.p} % of the requests; this fails all of them)` : '';
  return { from: Number(m[3]), to: Number(m[4]), glob, note, handler: `async (route) => { ${method}return ${body}; }` };
}
function reproSpec(steps) {
  const faults = faultsOf(guest);
  const out = [];
  out.push(`// The walk that hydo made${guest ? `, schedule: ${guest}` : ''}.`);
  out.push('// Run it against your own instance of the app, without hydo:');
  out.push(`//   BASE_URL=${start.origin} npx playwright test repro.spec.ts`);
  out.push("// Faults on the page's own requests are page.route handlers; a fault of the backend is a");
  out.push('// comment at the step where hydo injected it: do the same to your instance there.');
  out.push("import { expect, test } from '@playwright/test';");
  out.push('');
  out.push(`test.use({ baseURL: process.env.BASE_URL || ${q(start.origin)} });`);
  out.push(`test.setTimeout(${(steps.length + 1) * 120000});`);
  out.push('');
  out.push(`test(${q(`hydo walk ${process.argv[3] || '0'}`)}, async ({ page }) => {`);
  out.push('  const uncaught = [];');
  out.push("  page.on('pageerror', (e) => uncaught.push(e.message));");
  out.push("  page.on('console', (m) => m.type() === 'error' && console.log(`console.error: ${m.text()}`));");
  for (const [s, st] of steps.entries()) {
    out.push(`  // step ${s}`);
    for (const f of faults) {
      if (f.letter === 'H') {
        const r = routeOf(f);
        if (r && r.from === s) out.push(`  await page.route(${q(r.glob)}, ${r.handler}); // ${f.tok}${r.note}`);
        else if (r && r.to === s) out.push(`  await page.unroute(${q(r.glob)}); // the end of ${f.tok}`);
        else if (!r && f.at === s) out.push(`  // hydo: ${f.tok} (a fault of an outside service) acts from here`);
      } else if (f.at === s) {
        out.push(`  // hydo: ${f.tok} acts from here (hydo guide faults explains the token)`);
      }
    }
    for (const l of st) out.push(`  ${l}`);
  }
  out.push("  // hydo's watcher fails a run on an uncaught exception or an unhandled rejection; so does this test");
  out.push("  expect(uncaught, 'uncaught exceptions and unhandled rejections in the page').toEqual([]);");
  out.push('});');
  return out.join('\n') + '\n';
}

// ------------------------------------------------------------------ one step of the walk
// explore.server: the app's web server, started in this service for the step (as Playwright's
// webServer), and stopped at the step's end
async function startServer() {
  if (!cfg.server) return null;
  const out = fs.openSync(path.join(own, `server-${step}.log`), 'a');
  const child = spawn('/bin/sh', ['-c', cfg.server], { stdio: ['ignore', out, out], detached: true });
  for (let i = 0; i < 300; i++) {
    try {
      await fetch(start.href, { signal: AbortSignal.timeout(1000) });
      return child;
    } catch {
      await new Promise((r) => setTimeout(r, 200));
    }
  }
  log(`walk ${step}: the web server of explore.server did not answer at ${start.href} within 60 s`);
  return child;
}

async function main() {
  const t0 = Date.now();
  const server = await startServer();
  const statePath = path.join(own, 'state.json');
  let state = null;
  try {
    state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
  } catch {
    state = null;
  }
  const browser = await chromium.launch({ headless: true });
  const opts = { viewport: { width: 1280, height: 720 }, locale: 'en-US', timezoneId: 'UTC' };
  if (state && state.storage) opts.storageState = state.storage;
  // the controller copies these files out through the agent, about 1.5 KB per call, after the
  // verdict: with rec, a video of the last step that takes an action, where a walk's failure
  // shows most often (Milestone 264: a shrunk walk leaves the steps after its failure without
  // actions). Since Milestone 334 every step records a trace (DOM snapshots, no screenshots),
  // which the controller copies only when the run fails; with rec the recorded step's trace is
  // copied always, as before.
  const nsteps = cfg.steps || 1;
  const acts = (s) => Array.from({ length: actions }, (_, k) => k).some((k) => !walk.skip.has(`${s}.${k}`));
  let lastActive = nsteps - 1;
  while (lastActive > 0 && !acts(lastActive)) lastActive--;
  // rec<s> names the step to record (Milestone 267: the step whose failure the replay shows)
  const recording = walk.rec && step === (walk.recStep ?? lastActive);
  if (recording) opts.recordVideo = { dir: path.join(own, 'video'), size: { width: 640, height: 360 } };
  const context = await browser.newContext(opts);
  await context.tracing.start({ screenshots: false, snapshots: true, title });
  eventsSoFar();
  // What the server acknowledged: every write, answered 2xx. A write is a request under
  // watch.requests whose path matches watch.failures_of, or without that key every request that
  // is not a GET. Each goes to walk/acks.jsonl with the printable text of the request and the
  // answer. $HYDO_ACKED gets one word per resource name in them, `<the path's last part>:<name>`
  // (such as CreateMemo:memos/T5k4x8o5), or the path's last part alone: the file is a list of
  // words, and a power cut keeps it for the recovery boot. A project's own checks read them.
  // Since Milestone 273 each write also adds `<METHOD>:<path>=<id>` for every id of the objects
  // that a JSON answer returns (its own `id`, and the `id` of each object in a field or a list of
  // it), or `<METHOD>:<path>` without one: a REST API names a new object by a number, as Vikunja's
  // `POST /api/v2/projects/1/tasks/bulk` answers {"tasks":[{"id":6,...}]}. watch.failures_of
  // matches the path or `<METHOD> <path>` (an API whose reads and writes share a path).
  const writes = watch.failures_of ? new RegExp(watch.failures_of) : null;
  const isWrite = (method, p) => (writes ? writes.test(p) || writes.test(`${method} ${p}`) : method !== 'GET');
  const idsOf = (text) => {
    let v;
    try {
      v = JSON.parse(text);
    } catch {
      return [];
    }
    const out = [];
    const take = (o) => {
      if (o && typeof o === 'object' && !Array.isArray(o) && (typeof o.id === 'number' || (typeof o.id === 'string' && /^[A-Za-z0-9_-]+$/.test(o.id)))) out.push(String(o.id));
    };
    if (Array.isArray(v)) v.forEach(take);
    else if (v && typeof v === 'object') {
      take(v);
      if (out.length === 0) for (const f of Object.values(v)) Array.isArray(f) ? f.forEach(take) : take(f);
    }
    return [...new Set(out)].slice(0, 20);
  };
  context.on('response', async (res) => {
    const req = res.request();
    const t = req.resourceType();
    if ((t !== 'fetch' && t !== 'xhr') || res.status() < 200 || res.status() > 299) return;
    let p;
    try {
      p = new URL(req.url()).pathname;
    } catch {
      return;
    }
    if (watch.requests && !p.startsWith(watch.requests)) return;
    if (!isWrite(req.method(), p)) return;
    const printable = (b) =>
      (b || '')
        .toString('latin1')
        .replace(/[^\x20-\x7e]+/g, ' ')
        .trim()
        .slice(0, 600);
    let answer = '';
    let ids = [];
    try {
      const body = await res.body();
      answer = printable(body);
      ids = idsOf(body.toString('utf8'));
    } catch {
      answer = '';
    }
    const sent = printable(req.postDataBuffer());
    const names = [...new Set(`${sent} ${answer}`.match(/\b[a-z][a-z0-9_-]*\/[A-Za-z0-9_-]{6,}\b/g) || [])].slice(0, 6);
    fs.appendFileSync(path.join(own, 'acks.jsonl'), JSON.stringify({ t: Date.now(), step, method: req.method(), path: p, status: res.status(), names, ids, sent, answer }) + '\n');
    if (ackedFile) {
      try {
        const last = p.split('/').filter(Boolean).pop() || p;
        const words = names.length ? names.map((n) => `${last}:${n}`) : [last];
        const at = `${req.method()}:${p.replace(/\s+/g, '')}`;
        words.push(...(ids.length ? ids.map((id) => `${at}=${id}`) : [at]));
        fs.appendFileSync(ackedFile, words.join('\n') + '\n');
      } catch {
        /* no acknowledged-work file in this guest */
      }
    }
  });
  const page = await context.newPage();
  let status = 'passed';
  let error = '';
  try {
    if (!state || !state.signedIn) {
      if ((cfg.setup || []).length) log(`walk ${step}: setup (${cfg.setup.length} steps)`);
      if (!(cfg.setup || []).some((s) => s.open !== undefined)) {
        await page.goto(start.href, { timeout: 30000 });
        code.push(`await page.goto(${q(start.pathname + start.search)});`);
      }
      await setup(page);
    } else {
      await page.goto(state.url, { timeout: 30000 });
      code.push(`await page.goto(${q(new URL(state.url).pathname + new URL(state.url).search)});`);
    }
  } catch (e) {
    status = 'failed';
    error = `the walk could not open its page: ${String(e.message || e).split('\n')[0].slice(0, 200)}`;
    log(`walk ${step}: ${error}`);
    code.push(`// not done in hydo's run: ${error}`);
  }
  const signedIn = status === 'passed';
  await frame(page, 'start', signedIn ? `open ${shortPath(page.url())}` : error, signedIn ? {} : { failed: true });
  if (signedIn) await signInAgain(page, 'start');
  if (walk.rec && !script) {
    log(`walk ${step}: this recording has no script of the run it replays (${SCRIPT}), so the walk chooses its actions again and may take others`);
  }
  let missed = 0;
  for (let k = 0; k < actions; k++) {
    if (walk.skip.has(`${step}.${k}`)) continue;
    let done;
    if (script) {
      const a = script.get(`${step}.${k}`);
      if (!a) {
        missed++;
        log(`walk ${step}.${k}: not in the script: the run that this replays took no action ${step}.${k}, so the replay takes none`);
        code.push(`// left out in hydo's recording replay: the run that it replays took no action ${step}.${k}`);
        continue;
      }
      done = await replay(page, a);
      if (done.act.missing) missed++;
    } else {
      done = await act(page, k);
    }
    log(`walk ${step}.${k}: ${done.what}`);
    log(`walk-act ${step}.${k} ${JSON.stringify(done.act)}`);
    code.push(...done.lines);
    await frame(page, k, done.what, done.act.failed || done.act.missing ? { failed: true } : {});
    await signInAgain(page, k);
  }
  if (script) {
    log(`walk ${step}: the recording replay ${missed ? `left out ${missed} action${missed === 1 ? '' : 's'}` : 'repeated every action'} of the run it replays`);
  }
  await page.waitForTimeout(quiet);
  try {
    const url = page.url();
    if (url.startsWith('http')) {
      fs.writeFileSync(statePath, JSON.stringify({ url, signedIn: signedIn || !!(state && state.signedIn), storage: await context.storageState() }));
    }
  } catch {
    /* the page went away: the next step starts where this one started */
  }
  // A recording larger than this stays in the guest: the copy out of it must fit the run's copy
  // budget, and a trace's snapshots carry the app's scripts and styles (Memos's: megabytes).
  const keep = (file, why, extra) => {
    let size = 0;
    try {
      size = fs.statSync(path.join(shots, file)).size;
    } catch {
      return;
    }
    if (size > RECORDING_MAX) {
      log(`walk ${step}: ${file} has ${size} bytes, more than ${RECORDING_MAX}; it stays in the guest (${path.join(shots, file)})`);
      return;
    }
    indexLine(file, why, extra);
  };
  {
    const trace = `walk-step-${step}.trace.zip`;
    await context.tracing.stop({ path: path.join(shots, trace) }).catch(() => {});
    // only: failed (Milestone 334): the controller copies it when the run fails, or at a power cut
    keep(trace, `the Playwright trace of step ${step} of the walk (npx playwright show-trace)`, recording ? {} : { only: 'failed', step });
  }
  const video = page.video();
  await context.close();
  if (video) {
    const file = `walk-step-${step}.webm`;
    await video.saveAs(path.join(shots, file)).catch(() => {});
    keep(file, 'the video of the walk');
  }
  await browser.close();
  if (server) {
    try {
      process.kill(-server.pid, 'SIGTERM');
    } catch {
      /* gone already */
    }
  }
  // the walk so far as one Playwright test
  fs.writeFileSync(path.join(own, `repro-${step}.json`), JSON.stringify(code));
  const steps = [];
  for (let s = 0; s <= step; s++) {
    try {
      steps.push(JSON.parse(fs.readFileSync(path.join(own, `repro-${s}.json`), 'utf8')));
    } catch {
      steps.push(['// this step did not run']);
    }
  }
  fs.writeFileSync(path.join(shots, 'repro.spec.ts'), reproSpec(steps));
  indexLine('repro.spec.ts', 'the walk as a Playwright test');
  const ms = Date.now() - t0;
  event({ kind: 'test', test: title, file: 'walk', status, expected: 'passed', ms, error });
  log(`test "${title}" ${status} in ${(ms / 1000).toFixed(1)} s${error ? `: ${error}` : ''}`);
}

// ------------------------------------------------------------------ the read-back check
// `browser.read_back` (Milestone 334): the pages show what the server acknowledged. Each entry of
// cfg.read_back.objects names the writes it judges (a regular expression over `<METHOD> <path>`
// of an acknowledged write that returned an id), the page that lists such an object (a path,
// with {id} and the regular expression's named groups), how the page shows it (a Playwright
// selector, with the same places), and the writes after which it is gone on purpose (`unless`).
const ACKED_WORD = /^(GET|POST|PUT|PATCH|DELETE):(\S+?)(?:=([A-Za-z0-9_-]+))?$/;
const esc = (x) => String(x).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const fill = (tpl, vals, quote = (x) => x) => tpl.replace(/\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (m, k) => (k in vals ? quote(vals[k]) : m));
// the kind of object a write makes: the last word of its path that is not a number or a verb of
// the API (POST /api/v2/projects/1/tasks/bulk makes tasks), in the singular for the sentences
function kindOf(p) {
  const parts = p.split('/').filter((x) => x && !/^\d+$/.test(x) && !/^(bulk|batch|new|create|api|v\d+)$/i.test(x));
  return parts.pop() || '';
}
const singular = (k) => (k.endsWith('ies') ? `${k.slice(0, -3)}y` : k.endsWith('s') && !k.endsWith('ss') ? k.slice(0, -1) : k) || 'object';

function ackedWrites() {
  let text = '';
  try {
    text = fs.readFileSync(ackedFile, 'utf8');
  } catch {
    text = '';
  }
  const out = [];
  for (const w of text.split(/\s+/)) {
    const m = ACKED_WORD.exec(w);
    if (m) out.push({ method: m[1], path: m[2], id: m[3], line: `${m[1]} ${m[2]}` });
  }
  return out;
}

// The objects to look for: one per entry and id, the last write that returned it.
function wanted(rb, writes) {
  const objs = new Map();
  const goneBy = [];
  for (const [n, e] of (rb.objects || [{}]).entries()) {
    const re = new RegExp(e.writes || '^(POST|PUT) ');
    for (const w of writes) {
      if (!w.id) continue;
      const m = re.exec(w.line);
      if (!m) continue;
      const kind = kindOf(w.path);
      const vals = { ...(m.groups || {}), id: w.id };
      const unless = new RegExp(fill(e.unless || (kind ? `^DELETE \\S*/${esc(kind)}/{id}$` : '^DELETE \\S*/{id}$'), vals, esc));
      const gone = writes.find((x) => unless.test(x.line));
      const key = `${n}\u0000${w.id}`;
      if (gone) {
        objs.delete(key);
        goneBy.push(`${e.name || singular(kind)} ${w.id} (${gone.line})`);
        continue;
      }
      const shows = e.shows || (kind ? `a[href$="/${kind}/{id}"], a[href*="/${kind}/{id}/"]` : 'a[href$="/{id}"]');
      objs.set(key, {
        id: w.id,
        name: `${e.name || singular(kind)} ${w.id}`,
        write: w.line,
        page: fill(e.page || start.pathname + start.search, vals, encodeURIComponent),
        shows: fill(shows, vals),
      });
    }
  }
  return { objs: [...objs.values()], goneBy };
}

async function readBackMain() {
  const rb = cfg.read_back || {};
  const writes = ackedWrites();
  const { objs, goneBy } = wanted(rb, writes);
  const left = goneBy.length ? `; ${goneBy.length} left out because a later acknowledged write removed them, such as the ${goneBy[0]}` : '';
  // --list: what the check would look for, without a browser (hydo-manifest's tests)
  if (process.argv[4] === '--list') {
    console.log(JSON.stringify({ objects: objs, left_out: goneBy }));
    process.exit(0);
  }
  if (objs.length === 0) {
    console.log(`no acknowledged write that browser.read_back names returned an id (${writes.length} acknowledged writes in all), so no page had an object to show${left}`);
    process.exit(0);
  }
  const wait = rb.wait_ms ?? 10000;
  const server = await startServer();
  const browser = await chromium.launch({ headless: true });
  let state = null;
  try {
    state = JSON.parse(fs.readFileSync(path.join(own, 'state.json'), 'utf8'));
  } catch {
    state = null;
  }
  const opts = { viewport: { width: 1280, height: 720 }, locale: 'en-US', timezoneId: 'UTC' };
  if (state && state.storage) opts.storageState = state.storage;
  const context = await browser.newContext(opts);
  const page = await context.newPage();
  eventsSoFar();
  const stop = async (code) => {
    await browser.close().catch(() => {});
    if (server) {
      try {
        process.kill(-server.pid, 'SIGTERM');
      } catch {
        /* gone already */
      }
    }
    process.exit(code);
  };
  // the walk's session; in a recovery boot, or when the walk never signed in, the setup steps
  if (!state || !state.signedIn) {
    const steps = rb.setup || cfg.setup || [];
    try {
      if (!steps.some((x) => x.open !== undefined)) await page.goto(start.href, { timeout: 30000 });
      await setup(page, steps);
    } catch (e) {
      const why = String((e && e.message) || e).split('\n')[0].slice(0, 200);
      await frame(page, 'setup', `sign in again: ${why}`, { failed: true });
      console.log(
        `The pages could not be opened to look for the ${objs.length} objects that the server acknowledged, such as the ${objs[0].name} (${objs[0].write}): ` +
          `the setup steps of browser.read_back failed: ${why}`,
      );
      await stop(1);
    }
  }
  const byPage = new Map();
  for (const o of objs) {
    if (!byPage.has(o.page)) byPage.set(o.page, []);
    byPage.get(o.page).push(o);
  }
  const missing = [];
  let shot = 0;
  let n = 0;
  let signedAgain = false;
  for (const [p, list] of byPage) {
    let open = '';
    try {
      await page.goto(new URL(p, start).href, { timeout: 30000 });
      // Milestone 390: the walk's session ended (a clock step past the token's time): the page
      // asks to sign in, and the check signs in with its setup steps, or the walk's sign_in, once
      const again = rb.setup || cfg.sign_in;
      if (again && !signedAgain && (await askedToSignIn(page, true))) {
        signedAgain = true;
        await setup(page, again);
        await frame(page, 'sign-in', `sign in again: the walk's session had ended`);
        await page.goto(new URL(p, start).href, { timeout: 30000 });
      }
    } catch (e) {
      open = String((e && e.message) || e).split('\n')[0].slice(0, 160);
    }
    const t0 = Date.now();
    let notYet = list;
    while (!open) {
      const still = [];
      for (const o of notYet) {
        const c = await page.locator(o.shows).count().catch(() => 0);
        if (c === 0) still.push(o);
      }
      notYet = still;
      if (notYet.length === 0 || Date.now() - t0 >= wait) break;
      await page.waitForTimeout(500);
    }
    n++;
    await frame(page, n, `look on ${p} for ${list.length} acknowledged object${list.length === 1 ? '' : 's'}: ${notYet.length} not shown`, notYet.length ? { failed: true } : {});
    if (notYet.length) {
      let file = '';
      try {
        file = `read-back-${++shot}.png`;
        await page.screenshot({ path: path.join(shots, file) });
        indexLine(file, `the page ${p} without ${notYet.length === 1 ? notYet[0].name : `${notYet.length} acknowledged objects`}`);
      } catch {
        file = '';
      }
      for (const o of notYet) missing.push({ ...o, file, open, secs: ((Date.now() - t0) / 1000).toFixed(1) });
    }
  }
  if (missing.length === 0) {
    const pages = [...byPage.keys()];
    console.log(
      `every one of the ${objs.length} objects that the server acknowledged and browser.read_back names is shown, on ${pages.length === 1 ? `the page ${pages[0]}` : `${pages.length} pages`}${left}`,
    );
    await stop(0);
  }
  for (const m of missing.slice(1, 20)) console.log(`also not shown on ${m.page}: the ${m.name} (${m.write})`);
  const f = missing[0];
  const how = f.open ? `the page did not open (${f.open})` : `no element matches ${f.shows} after ${f.secs} s`;
  const more = missing.length > 1 ? `; ${missing.length} of ${objs.length} acknowledged objects are not shown` : '';
  console.log(
    `The page ${f.page} does not show the ${f.name} that the server acknowledged (${f.write} answered with its id): ${how}${more}.` +
      `${f.file ? ` Screenshot: screens/${f.file} in the run directory.` : ''}`,
  );
  await stop(1);
}

(readingBack ? readBackMain() : main()).catch((e) => {
  if (readingBack) {
    console.log(`browser.read_back could not look at the pages: ${String((e && e.message) || e).split('\n')[0].slice(0, 200)}`);
    process.exit(2);
  }
  log(`walk ${step}: the walker stopped: ${String((e && e.stack) || e).split('\n').slice(0, 3).join(' / ')}`);
  event({ kind: 'test', test: title, file: 'walk', status: 'failed', expected: 'passed', ms: 0, error: String(e && e.message) });
  process.exit(1);
});

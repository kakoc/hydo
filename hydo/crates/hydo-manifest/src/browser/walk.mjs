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
// control the walk takes.
//
// The watcher (hook.cjs, loaded with NODE_OPTIONS=--require) watches every page as it watches a
// team's Playwright tests, and check.mjs judges what it saw. This file also writes the walk as a
// Playwright test, repro.spec.ts, which the controller copies into the run directory's screens/.
// With rec, the last step that takes an action also leaves a video and a Playwright trace there.
//
// The configuration is $HYDO_WEB_WALK (JSON, written by the expansion of `browser.explore`):
//   start, actions, setup, avoid, within, values, settle_ms, quiet_ms, text, routes
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
const cfg = JSON.parse(process.env.HYDO_WEB_WALK || '{}');
const step = Number(process.argv[2] || 0);
const walk = parseWalk(process.argv[3] || '');
const guest = process.argv[4] || '';
const ackedFile = process.argv[5] || '';
const watch = JSON.parse(process.env.HYDO_WEB_WATCH || '{}');
const actions = walk.actions || cfg.actions || 10;
const settle = cfg.settle_ms ?? 700;
const quiet = cfg.quiet_ms ?? 3000;
const start = new URL(cfg.start || 'http://localhost:3000/');
const within = cfg.within || '/';
const avoid = (cfg.avoid || ['log ?out', 'sign ?out']).map((a) => new RegExp(a, 'i'));
const title = `walk step ${step}`;
globalThis.__hydoWalkTest = title; // the stub @playwright/test hands it to hook.cjs
const own = path.join(dir, 'walk');
const shots = path.join(dir, 'shots');
fs.mkdirSync(own, { recursive: true });
fs.mkdirSync(shots, { recursive: true });

function parseWalk(v) {
  const w = { seed: 0n, actions: 0, skip: new Set(), rec: false };
  if (!v) return w;
  const [seed, ...fields] = v.split(',');
  w.seed = BigInt(`0x${seed}`);
  for (const f of fields) {
    if (f === 'rec') w.rec = true;
    else if (f.startsWith('n')) w.actions = Number(f.slice(1));
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
function indexLine(file, why) {
  const index = path.join(shots, 'index.jsonl');
  let have = '';
  try {
    have = fs.readFileSync(index, 'utf8');
  } catch {
    have = '';
  }
  if (!have.split('\n').some((l) => l.includes(`"file":${JSON.stringify(file)}`))) {
    fs.appendFileSync(index, JSON.stringify({ file, test: title, url: '', why }) + '\n');
  }
}

// The walk as Playwright code, for repro.spec.ts: each step's lines in walk/repro-<step>.json.
const code = [];
const q = (s) => JSON.stringify(s);
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
  /\b(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]* \d{1,2}(?:, \d{4})?\b/gi,
  /\b\d{1,2}:\d{2}(?::\d{2})?(?: ?[ap]\.?m\.?)?(?![\w])/gi,
];
function timeless(name) {
  let n = name;
  for (const r of TIMES) n = n.replace(r, '<time>');
  return n.replace(/\bin <time>/gi, '<time>');
}

// The controls of an ariaSnapshot, in document order: {role, name, m (place among the same role
// and name), j (place among the same role), key and km (the name without its times, and the
// place among the same role and key), url, disabled}.
function controls(snapshot) {
  const out = [];
  const byKey = new Map();
  const byName = new Map();
  const byRole = new Map();
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
    const c = { role, name, m: n, j, key: tk, km, url: undefined, disabled: /\[disabled/.test(m[4] || '') };
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
// How often the walk acted on each family, carried from step to step.
let uses = {};

// Three times in four the walk prefers the families it used least, so that it reaches new pages
// instead of pressing the first button again; the fourth time any family may come again (a
// repeated click is a user too). Then rendezvous hashing, for the family and then the control:
// the one with the smallest hash of (stream, its key) wins, and it wins as long as it is on the
// page, whatever else is.
function choose(list, r, s) {
  let pool = list;
  if (r.below(4) > 0) {
    const least = Math.min(...list.map((c) => uses[family(c)] || 0));
    pool = list.filter((c) => (uses[family(c)] || 0) === least);
  }
  let fam;
  let famH;
  for (const c of pool) {
    const h = mix(s ^ fnv(family(c)));
    if (famH === undefined || h < famH) {
      fam = family(c);
      famH = h;
    }
  }
  const c = pick(
    pool.filter((x) => family(x) === fam),
    mix(s),
  );
  uses[fam] = (uses[fam] || 0) + 1;
  return c;
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
// the largest video or trace that the controller copies out. The controller copies at most 1 MiB
// of files per run (hydo-control's screens::MAX_BYTES, Milestone 264): the test, a trace and a
// video of this size fit
const RECORDING_MAX = 640 * 1024;
// the names of buttons that commit what a form holds
const COMMIT = /^(save|submit|create|send|post|update|confirm|ok|done|apply|publish|place)\b/i;

async function act(page, k) {
  const r = new Stream(streamOf(k));
  let snap = '';
  try {
    snap = await page.locator('body').ariaSnapshot({ timeout: 5000 });
  } catch {
    snap = '';
  }
  const all = controls(snap).filter(allowed);
  const clickable = all.filter((c) => CLICK.has(c.role));
  const fillable = all.filter((c) => FILL.has(c.role));
  const have = {
    click: clickable.length > 0,
    fill: fillable.length > 0,
    commit: fillable.length > 0,
    submit: fillable.length > 0,
    double: all.some((c) => c.role === 'button'),
    select: all.some((c) => c.role === 'combobox'),
    back: true,
    reload: true,
    home: true,
    wait: true,
  };
  // The kind by weighted rendezvous hashing (Milestone 264): each kind the page allows scores
  // -ln(u)/weight, with u from the action's stream and the kind's name, and the lowest wins. So a
  // kind that the page allows or not (a select on one page and not on the next) changes the
  // choice only when it wins; a roll over the sum of the weights moved every other kind with it,
  // and a clock step that changed the page changed the rest of the walk.
  const kinds = KINDS.filter(([kind]) => have[kind]);
  const ks = r.next();
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
  try {
    if (kind === 'click' || kind === 'double') {
      const list = kind === 'double' ? all.filter((c) => c.role === 'button') : clickable;
      const c = choose(list, r, s2);
      const loc = locatorCode(c);
      what = `${kind === 'double' ? 'click twice' : 'click'} ${c.role} ${q(c.name)}`;
      const el = c.name ? page.getByRole(c.role, { name: c.name, exact: true }).nth(c.m) : page.getByRole(c.role).nth(c.j);
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
      const c = choose(fillable, r, s2);
      const el = c.name ? page.getByRole(c.role, { name: c.name, exact: true }).nth(c.m) : page.getByRole(c.role).nth(c.j);
      const type = await el.evaluate((e) => (e.tagName === 'INPUT' ? e.type : e.tagName.toLowerCase()), null, T);
      const v = valueFor(type, r);
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
          const bl = locatorCode(b);
          const be = b.name ? page.getByRole(b.role, { name: b.name, exact: true }).nth(b.m) : page.getByRole(b.role).nth(b.j);
          lines.push('await page.waitForTimeout(100);', `await ${bl}.click();`);
          what += ` and click ${b.role} ${q(b.name)}`;
          await be.click(T);
        }
      }
    } else if (kind === 'select') {
      const c = choose(all.filter((x) => x.role === 'combobox'), r, s2);
      const el = c.name ? page.getByRole(c.role, { name: c.name, exact: true }).nth(c.m) : page.getByRole(c.role).nth(c.j);
      const n = await el.evaluate((e) => (e.tagName === 'SELECT' ? e.options.length : 0), null, T);
      const loc = locatorCode(c);
      if (n > 0) {
        const i = r.below(n);
        await el.selectOption({ index: i }, T);
        lines.push(`await ${loc}.selectOption({ index: ${i} });`);
        what = `choose option ${i} of ${c.role} ${q(c.name)}`;
      } else {
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
      what = `wait ${ms / 1000} s`;
      await page.waitForTimeout(ms);
      lines.push(`await page.waitForTimeout(${ms});`);
    }
  } catch (e) {
    // the control went away or never became ready: the action is part of the walk all the same
    const why = String((e && e.message) || e).split('\n')[0].slice(0, 120);
    what = `${what || kind}: not done (${why})`;
    lines.push(`// not done in hydo's run: ${why}`);
  }
  await page.waitForTimeout(settle);
  lines.push(`await page.waitForTimeout(${settle});`);
  return { what, lines };
}

// ------------------------------------------------------------------ setup steps (sign in)
// Each is one of {open: <path>}, {click: <name>[, role]}, {fill: <label>, with: <value>},
// {press: <key>}, {see: <text>}, {wait: <ms>}.
async function setup(page) {
  for (const s of cfg.setup || []) {
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
  uses = (state && state.uses) || {};
  // the controller copies these files out through the agent, about 1.5 KB per call, after the
  // verdict and at most 1 MiB of them: a video and a trace (DOM snapshots, no screenshots) of the
  // last step that takes an action, where a walk's failure shows most often (Milestone 264: a
  // shrunk walk leaves the steps after its failure without actions)
  const nsteps = cfg.steps || 1;
  const acts = (s) => Array.from({ length: actions }, (_, k) => k).some((k) => !walk.skip.has(`${s}.${k}`));
  let lastActive = nsteps - 1;
  while (lastActive > 0 && !acts(lastActive)) lastActive--;
  const tracing = walk.rec && step === lastActive;
  if (tracing) opts.recordVideo = { dir: path.join(own, 'video'), size: { width: 640, height: 360 } };
  const context = await browser.newContext(opts);
  if (tracing) await context.tracing.start({ screenshots: false, snapshots: true, title });
  // What the server acknowledged: every write, answered 2xx. A write is a request under
  // watch.requests whose path matches watch.failures_of, or without that key every request that
  // is not a GET. Each goes to walk/acks.jsonl with the printable text of the request and the
  // answer. $HYDO_ACKED gets one word per resource name in them, `<the path's last part>:<name>`
  // (such as CreateMemo:memos/T5k4x8o5), or the path's last part alone: the file is a list of
  // words, and a power cut keeps it for the recovery boot. A project's own checks read them.
  const writes = watch.failures_of ? new RegExp(watch.failures_of) : null;
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
    if (writes ? !writes.test(p) : req.method() === 'GET') return;
    const printable = (b) =>
      (b || '')
        .toString('latin1')
        .replace(/[^\x20-\x7e]+/g, ' ')
        .trim()
        .slice(0, 600);
    let answer = '';
    try {
      answer = printable(await res.body());
    } catch {
      answer = '';
    }
    const sent = printable(req.postDataBuffer());
    const names = [...new Set(`${sent} ${answer}`.match(/\b[a-z][a-z0-9_-]*\/[A-Za-z0-9_-]{6,}\b/g) || [])].slice(0, 6);
    fs.appendFileSync(path.join(own, 'acks.jsonl'), JSON.stringify({ t: Date.now(), step, method: req.method(), path: p, status: res.status(), names, sent, answer }) + '\n');
    if (ackedFile) {
      try {
        const last = p.split('/').filter(Boolean).pop() || p;
        const words = names.length ? names.map((n) => `${last}:${n}`) : [last];
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
  for (let k = 0; k < actions; k++) {
    if (walk.skip.has(`${step}.${k}`)) continue;
    const { what, lines } = await act(page, k);
    log(`walk ${step}.${k}: ${what}`);
    code.push(...lines);
  }
  await page.waitForTimeout(quiet);
  try {
    const url = page.url();
    if (url.startsWith('http')) {
      fs.writeFileSync(statePath, JSON.stringify({ url, signedIn: signedIn || !!(state && state.signedIn), uses, storage: await context.storageState() }));
    }
  } catch {
    /* the page went away: the next step starts where this one started */
  }
  // A recording larger than this stays in the guest: the copy out of it must fit the run's copy
  // budget, and a trace's snapshots carry the app's scripts and styles (Memos's: megabytes).
  const keep = (file, why) => {
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
    indexLine(file, why);
  };
  if (tracing) {
    const trace = `walk-step-${step}.trace.zip`;
    await context.tracing.stop({ path: path.join(shots, trace) }).catch(() => {});
    keep(trace, 'the Playwright trace of the walk (npx playwright show-trace)');
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

main().catch((e) => {
  log(`walk ${step}: the walker stopped: ${String((e && e.stack) || e).split('\n').slice(0, 3).join(' / ')}`);
  event({ kind: 'test', test: title, file: 'walk', status: 'failed', expected: 'passed', ms: 0, error: String(e && e.message) });
  process.exit(1);
});

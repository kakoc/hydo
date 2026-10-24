'use strict';
// Unit tests of hydo-assert (node --test). The byte-for-byte comparison with the Go SDK is
// test/conformance.sh; these cover what the Go program cannot express (cycles, undefined,
// Map/Set, async liveness, the sink switches) and the BUGGIFY rates.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const ha = require('..');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'hydo-js-unit-'));
let n = 0;
function capture(fn) {
  const f = path.join(tmp, 'out' + n++);
  fs.writeFileSync(f, '');
  process.env.HYDO_ASSERT_OUT = f;
  delete process.env.HYDO_BUGGIFY;
  process.env.HYDO_BUGGIFY_FILE = '/nonexistent';
  ha._reset();
  fn();
  ha._reset();
  return fs.readFileSync(f, 'utf8');
}

test('encoder: Go formats for JS-only values', () => {
  assert.strictEqual(ha.toJSON({ b: 1, a: undefined, f() {}, s: Symbol('x') }), '{"b":1}');
  assert.strictEqual(ha.toJSON([undefined, () => 1]), '[null,null]');
  assert.strictEqual(ha.toJSON(new Set([3, 1])), '[3,1]');
  assert.strictEqual(ha.toJSON(new Map([[2, 'x'], ['10', 'y']])), '{"10":"y","2":"x"}');
  assert.strictEqual(ha.toJSON(new Date(0)), '"1970-01-01T00:00:00.000Z"');
  assert.strictEqual(ha.toJSON(Buffer.from([255])), '"/w=="');
  assert.strictEqual(ha.toJSON(new Float64Array([0.5, -0])), '[0.5,-0]');
  assert.strictEqual(ha.toJSON('\udc00 😀 \ud83d'), '"\\ufffd 😀 \\ufffd"');
  const c = { a: 1 };
  c.self = c;
  assert.strictEqual(ha.toJSON(c), null);
  assert.strictEqual(ha.toJSON({ x: NaN }), null);
  const shared = { k: 1 };
  assert.strictEqual(ha.toJSON({ a: shared, b: shared }), '{"a":{"k":1},"b":{"k":1}}'); // not a cycle
});

test('dedup: one record per (assertion, outcome); sometimes false never written', () => {
  const a = ha.always('u.a', 'a');
  const s = ha.sometimes('u.s', 's');
  const out = capture(() => {
    a.check(true, { i: 1 });
    a.check(true, { i: 2 });
    a.check(false, { i: 3 });
    a.check(false, { i: 4 });
    s.check(false, { i: 5 });
    s.check(0);
  });
  assert.deepStrictEqual(out.trim().split('\n'), [
    'HYDO-ASSERT {"t":"always","id":"u.a","ok":true,"msg":"a","details":{"i":1}}',
    'HYDO-ASSERT {"t":"always","id":"u.a","ok":false,"msg":"a","details":{"i":3}}',
  ]);
});

test('an unencodable record is dropped and still counts as seen (as in Go)', () => {
  const a = ha.always('u.cyc', 'cycle');
  const c = {};
  c.c = c;
  const out = capture(() => {
    a.check(true, c);
    a.check(true, { ok: 1 });
  });
  assert.strictEqual(out, '');
});

test('details that are not an object are written as {"value": ...}; helpers add fields', () => {
  const a = ha.always('u.v', 'v');
  const b = ha.always('u.m', 'm');
  const m = new Map([['x', 1]]);
  const out = capture(() => {
    a.check(true, 'just text');
    a.check(false, [1, 2]);
    b.lessThanOrEqualTo(1n, 2, m);
  });
  assert.deepStrictEqual(out.trim().split('\n'), [
    'HYDO-ASSERT {"t":"always","id":"u.v","ok":true,"msg":"v","details":{"value":"just text"}}',
    'HYDO-ASSERT {"t":"always","id":"u.v","ok":false,"msg":"v","details":{"value":[1,2]}}',
    'HYDO-ASSERT {"t":"always","id":"u.m","ok":true,"msg":"m","details":{"left":1,"op":"\\u003c=","right":2,"x":1}}',
  ]);
  assert.strictEqual(m.get('op'), '<=');
});

test('emitLine and setupComplete', () => {
  const out = capture(() => {
    assert.strictEqual(ha.emitLine('HYDO-HISTORY', { op: 'put', k: '<1>' }), true);
    assert.strictEqual(ha.emitLine('HYDO-HISTORY', { bad: Infinity }), false);
    ha.setupComplete();
  });
  assert.strictEqual(out, 'HYDO-HISTORY {"k":"\\u003c1\\u003e","op":"put"}\nHYDO-SETUP-COMPLETE\n');
});

test('eventually: awaitCond (async) in time and late', async () => {
  const ok = ha.eventually('u.ev_ok', 'ok', 5000);
  const late = ha.eventually('u.ev_late', 'late', 20);
  const f = path.join(tmp, 'ev');
  fs.writeFileSync(f, '');
  process.env.HYDO_ASSERT_OUT = f;
  ha._reset();
  let k = 0;
  assert.strictEqual(await ok.awaitCond(async () => ++k >= 3, 5, { a: 1 }), true);
  assert.strictEqual(await late.awaitCond(() => false, 5), false);
  assert.strictEqual(late.awaitCondSync(() => false, 5), false);
  ha._reset();
  const lines = fs.readFileSync(f, 'utf8').trim().split('\n').map((l) => JSON.parse(l.slice(l.indexOf(' ') + 1)));
  assert.strictEqual(lines.length, 2); // the second late episode has the same outcome: not written again
  assert.strictEqual(lines[0].ok, true);
  assert.strictEqual(lines[0].details.within_ms, 5000);
  assert.strictEqual(lines[0].details.a, 1);
  assert.strictEqual(lines[1].ok, false);
  assert.ok(lines[1].details.elapsed_ms >= 20);
});

test('sink: off writes nothing, stderr and an unopenable path go to stderr', () => {
  const prog = "const h=require(process.argv[1]); const a=h.always('s.x','x'); h.catalog(); a.check(true); h.setupComplete();";
  const pkg = path.join(__dirname, '..');
  for (const [v, want] of [['off', ''], ['stderr', 2], ['/nonexistent/dir/file', 2]]) {
    const r = spawnSync(process.execPath, ['-e', prog, pkg], { env: { ...process.env, HYDO_ASSERT_OUT: v }, encoding: 'utf8' });
    assert.strictEqual(r.status, 0);
    assert.strictEqual(r.stdout, '');
    if (want === '') assert.strictEqual(r.stderr, '');
    else assert.strictEqual(r.stderr.split('\n').length - 1, 3, r.stderr);
  }
});

test('buggify: off without a seed; same seed same decisions; rates about 25% and 25%', () => {
  const ps = 'abcdefgh'.split('').map((c) => ha.buggify('ut.' + c, c));
  const trace = (k) => {
    let s = '';
    for (let i = 0; i < k; i++) for (const p of ps) s += p.fire() ? '1' : '0';
    return s;
  };
  process.env.HYDO_ASSERT_OUT = 'off';
  process.env.HYDO_BUGGIFY_FILE = '/nonexistent';
  delete process.env.HYDO_BUGGIFY;
  ha._reset();
  assert.ok(!trace(50).includes('1'));
  process.env.HYDO_BUGGIFY = '1234abcd';
  ha._reset();
  const a = trace(200);
  ha._reset();
  assert.strictEqual(trace(200), a);
  let en = 0;
  let fired = 0;
  let hits = 0;
  for (let s = 1; s <= 400; s++) {
    process.env.HYDO_BUGGIFY = s.toString(16);
    ha._reset();
    ps[0].fire(); // loads the configuration
    for (const p of ps) {
      if (!p.enabled()) continue;
      en++;
      for (let i = 0; i < 20; i++) {
        hits++;
        if (p.fire()) fired++;
      }
    }
  }
  const er = en / (400 * 8);
  const fr = fired / hits;
  assert.ok(er > 0.2 && er < 0.3, 'enable rate ' + er);
  assert.ok(fr > 0.2 && fr < 0.3, 'fire rate ' + fr);
  delete process.env.HYDO_BUGGIFY;
  ha._reset();
});

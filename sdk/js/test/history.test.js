'use strict';
// Unit tests of hydo-assert/history (node --test). The byte-for-byte comparison with the Go
// recorder is test/history-conformance.sh; these repeat Go's own unit test (hydohist_test.go)
// and cover the argument checks, the sink switches and the one-write-per-record rule.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const h = require('../history.js');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'hydo-js-hist-'));
let n = 0;
function capture(fn) {
  const f = path.join(tmp, 'hist' + n++);
  process.env.HYDO_HIST_OUT = f;
  h._reset();
  fn();
  h._reset();
  return fs.existsSync(f) ? fs.readFileSync(f, 'utf8') : null;
}
const masked = (out) => out.trim().split('\n').map((l) => {
  const f = l.split(' ');
  assert.match(f[4], /^[0-9]+$/);
  f[4] = '*';
  return f.join(' ');
});

test('the records of Go\'s TestRecords', () => {
  const out = capture(() => {
    const kv = new h.History('kv');
    let op = kv.invoke(3, 'write', 'k1', h.enc('a b'));
    op.ok('k1', h.ver('a b', 83));
    op = kv.invoke(3, 'read', 'k1');
    op.ok('k1', h.valueOrNil('', false));
    op = kv.invoke(4, 'txn', h.append('x', '1'), h.read('y'));
    op.ok(h.append('x', '1'), h.readResult('y', '1', '2'));
    kv.invoke(5, 'write', 'k2', '').info();
  });
  assert.deepStrictEqual(masked(out), [
    'HYDO-HIST kv 3 invoke * write k1 a%20b',
    'HYDO-HIST kv 3 ok * write k1 a%20b@83',
    'HYDO-HIST kv 3 invoke * read k1',
    'HYDO-HIST kv 3 ok * read k1 nil',
    'HYDO-HIST kv 4 invoke * txn a:x:1 r:y',
    'HYDO-HIST kv 4 ok * txn a:x:1 r:y:[1,2]',
    'HYDO-HIST kv 5 invoke * write k2 %',
    'HYDO-HIST kv 5 info * write k2 %',
  ]);
});

test('enc: Go\'s TestEnc and every byte class', () => {
  assert.strictEqual(h.enc('a,b[c]:d@e%f\tg'), 'a%2Cb%5Bc%5D%3Ad%40e%25f%09g');
  assert.strictEqual(h.enc(''), '%');
  assert.strictEqual(h.enc('nil'), '%6Eil');
  assert.strictEqual(h.enc('nil '), 'nil%20');
  assert.strictEqual(h.enc(' \n\r\x00\x1f\x7f'), '%20%0A%0D%00%1F%7F');
  assert.strictEqual(h.enc('é 中文 😀'), 'é%20中文%20😀');
  assert.strictEqual(h.enc('a\ud800b'), 'a\uFFFDb'); // a lone surrogate: U+FFFD
  assert.strictEqual(h.enc('\udc00'), '\uFFFD');
  assert.strictEqual(h.enc(42), '42');
  assert.strictEqual(h.enc(-7n), '-7');
  assert.strictEqual(h.NIL, 'nil');
});

test('helpers', () => {
  assert.strictEqual(h.ver('v', -9223372036854775808n), 'v@-9223372036854775808');
  assert.strictEqual(h.ver('v', 2 ** 53), 'v@9007199254740992');
  assert.strictEqual(h.ver('v', -0), 'v@0');
  assert.throws(() => h.ver('v', 1.5), TypeError);
  assert.throws(() => h.ver('v', '3'), TypeError);
  assert.throws(() => h.ver('v', 9223372036854775808n), RangeError);
  assert.throws(() => h.ver('v', 1e19), RangeError);
  assert.strictEqual(h.valueOrNil(null), 'nil');
  assert.strictEqual(h.valueOrNil(undefined), 'nil');
  assert.strictEqual(h.valueOrNil(''), '%');
  assert.strictEqual(h.valueOrNil('x', false), 'nil');
  assert.strictEqual(h.valueOrNil('x', true), 'x');
  assert.strictEqual(h.list(), '[]');
  assert.strictEqual(h.list('', 'a,b', 'nil'), '[%,a%2Cb,%6Eil]');
  assert.strictEqual(h.readResult('k'), 'r:k:[]');
  assert.strictEqual(h.history('x').name, 'x');
  assert.throws(() => new h.History('kv').invoke(1.5, 'read'), TypeError);
  assert.throws(() => new h.History('kv').invoke(1, 'read', {}), TypeError);
});

test('ok without args repeats the invocation; a process may be a token', () => {
  const out = capture(() => {
    const t = new h.History('t');
    t.invoke('w-3', 'write', 'k', 'v').ok();
    t.invoke(1, 'write', '', 'v').fail();
    t.record(2, 'info', 'read');
  });
  assert.deepStrictEqual(masked(out), [
    'HYDO-HIST t w-3 invoke * write k v',
    'HYDO-HIST t w-3 ok * write k v',
    'HYDO-HIST t 1 invoke * write % v',
    'HYDO-HIST t 1 fail * write % v',
    'HYDO-HIST t 2 info * read',
  ]);
});

test('one fs.writeSync per record; times never decrease', () => {
  const real = fs.writeSync;
  let calls = 0;
  let out;
  try {
    fs.writeSync = function (...a) {
      calls++;
      return real.apply(this, a);
    };
    out = capture(() => {
      const t = new h.History('t');
      for (let i = 0; i < 50; i++) t.invoke(i, 'write', 'k', String(i)).ok();
    });
  } finally {
    fs.writeSync = real;
  }
  assert.strictEqual(calls, 100);
  const times = out.trim().split('\n').map((l) => BigInt(l.split(' ')[4]));
  for (let i = 1; i < times.length; i++) assert.ok(times[i] >= times[i - 1]);
});

test('sink switches: append, create, off, stderr, fallback; separate from HYDO_ASSERT_OUT', () => {
  const f = path.join(tmp, 'existing');
  fs.writeFileSync(f, 'PRE\n');
  process.env.HYDO_HIST_OUT = f;
  h._reset();
  new h.History('a').record(1, 'ok', 'read');
  h._reset();
  assert.match(fs.readFileSync(f, 'utf8'), /^PRE\nHYDO-HIST a 1 ok [0-9]+ read\n$/);

  assert.strictEqual(capture(() => new h.History('a').record(1, 'ok', 'read')).split('\n').length, 2); // created

  const script = "const h=require(process.argv[1]);new h.History('s').invoke(1,'read','k').ok('k','nil');";
  const run = (out) => spawnSync(process.execPath, ['-e', script, path.join(__dirname, '..', 'history.js')], {
    env: { ...process.env, HYDO_HIST_OUT: out, HYDO_ASSERT_OUT: path.join(tmp, 'assert-out') },
    encoding: 'utf8',
  });
  let r = run('off');
  assert.strictEqual(r.stdout + r.stderr, '');
  r = run('stderr');
  assert.strictEqual(r.stdout, '');
  assert.match(r.stderr, /^HYDO-HIST s 1 invoke [0-9]+ read k\nHYDO-HIST s 1 ok [0-9]+ read k nil\n$/);
  r = run(path.join(tmp, 'no-such-dir', 'x'));
  assert.match(r.stderr, /^HYDO-HIST s 1 invoke /);
  assert.ok(!fs.existsSync(path.join(tmp, 'assert-out')));
});

test('the ESM entry shares the CommonJS module', async () => {
  const m = await import('../history.mjs');
  assert.strictEqual(m.History, h.History);
  assert.strictEqual(m.enc, h.enc);
  assert.strictEqual(m.default, h);
});

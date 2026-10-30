'use strict';
// The JavaScript half of the history recorder's conformance test (test/history-conformance.sh):
// the same calls as test/history-go/main.go, in the same order. The records (time masked) and
// the stdouts of the two programs must be byte-identical. HYDO_JS_ENTRY=esm loads the recorder
// through its ESM entry. Where Go passes U+FFFD, this side passes a lone surrogate.
const fs = require('fs');

async function main() {
  const h = process.env.HYDO_JS_ENTRY === 'esm' ? await import('../history.mjs') : require('../history.js');
  let stdout = '';
  const println = (...a) => {
    stdout += a.join(' ') + '\n';
  };

  const kv = new h.History('kv');
  const la = h.history('la');
  const other = new h.History('Other.hist_2-x');

  let ascii = '';
  for (let c = 0; c < 0x80; c++) ascii += String.fromCharCode(c);
  const vals = [
    '', 'nil', 'Nil', 'nil ', ' nil', 'nilnil', '%', '%6Eil', '@', ' ', '\t', '\n', '\r',
    '\x00', '\x01', '\x1f', '\x7f', ',', '[', ']', ':', 'a,b[c]:d@e%f\tg', 'a b',
    'plain-Token_1.2', '~!#$^&*()_+-={}|\\;\'"<>?/`', 'é', 'café au lait', '中文',
    '日本語,テスト', '😀', 'a😀b c', '\u00a0', '\u2028\u2029', '\uFFFD', '\uFFFF',
    'a\ud800b',
    '\udc00\ud83d',
    42,
    -7n,
    ascii,
  ];
  vals.forEach((v, i) => {
    println(h.enc(v));
    const k = 'k' + i;
    const op = kv.invoke(i % 3, 'write', k, h.enc(v));
    switch (i % 4) {
      case 0: op.ok(); break;
      case 1: op.ok(k, h.ver(v, BigInt(i) * 1000003n)); break;
      case 2: op.fail(); break;
      default: op.info();
    }
  });

  const vers = [0, 1, -1, 83, -0, 2 ** 53, 9223372036854775807n, -9223372036854775808n, 9007199254740993n, -9007199254740993n];
  vers.forEach((n, i) => {
    println(h.ver('v', n));
    const op = kv.invoke(10 + i, 'write', 'kv', 'v');
    op.ok('kv', h.ver('v', n));
  });
  println(h.ver('', 5), h.ver('nil', -5n), h.ver('a@b', 7), h.ver(12, 12));

  const von = [
    h.valueOrNil('', true), h.valueOrNil('', false), h.valueOrNil('nil'),
    h.valueOrNil('x y', true), h.valueOrNil('x', false), h.valueOrNil('é'),
    h.valueOrNil(null), h.valueOrNil(undefined), h.valueOrNil('v', 1),
  ];
  println(von.join(' '));
  let op = kv.invoke(20, 'read', 'k1', 'k2', 'k3');
  op.ok('k1', von[0], 'k2', von[1], 'k3', h.NIL);
  op = kv.invoke(20, 'read', 'k4');
  op.ok('k4', h.valueOrNil('x y'));

  const lists = [
    h.list(), h.list(''), h.list('a'),
    h.list('a', 'b,c', '[x]', 'nil', '', 'é', '1 2', '%', ':'),
    h.list(1, 2n, '3'),
  ];
  println(lists.join(' '));
  la.invoke(1, 'append', 'x', 1).ok();
  la.invoke(1, 'read', 'x').ok('x', h.list('1'));
  la.invoke(2, 'read', 'empty').ok('empty', h.list());
  la.invoke(2, 'read', 'odd').ok('odd', lists[3]);

  const mops = [
    h.append('x', 2), h.read('y'), h.append('k:1', 'e,2'), h.read(''),
    h.readResult('y'), h.readResult('', 'a', 'b c'), h.readResult('nil', 'nil'),
    h.append('', ''), h.read('@'),
  ];
  println(mops.join(' '));
  let tx = la.invoke(3, 'txn', mops[0], mops[1], mops[2], mops[3]);
  tx.ok(mops[0], mops[4], mops[2], mops[5]);
  tx = la.invoke(3, 'txn', h.append('x', '3'));
  tx.fail();
  tx = la.invoke(4, 'txn', h.read('x'), h.append('y', '1'));
  tx.info();

  other.record(7, 'invoke', 'cas', 'k', 'a', 'b');
  other.record(7n, 'ok', 'cas', 'k', 'a', 'b@9');
  other.record('8', 'info', 'read');
  other.record(0, 'fail', 'write', '', 'x', '');

  op = kv.invoke(9, 'write', '', '');
  op.ok('', '');
  op = kv.invoke(9, 'write', '', '');
  op.ok();

  op = kv.invoke(5, 'noop');
  op.ok();
  op = kv.invoke(5, 'noop');
  op.fail();
  op = kv.invoke(5, 'noop');
  op.info();
  kv.invoke(2 ** 40, 'write', 'big', '1').ok();
  kv.invoke(-5, 'write', 'neg', 1).ok();
  kv.invoke(9223372036854775807n, 'write', 'max', '1').info();
  kv.invoke(-9223372036854775808n, 'write', 'min', '1').fail();

  const a = kv.invoke(30, 'write', 'i', '1');
  const b = la.invoke(30, 'append', 'i', '1');
  const c = other.invoke(31, 'read', 'i');
  b.ok();
  c.ok('i', 1);
  a.info();

  other.invoke(40, 'write', 'x\udc00', 'é').ok();
  other.invoke(41, 'write', 'n', 42).ok('n', 42n, -7);
  println(h.NIL);
  fs.writeSync(1, stdout);
}
main();

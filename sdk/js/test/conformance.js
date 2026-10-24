'use strict';
// The JavaScript half of the conformance test (test/conformance.sh): the same SDK calls as
// test/go-conformance/main.go, in the same order. The consoles and stdouts of the two
// programs must be byte-identical. HYDO_JS_ENTRY=esm loads the SDK through its ESM entry.
async function main() {
  const ha = process.env.HYDO_JS_ENTRY === 'esm' ? await import('../index.mjs') : require('..');

  const rich = () => ({
    s: '<>&"\\\n\r\t\b\f\x01\x1f\x7f é     😀 ￿ 中文 ok',
    bad: 'a\ud800b',
    neg: -5,
    zero: 0,
    umax: 18446744073709551615n,
    imin: -9223372036854775808n,
    i53: 9007199254740993n,
    f1: 0.1,
    fneg0: -0,
    f21: 1e21,
    fn21: -1e21,
    f20: 1e20,
    f64: 2 ** 64,
    fsm: 1.5e-7,
    fsm2: 9.99e-7,
    f6: 1e-6,
    f6b: 1.234e-6,
    fmix: 123456789.125,
    f3: 3.0,
    ftiny: 2.5e-300,
    fsub: 5e-324,
    fmax: Number.MAX_VALUE,
    fbig: 1e100,
    fthird: 1 / 3,
    t: true,
    f: false,
    nil: null,
    bytes: new Uint8Array([0, 1, 2, 250, 255]),
    hello: Buffer.from('hello'),
    ints: new Int32Array([1, -2, 3]),
    list: [1, 'a', false, null, new Map([['z', 1], ['a', 2]]), []],
    empty: [],
    emap: {},
    nest: { 'é': 1, B: 2, a: 3, _: 4, 'a b': { x: 'y' }, '￿': 5, '😀': 6, '': 7,
      Z: { deep: [{ k: '<v>' }] } },
  });

  const a = ha.always('c.always', 'always <holds> & "quotes"');
  const b = ha.alwaysOrUnreachable('c.aou', 'always or unreachable');
  const s = ha.sometimes('c.sometimes', 'sometimes é   true 😀');
  const r = ha.reachable('c.reach', 'reached');
  const u = ha.unreachable('c.unreach', 'never reached');
  ha.always('c.never', 'never evaluated');
  const cmp = ha.always('c.cmp', 'a comparison');
  const scmp = ha.sometimes('c.scmp', 'a sometimes comparison');
  const nan = ha.always('c.nan', 'details with a NaN');
  const uni = ha.always('c.ünï', 'ünïcödé id \x01');
  const l1 = ha.eventually('c.live_ok', 'in time', 10000);
  const l2 = ha.eventually('c.live_late', 'never', 0);
  const ps = ['p.a', 'p.b', 'p.c', 'p.d', 'p.e', 'p.f', 'p.g', 'p.h'].map((id) => ha.buggify(id, 'point ' + id.toUpperCase()));

  ha.catalog();

  a.check(true, null);
  a.check(true, { x: 1 });
  a.check(false, rich());
  a.check(false, { dup: true });
  b.check(true, {});
  s.check(false);
  s.check(true, { n: 3 });
  r.hit();
  r.hit(null);
  u.hit({ why: 'test' });
  cmp.cmp(3, '<=', 5);
  cmp.cmp(7, '<', 5, { extra: 'e' });
  cmp.cmp(1, '??', 1);
  scmp.greaterThan(1, 2);
  scmp.cmp(2, '==', 2n);
  nan.check(true, { f: NaN });
  nan.check(true, { f: 1 });
  nan.check(false, { f: Infinity });
  nan.check(false, { f: -Infinity });
  uni.check(true, new Map([['k', 'v']]));
  const w = l1.begin();
  w.done();
  w.done();
  l1.awaitCondSync(() => true, 1);
  const w2 = l2.begin();
  w2.expire({ k: 'v' });
  ha.setupComplete();

  let fires = '';
  for (let i = 0; i < 50; i++) {
    const shared = { round: i, stmt: 'x' };
    ps.forEach((p, j) => {
      let d;
      if (j === 0) d = { round: i };
      else if (j === 1 || j === 2) d = shared;
      fires += p.fire(d) ? '1' : '0';
    });
  }
  ha.catalog();
  process.stdout.write(fires + '\n');
}
main();

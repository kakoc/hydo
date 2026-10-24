'use strict';
// hydo-assert: hydo's assertion SDK for Node.js guest programs.
//
// A program running under hydo (the deterministic hypervisor) uses it to say what must be
// true (always, alwaysOrUnreachable, unreachable), what should happen at least once
// (sometimes, reachable), what must happen within a deadline (eventually), and where a
// dependency may misbehave (buggify fault points). Records go to the guest console in the
// wire format of the Go SDK (hvx/stack/hydoassert), byte for byte, with the same
// deduplication rules; hvx/ASSERTIONS.md is the contract.
//
// Wire format, one record per line, each written with a single write():
//
//   HYDO-ASSERT-DECL {"t":"always","id":"order.acked","msg":"..."}
//   HYDO-ASSERT {"t":"always","id":"order.acked","ok":false,"msg":"...","details":{...}}
//   HYDO-SETUP-COMPLETE
//
// Determinism: the output is a pure function of the evaluation sequence -- no clocks (except
// the guest's monotonic clock for `eventually`, which hydo makes deterministic), no host
// state, the catalog in declaration order, details with object keys in bytewise (UTF-8)
// order, numbers and strings formatted as Go's encoding/json formats them.
//
// No runtime dependencies; plain CommonJS (index.mjs re-exports this module for ESM, so a
// program that uses both share one state).

const fs = require('fs');

// ---------------------------------------------------------------------------------------------
// Output sink: the guest console, opened once, as the Go SDK does.
//
// HYDO_ASSERT_OUT unset or empty: /dev/ttyS0; "off": nothing (a no-op outside hydo); "stderr":
// standard error (plain logging outside hydo); anything else: that file, appended. If the
// file cannot be opened the records go to standard error.

const SINK_UNSET = 0;
const SINK_OFF = 1;
const SINK_FD = 2;
let sinkState = SINK_UNSET;
let sinkFd = -1;

function openSink() {
  const p = process.env.HYDO_ASSERT_OUT || '';
  if (p === 'off') {
    sinkState = SINK_OFF;
    return;
  }
  sinkState = SINK_FD;
  if (p === 'stderr') {
    sinkFd = 2;
    return;
  }
  const c = fs.constants;
  // O_NOCTTY: never make the console the controlling terminal of the caller (0 where absent).
  const flags = c.O_WRONLY | c.O_APPEND | (c.O_NOCTTY || 0);
  try {
    sinkFd = fs.openSync(p === '' ? '/dev/ttyS0' : p, flags);
  } catch (e) {
    sinkFd = 2;
  }
}

// writeLine writes one complete line (a Buffer ending in '\n'). One write() call; a short
// write (which the console never does) is continued, as Go's File.Write does.
function writeLine(buf) {
  if (sinkState === SINK_UNSET) openSink();
  if (sinkState === SINK_OFF) return;
  try {
    let off = 0;
    while (off < buf.length) {
      const n = fs.writeSync(sinkFd, buf, off, buf.length - off);
      if (n <= 0) break;
      off += n;
    }
  } catch (e) {
    // like the Go SDK: a failed console write is ignored
  }
}

// ---------------------------------------------------------------------------------------------
// JSON encoder: exactly what Go's encoding/json (json.Marshal) writes for the same data.
//
//  - object keys sorted by their UTF-8 bytes (Go sorts map keys bytewise; JavaScript's own
//    string order is by UTF-16 units, which differs above U+D7FF);
//  - strings: '"' and '\\' escaped, \b \f \n \r \t, other control characters as \u00xx,
//    '<' '>' '&' as \u003c \u003e \u0026, U+2028 and U+2029 as \u2028 and \u2029, and a
//    lone surrogate (JavaScript's form of invalid UTF-8) as \ufffd;
//  - numbers: shortest round-trip digits, exponent form below 1e-6 and from 1e21 (JavaScript's
//    Number#toString already matches Go's float64 format there), -0 as "-0"; a NaN or an
//    infinity makes the whole record unencodable (Go's Marshal fails, the record is dropped);
//  - bigint: decimal integer (Go's int64/uint64/big.Int);
//  - Uint8Array (and Buffer): a base64 string (Go's []byte);
//  - other typed arrays and Set: arrays; Map: an object with String(key) keys;
//  - a value with a toJSON() method is replaced by its result (as JSON.stringify does);
//  - undefined, functions and symbols: left out of objects, null in arrays (as JSON.stringify);
//  - a cycle makes the record unencodable (Go's Marshal fails on cycles too).

class Unencodable extends Error {}

const HEX = '0123456789abcdef';

function quote(s) {
  s = String(s);
  let out = '"';
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    let esc = null;
    if (c < 0x80) {
      if (c >= 0x20 && c !== 0x22 && c !== 0x5c && c !== 0x3c && c !== 0x3e && c !== 0x26) continue;
      switch (c) {
        case 0x22: esc = '\\"'; break;
        case 0x5c: esc = '\\\\'; break;
        case 0x08: esc = '\\b'; break;
        case 0x0c: esc = '\\f'; break;
        case 0x0a: esc = '\\n'; break;
        case 0x0d: esc = '\\r'; break;
        case 0x09: esc = '\\t'; break;
        default: esc = '\\u00' + HEX[c >> 4] + HEX[c & 0xf];
      }
    } else if (c === 0x2028) {
      esc = '\\u2028';
    } else if (c === 0x2029) {
      esc = '\\u2029';
    } else if (c >= 0xd800 && c <= 0xdbff) {
      const d = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
      if (d >= 0xdc00 && d <= 0xdfff) {
        i++; // a valid pair: written as is (UTF-8 on the wire)
        continue;
      }
      esc = '\\ufffd';
    } else if (c >= 0xdc00 && c <= 0xdfff) {
      esc = '\\ufffd';
    } else {
      continue;
    }
    out += s.slice(start, i) + esc;
    start = i + 1;
  }
  return out + s.slice(start) + '"';
}

function formatNumber(x) {
  if (!Number.isFinite(x)) throw new Unencodable();
  if (x === 0 && 1 / x < 0) return '-0';
  return String(x);
}

// Bytewise (UTF-8) order of two strings. Equal to UTF-16 order except when one side has a
// surrogate (astral character) where the other has U+E000..U+FFFF.
function cmpUtf8(a, b) {
  if (a === b) return 0;
  return Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8'));
}

function isSkipped(v) {
  return v === undefined || typeof v === 'function' || typeof v === 'symbol';
}

function encodeObject(entries, stack) {
  // entries: [key, value] pairs; skipped values are left out, keys sorted bytewise
  const kept = entries.filter((e) => !isSkipped(e[1]));
  kept.sort((x, y) => cmpUtf8(x[0], y[0]));
  let out = '{';
  for (let i = 0; i < kept.length; i++) {
    if (i > 0) out += ',';
    out += quote(kept[i][0]) + ':' + encodeValue(kept[i][1], stack);
  }
  return out + '}';
}

function encodeArray(items, stack) {
  let out = '[';
  for (let i = 0; i < items.length; i++) {
    if (i > 0) out += ',';
    out += isSkipped(items[i]) ? 'null' : encodeValue(items[i], stack);
  }
  return out + ']';
}

function encodeValue(v, stack) {
  if (v !== null && typeof v === 'object' && typeof v.toJSON === 'function' && !(v instanceof Uint8Array)) {
    v = v.toJSON();
  }
  switch (typeof v) {
    case 'string': return quote(v);
    case 'number': return formatNumber(v);
    case 'bigint': return v.toString(10);
    case 'boolean': return v ? 'true' : 'false';
    case 'undefined': return 'null';
    case 'function':
    case 'symbol': return 'null';
  }
  if (v === null) return 'null';
  if (v instanceof Number || v instanceof String || v instanceof Boolean) return encodeValue(v.valueOf(), stack);
  if (stack.indexOf(v) >= 0) throw new Unencodable(); // a cycle
  stack.push(v);
  let out;
  if (v instanceof Uint8Array) {
    out = '"' + Buffer.from(v.buffer, v.byteOffset, v.byteLength).toString('base64') + '"';
  } else if (Array.isArray(v)) {
    out = encodeArray(v, stack);
  } else if (ArrayBuffer.isView(v) && typeof v.length === 'number') {
    out = encodeArray(Array.from(v), stack);
  } else if (v instanceof Set) {
    out = encodeArray(Array.from(v), stack);
  } else if (v instanceof Map) {
    out = encodeObject(Array.from(v, (e) => [String(e[0]), e[1]]), stack);
  } else {
    out = encodeObject(Object.keys(v).map((k) => [k, v[k]]), stack);
  }
  stack.pop();
  return out;
}

// toJSON returns the Go encoding of a details value, or null if it cannot be encoded (it holds
// a NaN, an infinity or a cycle). Exported for tests and for record kinds built on emitLine.
function toJSON(v) {
  try {
    return encodeValue(v, []);
  } catch (e) {
    if (e instanceof Unencodable || e instanceof RangeError) return null;
    throw e;
  }
}

// A details object is omitted from a record when it is absent or has no keys (Go's
// `json:"details,omitempty"` on a map).
// A details value that is not an object (or is an array) is written as {"value": d}.
function detailsJSON(d) {
  if (d === undefined || d === null) return '';
  if (typeof d !== 'object' || Array.isArray(d)) d = { value: d };
  const s = encodeValue(d, []);
  return s === '{}' ? '' : s;
}

function emitRaw(prefix, json) {
  writeLine(Buffer.from(prefix + ' ' + json + '\n', 'utf8'));
}

// emitLine writes one line `<tag> <json of value>` through the same sink, with one write.
// It is the extension point for other one-line record kinds (for example a history
// recorder). Returns false (and writes nothing) if the value cannot be encoded.
function emitLine(tag, value) {
  const j = toJSON(value);
  if (j === null) return false;
  emitRaw(String(tag), j);
  return true;
}

// ---------------------------------------------------------------------------------------------
// Assertions.

const T_ALWAYS = 'always';
const T_ALWAYS_OR_UNREACHABLE = 'always_or_unreachable';
const T_SOMETIMES = 'sometimes';
const T_REACHABLE = 'reachable';
const T_UNREACHABLE = 'unreachable';
const T_EVENTUALLY = 'eventually';

const catalogList = [];

const OPS = {
  '<': (l, r) => l < r,
  '<=': (l, r) => l <= r,
  '==': (l, r) => l == r, // eslint-disable-line eqeqeq -- 1n == 1 must hold
  '!=': (l, r) => l != r, // eslint-disable-line eqeqeq
  '>=': (l, r) => l >= r,
  '>': (l, r) => l > r,
};

// withFields adds fields to the details of an evaluation: to the caller's own object (as the
// Go SDK writes into the caller's map) or, when there is none, to a new one.
function withFields(d, fields) {
  if (d instanceof Map) {
    for (const k of Object.keys(fields)) d.set(k, fields[k]);
    return d;
  }
  if (d === undefined || d === null) d = {};
  else if (typeof d !== 'object' || Array.isArray(d)) d = { value: d };
  return Object.assign(d, fields);
}

class Assertion {
  constructor(t, id, msg) {
    this.t = t;
    this.id = String(id);
    this.msg = String(msg);
    this.seen = [false, false]; // outcome already emitted by this process: [false, true]
  }

  get type() { return this.t; }

  record(ok, d) {
    const i = ok ? 1 : 0;
    if (this.seen[i]) return;
    this.seen[i] = true;
    let det;
    try {
      det = detailsJSON(d);
    } catch (e) {
      if (e instanceof Unencodable || e instanceof RangeError) return; // Go: Marshal fails, nothing is written
      throw e;
    }
    emitRaw('HYDO-ASSERT', '{"t":' + quote(this.t) + ',"id":' + quote(this.id) + ',"ok":' + (ok ? 'true' : 'false') +
      ',"msg":' + quote(this.msg) + (det ? ',"details":' + det : '') + '}');
  }

  // check evaluates an always / alwaysOrUnreachable / sometimes assertion. For sometimes only
  // a true outcome is recorded. Returns the condition as a boolean, so it can be used inline.
  check(cond, d) {
    const ok = !!cond;
    if (this.t === T_SOMETIMES && !ok) return ok;
    this.record(ok, d);
    return ok;
  }

  // hit marks a reachable (recorded ok:true) or unreachable (recorded ok:false: reaching it
  // is a violation) location as reached.
  hit(d) {
    this.record(this.t !== T_UNREACHABLE, d);
  }

  // cmp evaluates `left op right` (op one of "<", "<=", "==", "!=", ">=", ">"; another op
  // evaluates to false) as a check, with left/op/right added to the details. left and right
  // are numbers or bigints.
  cmp(left, op, right, d) {
    const f = Object.prototype.hasOwnProperty.call(OPS, op) ? OPS[op] : null;
    const ok = f ? f(left, right) : false;
    return this.check(ok, withFields(d, { left, op, right }));
  }

  // The named comparison helpers; the records are those of cmp.
  greaterThan(left, right, d) { return this.cmp(left, '>', right, d); }
  greaterThanOrEqualTo(left, right, d) { return this.cmp(left, '>=', right, d); }
  lessThan(left, right, d) { return this.cmp(left, '<', right, d); }
  lessThanOrEqualTo(left, right, d) { return this.cmp(left, '<=', right, d); }
}

function declare(t, id, msg) {
  const a = new Assertion(t, id, msg);
  catalogList.push(a);
  return a;
}

// Declare properties at module top level (load time), so the whole catalog is known before
// the first evaluation and a property that is never evaluated is still reported.
function always(id, msg) { return declare(T_ALWAYS, id, msg); }
function alwaysOrUnreachable(id, msg) { return declare(T_ALWAYS_OR_UNREACHABLE, id, msg); }
function sometimes(id, msg) { return declare(T_SOMETIMES, id, msg); }
function reachable(id, msg) { return declare(T_REACHABLE, id, msg); }
function unreachable(id, msg) { return declare(T_UNREACHABLE, id, msg); }

// catalog emits one HYDO-ASSERT-DECL line per declared assertion, in declaration order. Call it
// once, early, from the process that should carry the catalog into the console.
function catalog() {
  for (const a of catalogList) {
    emitRaw('HYDO-ASSERT-DECL', '{"t":' + quote(a.t) + ',"id":' + quote(a.id) + ',"msg":' + quote(a.msg) + '}');
  }
}

// setupComplete emits the lifecycle line HYDO-SETUP-COMPLETE: the system is provisioned and
// ready, faults may begin, and the point is a good snapshot/branch base.
function setupComplete() {
  writeLine(Buffer.from('HYDO-SETUP-COMPLETE\n', 'utf8'));
}

// ---------------------------------------------------------------------------------------------
// Eventually: bounded liveness. After an anchor, the condition becomes true within a deadline
// measured on the monotonic clock (deterministic under hydo). One record per episode, with
// "elapsed_ms" and "within_ms" in the details; each outcome at most once per process.

function nowNs() { return process.hrtime.bigint(); }

class Watch {
  constructor(l) {
    this.l = l;
    this.start = nowNs();
    this.finished = false;
  }

  finish(ok, d) {
    if (this.finished) return ok;
    this.finished = true;
    this.l.a.record(ok, withFields(d, {
      elapsed_ms: Number((nowNs() - this.start) / 1000000n),
      within_ms: Math.trunc(this.l.withinMs),
    }));
    return ok;
  }

  // done records that the condition became true now: ok if within the deadline, else a
  // violation (true, but too late). Returns whether it was in time.
  done(d) { return this.finish(nowNs() - this.start <= this.l.withinNs, d); }

  // expire records that the deadline passed with the condition still false (a violation).
  expire(d) { this.finish(false, d); }
}

let sleepCell = null;
function sleepSync(ms) {
  if (sleepCell === null) sleepCell = new Int32Array(new SharedArrayBuffer(4));
  Atomics.wait(sleepCell, 0, 0, Math.max(0, ms));
}

class Liveness {
  constructor(id, msg, withinMs) {
    this.a = declare(T_EVENTUALLY, id, msg);
    this.withinMs = Number(withinMs);
    this.withinNs = BigInt(Math.round(this.withinMs * 1e6));
  }

  get id() { return this.a.id; }

  // begin starts an episode now (the anchor).
  begin() { return new Watch(this); }

  // awaitCond starts an episode now, evaluates cond (which may return a promise) every
  // pollMs until it is true or the deadline has passed, records the outcome and resolves to
  // it. The last evaluation happens at or after the deadline, so a condition that becomes
  // true exactly at the deadline still counts.
  async awaitCond(cond, pollMs, d) {
    const w = this.begin();
    for (;;) {
      if (await cond()) return w.done(d);
      if (nowNs() - w.start > this.withinNs) {
        w.expire(d);
        return false;
      }
      await new Promise((r) => setTimeout(r, pollMs));
    }
  }

  // awaitCondSync is awaitCond for synchronous code: it blocks the thread between
  // evaluations (cond must return a boolean, not a promise).
  awaitCondSync(cond, pollMs, d) {
    const w = this.begin();
    for (;;) {
      if (cond()) return w.done(d);
      if (nowNs() - w.start > this.withinNs) {
        w.expire(d);
        return false;
      }
      sleepSync(pollMs);
    }
  }
}

// eventually declares a bounded-liveness property: after an anchor, the condition holds within
// withinMs milliseconds.
function eventually(id, msg, withinMs) { return new Liveness(id, msg, withinMs); }

// ---------------------------------------------------------------------------------------------
// BUGGIFY: code-level fault points (FoundationDB style), the Go SDK's decisions exactly.
//
// Configured by $HYDO_BUGGIFY, or else the file $HYDO_BUGGIFY_FILE (default /tmp/hydo-buggify),
// holding "<seed-hex>[,<enable-permille>[,<fire-permille>]]" (defaults 250 and 250). Under hydo
// the guest init writes the schedule's B= token to that file. Read once, at the first
// evaluation of any point. A point is enabled for the run when
// splitmix64(seed ^ fnv1a(id)) % 1000 < enable; its k-th evaluation (k = 1, 2, ...) fires when
// splitmix64(splitmix64(seed ^ fnv1a(id)) + k * 0x9e3779b97f4a7c15) % 1000 < fire.

const M64 = (1n << 64n) - 1n;
const GOLDEN = 0x9e3779b97f4a7c15n;

function bgMix(z) { // splitmix64 finalizer
  z = (z + GOLDEN) & M64;
  z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & M64;
  z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & M64;
  return z ^ (z >> 31n);
}

function bgKey(id) { // FNV-1a over the UTF-8 bytes
  let h = 14695981039346656037n;
  for (const b of Buffer.from(id, 'utf8')) {
    h ^= BigInt(b);
    h = (h * 1099511628211n) & M64;
  }
  return h;
}

// Go's strings.TrimSpace (unicode.IsSpace), which differs from String#trim.
const GO_SPACE = new Set([0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0x85, 0xa0, 0x1680,
  0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a,
  0x2028, 0x2029, 0x202f, 0x205f, 0x3000]);
function goTrimSpace(s) {
  let a = 0;
  let b = s.length;
  while (a < b && GO_SPACE.has(s.charCodeAt(a))) a++;
  while (b > a && GO_SPACE.has(s.charCodeAt(b - 1))) b--;
  return s.slice(a, b);
}

// Go's strconv.ParseUint(s, base, 64) for base 16 or 10; null on any error.
function parseUint64(s, base) {
  if (!(base === 16 ? /^[0-9a-fA-F]+$/ : /^[0-9]+$/).test(s)) return null;
  const v = BigInt((base === 16 ? '0x' : '') + s);
  return v > M64 ? null : v;
}

const points = [];
let bgLoaded = false;
let bgOn = false;
let bgSeed = 0n;
let bgEnable = 250n;
let bgFire = 250n;

class Point {
  constructor(id, msg) {
    this.id = String(id);
    this.a = declare(T_SOMETIMES, 'buggify.' + this.id, 'fault point fired: ' + String(msg));
    this.hits = 0n;
    this.key = bgKey(this.id);
  }

  enabled() { return bgMix(bgSeed ^ this.key) % 1000n < bgEnable; }

  // fire reports whether the point misbehaves on this evaluation. Always false when BUGGIFY is
  // not configured; then it has no other effect either. When it fires, {"hit": k} is added
  // to the details (the caller's object, as the Go SDK adds it to the caller's map) and the
  // point's sometimes property is recorded.
  fire(d) {
    if (!bgLoaded) bgLoad();
    if (!bgOn || !this.enabled()) return false;
    this.hits = (this.hits + 1n) & M64;
    const k = this.hits;
    const f = bgMix((bgMix(bgSeed ^ this.key) + k * GOLDEN) & M64) % 1000n < bgFire;
    if (f) {
      this.a.record(true, withFields(d, { hit: k }));
    }
    return f;
  }
}

function bgLoad() {
  bgLoaded = true;
  let cfg = process.env.HYDO_BUGGIFY || '';
  if (cfg === '') {
    const f = process.env.HYDO_BUGGIFY_FILE || '/tmp/hydo-buggify';
    try {
      cfg = fs.readFileSync(f, 'utf8');
    } catch (e) {
      cfg = '';
    }
  }
  const parts = goTrimSpace(cfg).split(',');
  const seed = parseUint64(parts[0], 16);
  if (seed === null) return; // no configuration: every point stays off
  bgOn = true;
  bgSeed = seed;
  if (parts.length > 1) {
    const v = parseUint64(parts[1], 10);
    if (v !== null) bgEnable = v;
  }
  if (parts.length > 2) {
    const v = parseUint64(parts[2], 10);
    if (v !== null) bgFire = v;
  }
  const enabled = points.filter((p) => p.enabled()).map((p) => p.id);
  emitRaw('HYDO-BUGGIFY', '{"seed":' + quote(bgSeed.toString(16)) + ',"enable_pm":' + bgEnable.toString() +
    ',"fire_pm":' + bgFire.toString() + ',"enabled":' + encodeArray(enabled, []) + '}');
}

// buggify declares a fault point.
function buggify(id, msg) {
  const p = new Point(id, msg);
  points.push(p);
  return p;
}

// For tests: forget the sink and the BUGGIFY configuration (declarations are kept).
function _reset() {
  if (sinkState === SINK_FD && sinkFd > 2) {
    try { fs.closeSync(sinkFd); } catch (e) { /* ignore */ }
  }
  sinkState = SINK_UNSET;
  sinkFd = -1;
  bgLoaded = false;
  bgOn = false;
  bgSeed = 0n;
  bgEnable = 250n;
  bgFire = 250n;
  for (const p of points) p.hits = 0n;
  for (const a of catalogList) a.seen = [false, false];
}

module.exports = {
  always,
  alwaysOrUnreachable,
  sometimes,
  reachable,
  unreachable,
  eventually,
  buggify,
  catalog,
  setupComplete,
  emitLine,
  toJSON,
  Assertion,
  Liveness,
  Watch,
  Point,
  types: Object.freeze({
    ALWAYS: T_ALWAYS,
    ALWAYS_OR_UNREACHABLE: T_ALWAYS_OR_UNREACHABLE,
    SOMETIMES: T_SOMETIMES,
    REACHABLE: T_REACHABLE,
    UNREACHABLE: T_UNREACHABLE,
    EVENTUALLY: T_EVENTUALLY,
  }),
  _reset,
};

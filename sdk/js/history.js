'use strict';
// hydo-assert/history: hydo's operation-history recorder for Node.js guest programs.
//
// A workload client records every operation it performs -- the invocation before the request,
// the completion after the reply -- and the host checks the history after the run
// (linearizability of a register / CAS object per key, Elle-style list-append checks). The
// lines are byte for byte those of the Go recorder (hydo/stack/hydohist); the wire format and
// the checkers are in hv/hvx/HISTORIES.md.
//
//   const { History, enc, ver, valueOrNil } = require('hydo-assert/history');
//   const kv = new History('kv');
//   let op = kv.invoke(3, 'write', 'k1', enc(v));   // process 3, before the request
//   try { const rev = await put('k1', v); op.ok('k1', ver(v, rev)); }
//   catch (e) { if (rejected(e)) op.fail(); else op.info(); }   // info = unknown outcome
//   op = kv.invoke(3, 'read', 'k1');
//   op.ok('k1', valueOrNil(await get('k1')));       // null / undefined = absent
//
//   HYDO-HIST <history> <process> <invoke|ok|fail|info> <time-ns> <f> <arg>...
//
// One record = one line = one fs.writeSync() on the guest console (/dev/console;
// HYDO_HIST_OUT overrides it: a path, "stderr" or "off"). The host orders operations by the
// position of their records, so record the invocation before sending and the completion after
// receiving. A process has at most one operation in flight: give each concurrent client its
// own process number. The time field is CLOCK_MONOTONIC in ns (process.hrtime.bigint(),
// deterministic under hydo); the checkers do not order by it.
//
// Strings are written as UTF-8. A JavaScript string cannot hold invalid UTF-8; a lone
// surrogate in it is written (and encoded by enc) as U+FFFD, as Buffer.from and TextEncoder
// convert it.
//
// This module is separate from the assertion SDK (index.js) and has its own sink: requiring
// it changes nothing that the assertion SDK writes. history.mjs re-exports it for ESM, so a
// program that loads it both ways shares one sink.

const fs = require('fs');

/** The value of an absent key (register model); the empty list is "[]". */
const NIL = 'nil';

const INT64_MIN = -(1n << 63n);
const INT64_MAX = (1n << 63n) - 1n;

// formatInt: an integer (a number that is an integer, or a bigint) in decimal, as Go's
// strconv.FormatInt writes an int64.
function formatInt(x, what) {
  let b;
  if (typeof x === 'bigint') b = x;
  else if (typeof x === 'number' && Number.isInteger(x)) b = BigInt(x);
  else throw new TypeError(`hydo-assert/history: ${what} must be an integer (number or bigint), got ${typeof x === 'number' ? x : typeof x}`);
  if (b < INT64_MIN || b > INT64_MAX) throw new RangeError(`hydo-assert/history: ${what} ${b} is outside int64`);
  return b.toString();
}

// token: a record argument. A string is taken as is (it must already be one token: encode
// free-form values with enc); an integer is written in decimal.
function token(x, what) {
  if (typeof x === 'string') return x;
  return formatInt(x, what);
}

// Bytes 0x00-0x7f: the byte itself, or %XX for '%', '@', whitespace and control characters
// (<= ' '), DEL and the list / txn separators ',[]:' (Go's Enc).
const ASCII = [];
for (let c = 0; c < 0x80; c++) {
  const esc = c === 0x25 || c === 0x40 || c <= 0x20 || c === 0x7f || ',[]:'.includes(String.fromCharCode(c));
  ASCII.push(esc ? '%' + c.toString(16).toUpperCase().padStart(2, '0') : String.fromCharCode(c));
}

/**
 * enc makes a value one token, working on the bytes of its UTF-8 form exactly as Go's
 * hydohist.Enc does: '%', '@', whitespace, control characters, DEL and ',[]:' become %XX,
 * other bytes (all of non-ASCII UTF-8) stay; "" is "%" and "nil" is "%6Eil" (the token nil
 * means absent). An integer argument is first written in decimal.
 */
function enc(s) {
  s = token(s, 'enc value');
  if (s === '') return '%';
  if (s === NIL) return '%6Eil';
  const b = Buffer.from(s, 'utf8');
  let out = '';
  let i = 0;
  while (i < b.length) {
    const c = b[i];
    if (c < 0x80) {
      out += ASCII[c];
      i++;
      continue;
    }
    // A run of bytes >= 0x80 is a run of whole characters (ASCII never occurs inside a UTF-8
    // sequence), so it decodes back to exactly those characters.
    let j = i + 1;
    while (j < b.length && b[j] >= 0x80) j++;
    out += b.toString('utf8', i, j);
    i = j;
  }
  return out;
}

/** ver is value v (encoded) with the version the store assigned it: "v@83" (int64). */
function ver(v, version) {
  return enc(v) + '@' + formatInt(version, 'version');
}

/**
 * valueOrNil is enc(v) when the value is present, else NIL. With one argument a value of null
 * or undefined is absent; with two (Go's ValOrNil(v, ok)) the second says whether it is
 * present.
 */
function valueOrNil(v, ok) {
  const present = ok === undefined ? v !== null && v !== undefined : Boolean(ok);
  return present ? enc(v) : NIL;
}

/** list is a list value "[e1,e2,...]" (list-append reads), each element encoded. */
function list(...elems) {
  return '[' + elems.map((e) => enc(e)).join(',') + ']';
}

/** List-append txn micro-ops: "a:<k>:<e>", "r:<k>" (invocation), "r:<k>:[...]" (completion). */
function append(k, e) {
  return 'a:' + enc(k) + ':' + enc(e);
}
function read(k) {
  return 'r:' + enc(k);
}
function readResult(k, ...l) {
  return 'r:' + enc(k) + ':' + list(...l);
}

// ---------------------------------------------------------------------------------------------
// Sink: opened once, at the first record, as the Go recorder opens it.
//
// HYDO_HIST_OUT unset or empty: /dev/console; "off": nothing; "stderr": standard error;
// anything else: that file, appended and created if missing (mode 0644). If it cannot be
// opened the records go to standard error.

const SINK_UNSET = 0;
const SINK_OFF = 1;
const SINK_FD = 2;
let sinkState = SINK_UNSET;
let sinkFd = -1;

function openSink() {
  let p = process.env.HYDO_HIST_OUT || '';
  if (p === 'off') {
    sinkState = SINK_OFF;
    return;
  }
  sinkState = SINK_FD;
  if (p === 'stderr') {
    sinkFd = 2;
    return;
  }
  if (p === '') p = '/dev/console';
  const c = fs.constants;
  const flags = c.O_WRONLY | c.O_APPEND | c.O_CREAT | (c.O_NOCTTY || 0);
  try {
    sinkFd = fs.openSync(p, flags, 0o644);
  } catch (e) {
    sinkFd = 2;
  }
}

function writeLine(buf) {
  let off = 0;
  try {
    while (off < buf.length) {
      const n = fs.writeSync(sinkFd, buf, off, buf.length - off);
      if (n <= 0) break;
      off += n; // a short write (which the console never does) is continued, as Go's Write does
    }
  } catch (e) {
    // like the Go recorder: a failed console write is ignored
  }
}

function emit(name, proc, typ, f, args) {
  if (sinkState === SINK_UNSET) openSink();
  if (sinkState === SINK_OFF) return;
  let s = 'HYDO-HIST ' + name + ' ' + proc + ' ' + typ + ' ' + process.hrtime.bigint().toString() + ' ' + f;
  for (const a of args) s += ' ' + (a === '' ? '%' : a);
  writeLine(Buffer.from(s + '\n', 'utf8')); // one write(): never interleaved with other output
}

/** One named history (a check's `history:`; [A-Za-z0-9_.-]+). */
class History {
  constructor(name) {
    this.name = String(name);
  }

  /**
   * invoke records the invocation of f(args...) by process proc (an integer, or a token
   * without spaces) and returns the operation. args are tokens: encode free-form values with
   * enc (ver, list and NIL help); integers are written in decimal.
   */
  invoke(proc, f, ...args) {
    const op = new Op(this, token(proc, 'process'), String(f), args.map((a) => token(a, 'argument')));
    emit(this.name, op.proc, 'invoke', op.f, op.args);
    return op;
  }

  /** record writes one raw record (for programs that keep their own bookkeeping). */
  record(proc, type, f, ...args) {
    emit(this.name, token(proc, 'process'), String(type), String(f), args.map((a) => token(a, 'argument')));
  }
}

/** An invoked operation waiting for its completion. */
class Op {
  constructor(h, proc, f, args) {
    this.history = h;
    this.proc = proc;
    this.f = f;
    this.args = args;
  }

  /**
   * ok records a successful completion. args are the results in the invocation's layout (a
   * read lists <key> <value> pairs, a txn its micro-ops with read results); with no args the
   * invocation's are repeated.
   */
  ok(...args) {
    emit(this.history.name, this.proc, 'ok', this.f, args.length === 0 ? this.args : args.map((a) => token(a, 'argument')));
  }

  /** fail records that the operation definitely had no effect. */
  fail() {
    emit(this.history.name, this.proc, 'fail', this.f, this.args);
  }

  /** info records that the outcome is unknown (it may take effect at any later time, or never). */
  info() {
    emit(this.history.name, this.proc, 'info', this.f, this.args);
  }
}

/** history(name) is new History(name). */
function history(name) {
  return new History(name);
}

// _reset forgets the sink (tests): the next record re-reads HYDO_HIST_OUT.
function _reset() {
  if (sinkState === SINK_FD && sinkFd > 2) {
    try {
      fs.closeSync(sinkFd);
    } catch (e) {
      // already closed
    }
  }
  sinkState = SINK_UNSET;
  sinkFd = -1;
}

module.exports = {
  NIL,
  History,
  Op,
  history,
  enc,
  ver,
  valueOrNil,
  list,
  append,
  read,
  readResult,
  _reset,
};

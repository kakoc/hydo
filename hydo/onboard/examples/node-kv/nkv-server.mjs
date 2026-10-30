// nkv server: a tiny durable key/value store over HTTP on loopback, written to show the
// JavaScript assertion SDK (sdk/js, package hydo-assert) under hydo. ESM on purpose: the SDK
// is loaded through its ESM entry here and through its CommonJS entry by props.cjs.
//
//   node nkv-server.mjs <dir> <port>     append-only log <dir>/log, one "PUT k v" per line
//
// PUT /k/<key> (body = value) is acknowledged with "OK" after the log line is written and,
// unless /tmp/nkv-fsync says "never" (read at every PUT; knob S), after fdatasync.
// GET /k/<key> returns the value (404 if absent); GET /ping returns "pong".
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { catalog } from 'hydo-assert';
import P from './props.cjs';

const [dir, port] = process.argv.slice(2);
const logPath = path.join(dir, 'log');
const MAX_LOG = 1 << 20;
const map = new Map();

function replay() {
  let buf = Buffer.alloc(0);
  try { buf = fs.readFileSync(logPath); } catch (e) { /* no log yet */ }
  const complete = buf.lastIndexOf(0x0a) + 1;
  if (complete < buf.length) P.TORN.hit({ bytes: buf.length - complete });
  for (const line of buf.subarray(0, complete).toString('utf8').split('\n')) {
    if (line === '') continue;
    const m = /^PUT (\S+) (.*)$/.exec(line);
    P.LOG_PARSES.check(m !== null, { line });
    if (m) map.set(m[1], m[2]);
  }
  P.RECOVERED.check(map.size > 0, { keys: map.size });
  if (complete < buf.length) fs.truncateSync(logPath, complete); // drop the unacknowledged tail
  return complete;
}

catalog();
let size = replay();
const fd = fs.openSync(logPath, 'a');

function fsyncMode() {
  try { return fs.readFileSync('/tmp/nkv-fsync', 'utf8').trim(); } catch (e) { return 'always'; }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function put(key, value, req, res) {
  const old = map.get(key);
  if (old !== undefined && old !== value) P.OVERWRITE.hit({ key, old, value });
  const line = Buffer.from('PUT ' + key + ' ' + value + '\n');
  fs.writeSync(fd, line);
  size += line.length;
  P.LOG_SIZE.lessThanOrEqualTo(size, MAX_LOG);
  if (fsyncMode() !== 'never') {
    if (P.SLOW_FSYNC.fire({ key })) await sleep(3000);
    fs.fdatasyncSync(fd);
  }
  map.set(key, value);
  if (P.LOST_REPLY.fire({ key })) {
    req.socket.destroy(); // durable, but the client never hears about it
    return;
  }
  res.end('OK');
}

http.createServer((req, res) => {
  const m = /^\/k\/([^/]+)$/.exec(req.url);
  if (req.method === 'GET' && req.url === '/ping') return res.end('pong');
  if (req.method === 'GET' && m) {
    const v = map.get(decodeURIComponent(m[1]));
    if (v === undefined) { res.statusCode = 404; return res.end(); }
    return res.end(v);
  }
  if (req.method === 'PUT' && m) {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      put(decodeURIComponent(m[1]), Buffer.concat(chunks).toString('utf8'), req, res).catch((e) => {
        res.statusCode = 500;
        res.end(String(e));
      });
    });
    return undefined;
  }
  res.statusCode = 400;
  return res.end();
}).listen(Number(port), '127.0.0.1');

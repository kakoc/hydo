'use strict';
// nkv client and oracle (CommonJS; the server is nkv-server.mjs).
//
//   node nkv.cjs put   <port> <key> <value>   prints OK (exit 0) or ERR <why> (exit 1); 2 s deadline
//   node nkv.cjs ping  <port>                 exit 0 when the server answers (readiness; no records)
//   node nkv.cjs check <port> <acked-file>    oracle: every acknowledged key k:<i> reads back as v<i>
//                                             (the file lists the acknowledged step numbers i)
const http = require('http');
const fs = require('fs');
const P = require('./props.cjs');

function request(port, method, p, body, timeoutMs) {
  return new Promise((resolve) => {
    const req = http.request({ host: '127.0.0.1', port: Number(port), method, path: p, agent: false, timeout: timeoutMs }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve({ status: res.statusCode, body: Buffer.concat(chunks).toString('utf8') }));
      res.on('error', (e) => resolve({ error: String(e) }));
    });
    req.on('timeout', () => req.destroy(new Error('deadline')));
    req.on('error', (e) => resolve({ error: e.message }));
    req.end(body);
  });
}

async function main() {
  const [cmd, port, a, b] = process.argv.slice(2);
  if (cmd === 'put') {
    const r = await request(port, 'PUT', '/k/' + encodeURIComponent(a), b, 2000);
    if (r.status === 200 && r.body === 'OK') {
      console.log('OK');
      return 0;
    }
    P.PUT_UNACKED.check(true, { key: a, why: r.error || ('status ' + r.status) });
    console.log('ERR ' + (r.error || r.status));
    return 1;
  }
  if (cmd === 'ping') {
    const r = await request(port, 'GET', '/ping', undefined, 1000);
    return r.body === 'pong' ? 0 : 1;
  }
  if (cmd === 'check') {
    P.ha.catalog(); // the oracle runs in every run's console, so it carries the catalog
    const up = await P.SERVES.awaitCond(async () => (await request(port, 'GET', '/ping', undefined, 1000)).body === 'pong', 100);
    if (!up) {
      console.log('the server did not answer within 5 s');
      return 1;
    }
    let acked = [];
    try { acked = fs.readFileSync(a, 'utf8').split('\n').filter((l) => l !== ''); } catch (e) { /* nothing acknowledged */ }
    for (const i of acked) {
      const r = await request(port, 'GET', '/k/k:' + i, undefined, 2000);
      const got = r.status === 200 ? r.body : null;
      if (!P.ACKED_DURABLE.check(got === 'v' + i, { key: 'k:' + i, got, want: 'v' + i })) {
        console.log('key k:' + i + ' lost (got ' + JSON.stringify(got) + ')');
        return 1;
      }
    }
    console.log('acked=' + acked.length + ' all present');
    return 0;
  }
  console.error('usage: nkv.cjs put|ping|check ...');
  return 2;
}

main().then((rc) => process.exit(rc));

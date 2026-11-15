// The shop's web server, as it runs in production: the built app from APP_DIST, index.html for
// every other path (the app routes in the browser), and /api/* passed on to API_URL.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const dist = path.resolve(process.env.APP_DIST || new URL('./dist-fixed', import.meta.url).pathname);
const api = new URL(process.env.API_URL || 'http://127.0.0.1:9001');
const port = Number(process.env.PORT || 3000);
const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
  '.json': 'application/json',
};

http
  .createServer((req, res) => {
    if (req.url.startsWith('/api/')) {
      const up = http.request(
        { host: api.hostname, port: api.port, method: req.method, path: req.url, headers: { ...req.headers, host: api.host } },
        (answer) => {
          res.writeHead(answer.statusCode, answer.headers);
          answer.pipe(res);
        },
      );
      up.on('error', () => {
        if (!res.headersSent) res.writeHead(502);
        res.end();
      });
      res.on('close', () => up.destroy()); // the browser gave up: so does the server
      req.pipe(up);
      return;
    }
    let file = path.join(dist, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!file.startsWith(dist) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
      file = path.join(dist, 'index.html');
    }
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' });
    fs.createReadStream(file).pipe(res);
  })
  .listen(port, () => console.log(`shop on http://localhost:${port}, API at ${api.href}`));

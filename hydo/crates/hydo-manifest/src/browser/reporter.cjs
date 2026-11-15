// hydo's Playwright reporter (Milestone 253, docs/design/web-apps.md): one line per test on
// standard output, and the same in $HYDO_WEB_DIR/events.jsonl for check.mjs.
'use strict';
const fs = require('fs');
const path = require('path');

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';

function brief(error) {
  if (!error) return '';
  const text = String(error.message || error.value || '').replace(/\u001b\[[0-9;]*m/g, '');
  const lines = text.split('\n').map((l) => l.trim()).filter(Boolean);
  const keep = lines.filter((l, i) => i === 0 || /^(Expected|Received|Locator|Timeout|Call log)/.test(l));
  return keep.slice(0, 4).join(' / ').slice(0, 300);
}

class HydoReporter {
  printsToStdio() {
    return true;
  }

  onTestEnd(test, result) {
    const title = test.titlePath().filter(Boolean).slice(1).join(' › ');
    const ev = {
      t: Date.now(),
      kind: 'test',
      test: title,
      file: path.basename(test.location.file),
      status: result.status,
      expected: test.expectedStatus,
      ms: result.duration,
      error: brief(result.error),
    };
    fs.mkdirSync(dir, { recursive: true });
    fs.appendFileSync(path.join(dir, 'events.jsonl'), JSON.stringify(ev) + '\n');
    const secs = (result.duration / 1000).toFixed(1);
    process.stdout.write(`hydo-web: test "${title}" ${result.status} in ${secs} s${ev.error ? `: ${ev.error}` : ''}\n`);
  }
}

module.exports = HydoReporter;

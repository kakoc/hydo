'use strict';
// The README (the npm page) is tested like code (node --test).
//
// Every `js` block of README.md runs as its own program, in a fresh Node.js process from a
// directory whose node_modules/hydo-assert is this package, and must exit with status 0. The
// complete program's records must be exactly the `text` block that follows it in the README.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const PKG = path.join(__dirname, '..');
const README = fs.readFileSync(path.join(PKG, 'README.md'), 'utf8');

// [info string, text, the heading above it] of every fenced block of the README.
function blocks() {
  const out = [];
  let heading = null;
  let info = null;
  let lines = [];
  for (const line of README.split('\n')) {
    if (info === null && line.startsWith('#')) heading = line.trim();
    if (line.startsWith('```')) {
      if (info === null) {
        info = line.slice(3).trim();
        lines = [];
      } else {
        out.push([info, lines.join('\n') + '\n', heading]);
        info = null;
      }
    } else if (info !== null) {
      lines.push(line);
    }
  }
  return out;
}

// Runs `code` as main.js in a new directory that has this package as node_modules/hydo-assert.
function run(code, env) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hydo-js-readme-'));
  fs.mkdirSync(path.join(dir, 'node_modules'));
  fs.symlinkSync(PKG, path.join(dir, 'node_modules', 'hydo-assert'), 'dir');
  fs.writeFileSync(path.join(dir, 'main.js'), code);
  const base = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('HYDO_')));
  const p = spawnSync(process.execPath, ['main.js'], {
    cwd: dir,
    env: { ...base, HYDO_BUGGIFY_FILE: path.join(dir, 'no-such-file'), ...env },
    encoding: 'utf8',
    timeout: 60000,
  });
  return { p, dir };
}

test('every js block of the README runs', () => {
  const found = blocks().filter((b) => b[0] === 'js');
  assert.ok(found.length >= 6, `only ${found.length} js blocks`);
  for (const [, text, heading] of found) {
    const { p, dir } = run(text, { HYDO_ASSERT_OUT: 'off', HYDO_HIST_OUT: 'off' });
    fs.rmSync(dir, { recursive: true, force: true });
    assert.strictEqual(p.status, 0, `${heading}:\n${text}\n${p.stderr}`);
  }
});

test('the complete program writes what the README shows', () => {
  const bs = blocks();
  const i = bs.findIndex((b) => b[2] === '## A complete program' && b[0] === 'js');
  assert.ok(i >= 0);
  assert.strictEqual(bs[i + 1][0], 'text');
  const dir0 = fs.mkdtempSync(path.join(os.tmpdir(), 'hydo-js-readme-out-'));
  const out = path.join(dir0, 'console');
  fs.writeFileSync(out, '');
  const { p, dir } = run(bs[i][1], { HYDO_ASSERT_OUT: out });
  assert.strictEqual(p.status, 0, p.stderr);
  assert.strictEqual(fs.readFileSync(out, 'utf8'), bs[i + 1][1]);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.rmSync(dir0, { recursive: true, force: true });
});

test('every link of the README is absolute', () => {
  // npm shows the README outside the repository
  for (const m of README.matchAll(/\]\(([^)]*)\)/g)) {
    assert.ok(m[1].startsWith('https://'), m[1]);
  }
});

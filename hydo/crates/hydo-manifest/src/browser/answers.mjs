// hydo's check that every page shows the newest answer it asked for (Milestone 393,
// `browser.watch.answers`).
//
//   node answers.mjs
//
// It reads $HYDO_WEB_DIR/answers.jsonl, which answers.cjs wrote, and prints one sentence. Exit 0:
// no page showed an older answer after a newer one came (also when no answer came after a newer
// one); 1: a page did.
import fs from 'node:fs';
import path from 'node:path';

const dir = process.env.HYDO_WEB_DIR || '/tmp/hydo/web';
if (!fs.existsSync(dir)) {
  console.log('no page ran in this boot (a recovery boot after a power cut runs no workload)');
  process.exit(0);
}
let lines = [];
try {
  lines = fs.readFileSync(path.join(dir, 'answers.jsonl'), 'utf8').split('\n').filter(Boolean);
} catch {
  lines = [];
}
const ev = lines.map((l) => JSON.parse(l));
const quote = (v) => `"${v.length > 40 ? `${v.slice(0, 40)}…` : v}"`;
const list = (vs) => vs.map(quote).join(', ');
const stale = ev.filter((e) => e.kind === 'stale');
if (stale.length) {
  const e = stale[0];
  const method = e.key.split(' ')[0];
  const more = stale.length > 1 ? ` (${stale.length} times in this run)` : '';
  console.log(
    `The page ${e.url} shows the answer of an older request after the answer of a newer one${more}: ` +
      `${method} ${e.older.path} was sent first and answered last, after ${method} ${e.newer.path}, ` +
      `and the page shows ${list(e.shows)} of the older answer and none of ${list(e.misses)} of the newer one.`,
  );
  process.exit(1);
}
const newest = ev.filter((e) => e.kind === 'newest').length;
const undecided = ev.filter((e) => e.kind === 'undecided').length;
if (!newest && !undecided) {
  console.log('no answer came after the answer of a newer request of the same method and path, so no page could show an older one');
} else {
  const u = undecided ? `; ${undecided} could not be judged (the page showed neither answer, or the two answers held the same values)` : '';
  console.log(`${newest} time${newest === 1 ? '' : 's'} an answer came after a newer one's, and the page showed the newer one${u}`);
}
process.exit(0);

// saved_memos.mjs <a copy of memos_prod.db> <$HYDO_ACKED>: Memos's own promise that a memo it
// saved stays saved (Milestone 258). The walk writes one word to $HYDO_ACKED for each resource
// name in a write that the server answered 200: `<method>:<name>`, such as
// CreateMemo:memos/T5k4x8o58vBLJfJSXPXNS4. A power cut keeps the file for the recovery boot.
// Every memo whose CreateMemo the server answered, and whose DeleteMemo it never answered, must
// be a row of the table memo.
// Exit 0: it holds (also when the walk saved no memo); 1: it does not; 3: the copy of the
// database could not be read (a copy taken while the server wrote it), so the invariant copies it
// again.
import fs from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

const [dbPath, acked] = process.argv.slice(2);
let words = [];
try {
  words = fs.readFileSync(acked, 'utf8').split(/\s+/).filter(Boolean);
} catch {
  words = [];
}
const created = new Map(); // uid -> the order of its acknowledgement
const deleted = new Set();
let n = 0;
for (const w of words) {
  const m = /^(CreateMemo|DeleteMemo):memos\/([A-Za-z0-9_-]+)$/.exec(w);
  if (!m) continue;
  if (m[1] === 'CreateMemo') created.set(m[2], ++n);
  else deleted.add(m[2]);
}
const want = [...created.keys()].filter((u) => !deleted.has(u));
if (want.length === 0) {
  console.log('the walk saved no memo, so no memo can be missing');
  process.exit(0);
}
let rows;
try {
  const db = new DatabaseSync(dbPath);
  rows = new Set(
    db
      .prepare('SELECT uid FROM memo')
      .all()
      .map((r) => r.uid),
  );
} catch (e) {
  console.log(`the database ${dbPath} could not be read: ${e.message}`);
  process.exit(3);
}
const lost = want.filter((u) => !rows.has(u));
if (lost.length) {
  console.log(
    `Memos answered that it saved the memo memos/${lost[0]}, and the database does not have it` +
      `${lost.length > 1 ? ` (${lost.length} of ${want.length} saved memos are missing)` : ''}.`,
  );
  process.exit(1);
}
console.log(`all ${want.length} memos that Memos answered as saved are in the database`);

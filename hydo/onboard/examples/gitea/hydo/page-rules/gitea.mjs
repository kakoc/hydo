// gitea.mjs <rule> <a copy of gitea.db> <$HYDO_ACKED>: Gitea's own promises over its database
// (Milestone 389). The walk writes one word to $HYDO_ACKED for each write that the server answered
// 2xx: `<METHOD>:<path>`, such as POST:/hydo/demo/issues/new or POST:/hydo/demo/issues/2/comments.
// Gitea's pages send their forms with fetch and answer {"redirect": "/hydo/demo/issues/3"}, with
// no `id` field, so a word names the kind of write and the issue of a comment, not the new object.
// A power cut keeps the file for the recovery boot.
//
//   saved     every issue that Gitea answered as created is in its repository: a repository has
//             at least as many issues as the walk created there (the walk never deletes an
//             issue). Every issue that got an acknowledged comment, a close or a reopen has at
//             least that many such comments.
//   counters  the numbers that Gitea keeps beside its rows agree with the rows: a repository's
//             open and closed issues, an issue's comments, a label's and a milestone's open and
//             closed issues, and the repository's last issue number. Gitea shows these numbers on
//             its pages (the "Issues 3" tab, "2 Open 1 Closed", a label's count).
//
// Exit 0: it holds (also when the walk created nothing); 1: it does not; 3: the copy of the
// database could not be read (a copy taken while the server wrote it), so the invariant copies it
// again.
import fs from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

const [rule, dbPath, acked] = process.argv.slice(2);
let words = [];
try {
  words = fs.readFileSync(acked, 'utf8').split(/\s+/).filter(Boolean);
} catch {
  words = [];
}

let db;
const all = (sql, ...a) => db.prepare(sql).all(...a);
try {
  db = new DatabaseSync(dbPath);
  all('SELECT count(*) FROM issue');
} catch (e) {
  console.log(`the database ${dbPath} could not be read: ${e.message}`);
  process.exit(3);
}

// comment types that a comment form's POST makes: a comment, a reopen, a close
// (models/issues/comment.go, lines 68 to 71: CommentTypeComment 0, CommentTypeReopen 1,
// CommentTypeClose 2)
const FORM_TYPES = '(0, 1, 2)';

if (rule === 'saved') {
  const made = new Map(); // owner/repo -> issues created
  const commented = new Map(); // owner/repo#index -> comment posts
  for (const w of words) {
    let m;
    if ((m = /^POST:\/([^/]+)\/([^/]+)\/issues\/new$/.exec(w))) {
      const k = `${m[1]}/${m[2]}`.toLowerCase();
      made.set(k, (made.get(k) || 0) + 1);
    } else if ((m = /^POST:\/([^/]+)\/([^/]+)\/issues\/(\d+)\/comments$/.exec(w))) {
      const k = `${m[1]}/${m[2]}#${m[3]}`.toLowerCase();
      commented.set(k, (commented.get(k) || 0) + 1);
    }
  }
  const n = [...made.values()].reduce((a, b) => a + b, 0) + [...commented.values()].reduce((a, b) => a + b, 0);
  if (n === 0) {
    console.log('the walk created nothing, so nothing can be missing');
    process.exit(0);
  }
  const lost = [];
  const repoOf = (k) => all('SELECT id FROM repository WHERE lower(owner_name) = ? AND lower_name = ?', ...k.split('/'))[0];
  for (const [k, count] of made) {
    const r = repoOf(k);
    const have = r ? all('SELECT count(*) AS c FROM issue WHERE repo_id = ? AND is_pull = 0', r.id)[0].c : 0;
    if (have < count) lost.push(`${count - have} of the ${count} issues created in ${k}`);
  }
  for (const [k, count] of commented) {
    const [repo, index] = k.split('#');
    const r = repoOf(repo);
    const i = r ? all('SELECT id FROM issue WHERE repo_id = ? AND "index" = ?', r.id, Number(index))[0] : undefined;
    const have = i ? all(`SELECT count(*) AS c FROM comment WHERE issue_id = ? AND type IN ${FORM_TYPES}`, i.id)[0].c : 0;
    if (have < count) {
      lost.push(`${count - have} of the ${count} comments, closes and reopens of ${k}`);
      // the issue's own rows, for the judgment: its state and every event of its timeline
      if (i) {
        const st = all('SELECT is_closed, closed_unix, created_unix, updated_unix FROM issue WHERE id = ?', i.id)[0];
        const ev = all('SELECT id, type, created_unix FROM comment WHERE issue_id = ? ORDER BY id', i.id);
        console.log(`${k}: is_closed=${st.is_closed} closed_unix=${st.closed_unix} created_unix=${st.created_unix}; events ${ev.map((e) => `${e.id}:${e.type}@${e.created_unix}`).join(' ')}`);
      }
    }
  }
  if (lost.length) {
    // the list goes on a line of its own: the verdict is the last line, and its words stay the
    // same for any number of lost objects, so that shrink candidates keep its signature
    console.log(`missing: ${lost.join('; ')}`);
    console.log('Gitea answered that it saved an issue or a comment, and the database does not have it.');
    process.exit(1);
  }
  console.log(`all ${n} issues, comments, closes and reopens that Gitea answered as saved are in the database`);
} else if (rule === 'counters') {
  const wrong = [];
  const say = (what, kept, rows) => wrong.push(`${what}: Gitea keeps ${kept}, the rows say ${rows}`);
  for (const r of all('SELECT id, owner_name, lower_name, num_issues, num_closed_issues FROM repository')) {
    const name = `${r.owner_name}/${r.lower_name}`;
    const c = all('SELECT count(*) AS n, coalesce(sum(is_closed), 0) AS closed FROM issue WHERE repo_id = ? AND is_pull = 0', r.id)[0];
    if (r.num_issues !== c.n) say(`the issues of ${name}`, r.num_issues, c.n);
    if (r.num_closed_issues !== c.closed) say(`the closed issues of ${name}`, r.num_closed_issues, c.closed);
    const ix = all("SELECT max_index FROM issue_index WHERE group_id = ?", r.id)[0];
    const top = all('SELECT coalesce(max("index"), 0) AS m FROM issue WHERE repo_id = ?', r.id)[0].m;
    if (ix && ix.max_index < top) say(`the last issue number of ${name}`, ix.max_index, top);
    const dup = all('SELECT "index" AS i, count(*) AS c FROM issue WHERE repo_id = ? GROUP BY "index" HAVING c > 1', r.id);
    for (const d of dup) wrong.push(`the issue number ${d.i} of ${name} belongs to ${d.c} issues`);
  }
  for (const i of all('SELECT id, repo_id, "index" AS ix, num_comments FROM issue')) {
    const c = all('SELECT count(*) AS n FROM comment WHERE issue_id = ? AND type = 0', i.id)[0].n;
    if (i.num_comments !== c) say(`the comments of the issue #${i.ix} (repository ${i.repo_id})`, i.num_comments, c);
  }
  for (const l of all('SELECT id, name, num_issues, num_closed_issues FROM label')) {
    const c = all(
      'SELECT count(*) AS n, coalesce(sum(i.is_closed), 0) AS closed FROM issue_label il JOIN issue i ON i.id = il.issue_id WHERE il.label_id = ?',
      l.id,
    )[0];
    if (l.num_issues !== c.n) say(`the issues of the label ${l.name}`, l.num_issues, c.n);
    if (l.num_closed_issues !== c.closed) say(`the closed issues of the label ${l.name}`, l.num_closed_issues, c.closed);
  }
  for (const m of all('SELECT id, name, num_issues, num_closed_issues FROM milestone')) {
    const c = all('SELECT count(*) AS n, coalesce(sum(is_closed), 0) AS closed FROM issue WHERE milestone_id = ?', m.id)[0];
    if (m.num_issues !== c.n) say(`the issues of the milestone ${m.name}`, m.num_issues, c.n);
    if (m.num_closed_issues !== c.closed) say(`the closed issues of the milestone ${m.name}`, m.num_closed_issues, c.closed);
  }
  if (wrong.length) {
    console.log(`wrong: ${wrong.join('; ')}`);
    console.log('A number that Gitea keeps beside its rows does not match the rows.');
    process.exit(1);
  }
  const k = all('SELECT count(*) AS n FROM issue')[0].n;
  console.log(`the counts of every repository, issue, label and milestone match their rows (${k} issues)`);
} else if (rule === 'search') {
  // the search of a repository's issues (the search box of /<owner>/<repo>/issues, and
  // GET /api/v1/repos/<owner>/<repo>/issues?q=) reads Gitea's issue index, which a queue of
  // its own fills after the issue's commit; an issue in the database that the search never
  // finds is missing from the index. Each issue is looked up by the longest word of its title,
  // letters and digits only, of 4 to 20 (the index keeps no longer term, such as the walk's 300
  // letters); an issue without such a word is not judged.
  const base = process.env.GITEA_URL || 'http://10.89.0.11:3000';
  const issues = all(
    `SELECT i."index" AS ix, i.name, r.owner_name AS owner, r.lower_name AS repo FROM issue i
     JOIN repository r ON r.id = i.repo_id WHERE i.is_pull = 0`,
  );
  const judged = [];
  for (const i of issues) {
    const word = i.name
      .split(/[^A-Za-z0-9]+/)
      .filter((w) => w.length >= 4 && w.length <= 20)
      .sort((a, b) => b.length - a.length)[0];
    if (word) judged.push({ ...i, word });
  }
  if (judged.length === 0) {
    console.log(`none of the ${issues.length} issues has a word of 4 letters or digits in its title, so none is judged`);
    process.exit(0);
  }
  // the queue may still be indexing: ask again for up to 60 s
  const deadline = Date.now() + 60000;
  let missing = judged;
  let lastErr = '';
  while (missing.length && Date.now() < deadline) {
    const next = [];
    for (const i of missing) {
      try {
        const u = `${base}/api/v1/repos/${i.owner}/${i.repo}/issues?state=all&type=issues&limit=50&q=${encodeURIComponent(i.word)}`;
        const res = await fetch(u, { signal: AbortSignal.timeout(10000) });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const found = await res.json();
        if (!found.some((f) => f.number === i.ix)) next.push(i);
      } catch (e) {
        lastErr = String(e.message || e);
        next.push(i);
      }
    }
    missing = next;
    if (missing.length) await new Promise((r) => setTimeout(r, 3000));
  }
  if (missing.length && lastErr && missing.length === judged.length) {
    console.log(`the search did not answer: ${lastErr}`);
    process.exit(2);
  }
  if (missing.length) {
    console.log(`not found: ${missing.map((i) => `#${i.ix} of ${i.owner}/${i.repo} by "${i.word}"`).join('; ')}`);
    console.log('An issue in the database is not found by the search of its repository for a word of its title.');
    process.exit(1);
  }
  console.log(`the search finds every one of the ${judged.length} issues by a word of its title`);
} else if (rule === 'times') {
  // a timer of an issue ("Start timer") adds the time it ran when it stops: Gitea's own words
  // are "Stop timer" and "time spent". The rule stops every timer that still runs, as the user
  // would, through the API (POST .../issues/<n>/stopwatch/stop, as hydo, the only user), and
  // then reads every tracked time back (GET /api/v1/repos/<owner>/<repo>/times): none may be
  // below zero.
  const base = process.env.GITEA_URL || 'http://10.89.0.11:3000';
  const auth = { Authorization: `Basic ${Buffer.from('hydo:hydo-pass-1').toString('base64')}` };
  const running = all(
    `SELECT i."index" AS ix, r.owner_name AS owner, r.lower_name AS repo FROM stopwatch s
     JOIN issue i ON i.id = s.issue_id JOIN repository r ON r.id = i.repo_id`,
  );
  const call = async (method, u) => {
    const res = await fetch(`${base}${u}`, { method, headers: auth, signal: AbortSignal.timeout(15000) });
    if (!res.ok && res.status !== 409) throw new Error(`${method} ${u}: HTTP ${res.status}`);
    return res.status === 204 || res.status === 201 ? null : res.json().catch(() => null);
  };
  try {
    for (const s of running) await call('POST', `/api/v1/repos/${s.owner}/${s.repo}/issues/${s.ix}/stopwatch/stop`);
    const repos = all('SELECT owner_name AS owner, lower_name AS repo FROM repository');
    const bad = [];
    let n = 0;
    for (const r of repos) {
      const times = (await call('GET', `/api/v1/repos/${r.owner}/${r.repo}/times?limit=200`)) || [];
      for (const t of times) {
        n++;
        if (t.time < 0) bad.push(`the issue #${t.issue?.number ?? t.issue_id} of ${r.owner}/${r.repo} has a tracked time of ${t.time} s`);
      }
    }
    if (bad.length) {
      console.log(`negative: ${bad.join('; ')}`);
      console.log('A time that Gitea tracked for an issue is below zero.');
      process.exit(1);
    }
    console.log(`every one of the ${n} tracked times is zero or more (${running.length} running timers stopped first)`);
  } catch (e) {
    console.log(`the API did not answer: ${e.message}`);
    process.exit(2);
  }
} else {
  console.log(`unknown rule ${rule}`);
  process.exit(2);
}

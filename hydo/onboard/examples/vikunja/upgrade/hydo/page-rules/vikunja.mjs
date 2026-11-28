// vikunja.mjs <rule> <a copy of vikunja.db> <$HYDO_ACKED>: Vikunja's own promises over its
// database (Milestone 273). The walk writes one word to $HYDO_ACKED for each id in the answer to
// a write that the server answered 2xx: `<METHOD>:<path>=<id>`, such as
// POST:/api/v2/projects/1/tasks/bulk=6, or `<METHOD>:<path>` when the answer has no id, such as
// DELETE:/api/v2/tasks/6. A power cut keeps the file for the recovery boot.
//
//   saved   every task, project, comment, label, saved filter and team that Vikunja answered as
//           created is a row of its table, unless a write that the server answered deleted it:
//           a task in the trash can be purged or the trash emptied, and a deleted project takes
//           its tasks along.
//   kanban  on every Kanban view that keeps its own buckets, each task of the project that is
//           not in the trash is in one bucket of the view, and when the view has a done bucket,
//           the task is in it exactly when it is done (Vikunja moves a done task there, and
//           marks a task done that is moved there).
//
// Exit 0: it holds (also when the walk saved nothing); 1: it does not; 3: the copy of the
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
const all = (sql) => db.prepare(sql).all();
try {
  db = new DatabaseSync(dbPath);
  all('SELECT count(*) FROM tasks');
} catch (e) {
  console.log(`the database ${dbPath} could not be read: ${e.message}`);
  process.exit(3);
}

// Milestone 392: the upgrade example runs 2.7.1 and 2.8.0 on one database. 2.8.0's migration
// adds `deleted_at` to projects and tasks (a deleted object is kept and marked), and 2.7.1 has
// no such column, so each clause reads the schema it finds.
const has = (table, col) => all(`PRAGMA table_info(${table})`).some((c) => c.name === col);
const live = (alias, table) => (has(table, 'deleted_at') ? `${alias}deleted_at IS NULL` : '1');

const V = '/api/v[12]';
const re = (s) => new RegExp(`^${s}$`);
const W = {
  taskCreated: re(`(POST|PUT):${V}/(projects/(\\d+)/tasks(/bulk)?|tasks/\\d+/duplicate)=(\\d+)`),
  taskTrashed: re(`DELETE:${V}/tasks/(\\d+)`),
  taskGone: re(`DELETE:${V}/tasks/(\\d+)/permanent`),
  projectCreated: re(`(POST|PUT):${V}/projects(/\\d+/duplicate)?=(\\d+)`),
  projectDeleted: re(`DELETE:${V}/projects/(\\d+)(/permanent)?`),
  commentCreated: re(`(POST|PUT):${V}/tasks/(\\d+)/comments=(\\d+)`),
  commentDeleted: re(`DELETE:${V}/tasks/(\\d+)/comments/(\\d+)`),
};
// the objects that live on their own: created at a collection, deleted at their own path
const OWN = [
  ['label', 'labels', 'labels'],
  ['saved filter', 'filters', 'saved_filters'],
  ['team', 'teams', 'teams'],
];

if (rule === 'saved') {
  const tasks = new Map(); // id -> project of its creation
  const comments = new Map(); // id -> task
  const projects = new Set();
  const goneTasks = new Set();
  const trashed = new Set();
  const goneComments = new Set();
  const goneProjects = new Set();
  let projectGone = false;
  const own = OWN.map(([what, path, table]) => ({ what, table, made: new Set(), gone: new Set(), c: re(`(POST|PUT):${V}/${path}=(\\d+)`), d: re(`DELETE:${V}/${path}/(\\d+)`) }));
  for (const w of words) {
    let m;
    for (const o of own) {
      if ((m = o.c.exec(w))) o.made.add(m[2]);
      else if ((m = o.d.exec(w))) o.gone.add(m[1]);
    }
    if ((m = W.taskCreated.exec(w))) tasks.set(m[5], m[3] || '');
    else if ((m = W.taskGone.exec(w))) goneTasks.add(m[1]);
    else if ((m = W.taskTrashed.exec(w))) trashed.add(m[1]);
    else if ((m = W.projectCreated.exec(w))) projects.add(m[3]);
    else if ((m = W.projectDeleted.exec(w))) {
      goneProjects.add(m[1]);
      projectGone = true;
    } else if ((m = W.commentCreated.exec(w))) comments.set(m[3], m[2]);
    else if ((m = W.commentDeleted.exec(w))) goneComments.add(m[2]);
  }
  // a task in the trash can be purged, or the trash emptied
  for (const t of trashed) goneTasks.add(t);
  const have = (sql) => new Set(all(sql).map((r) => String(r.id)));
  const taskRows = have('SELECT id FROM tasks');
  const projectRows = have('SELECT id FROM projects');
  const commentRows = have('SELECT id FROM task_comments');
  const lost = [];
  for (const [id, project] of tasks) {
    if (goneTasks.has(id) || goneProjects.has(project) || taskRows.has(id)) continue;
    // a task moved into a project that was deleted later went with it
    if (projectGone && project === '') continue;
    lost.push(`the task ${id}`);
  }
  for (const id of projects) if (!goneProjects.has(id) && !projectGone && !projectRows.has(id)) lost.push(`the project ${id}`);
  for (const [id, task] of comments) {
    if (goneComments.has(id) || goneTasks.has(task) || projectGone || commentRows.has(id)) continue;
    lost.push(`the comment ${id} of the task ${task}`);
  }
  for (const o of own) {
    const rows = have(`SELECT id FROM ${o.table}`);
    for (const id of o.made) if (!o.gone.has(id) && !rows.has(id)) lost.push(`the ${o.what} ${id}`);
  }
  const others = own.reduce((k, o) => k + o.made.size, 0);
  const n = tasks.size + projects.size + comments.size + others;
  if (n === 0) {
    console.log('the walk created nothing, so nothing can be missing');
    process.exit(0);
  }
  if (lost.length) {
    // the list goes on a line of its own: the verdict is the last line, and its words stay the
    // same for any number of lost objects, so that shrink candidates keep its signature
    if (lost.length > 1) console.log(`missing: ${lost.join(', ')}`);
    console.log(
      `Vikunja answered that it saved ${lost[0]}, and the database does not have it` +
        ` (${lost.length} of ${n} saved objects are missing).`,
    );
    process.exit(1);
  }
  console.log(`all ${tasks.size} tasks, ${projects.size} projects, ${comments.size} comments and ${others} labels, saved filters and teams that Vikunja answered as saved are in the database`);
} else if (rule === 'kanban') {
  const views = all(
    `SELECT v.id, v.project_id, v.done_bucket_id FROM project_views v JOIN projects p ON p.id = v.project_id
     WHERE v.view_kind = 3 AND v.bucket_configuration_mode = 1 AND ${live('p.', 'projects')}`,
  );
  const bucketsOf = new Map();
  for (const b of all('SELECT id, project_view_id FROM buckets')) {
    if (!bucketsOf.has(b.project_view_id)) bucketsOf.set(b.project_view_id, new Set());
    bucketsOf.get(b.project_view_id).add(b.id);
  }
  const placed = new Map(); // `${task}/${view}` -> [bucket...]
  for (const r of all('SELECT task_id, project_view_id, bucket_id FROM task_buckets')) {
    const k = `${r.task_id}/${r.project_view_id}`;
    if (!placed.has(k)) placed.set(k, []);
    placed.get(k).push(r.bucket_id);
  }
  const tasks = all(`SELECT id, project_id, done, title FROM tasks WHERE ${live('', 'tasks')}`);
  const wrong = [];
  let judged = 0;
  for (const v of views) {
    const mine = bucketsOf.get(v.id) || new Set();
    for (const t of tasks.filter((x) => x.project_id === v.project_id)) {
      judged++;
      const at = (placed.get(`${t.id}/${v.id}`) || []).filter((b) => mine.has(b));
      if (at.length !== 1) {
        wrong.push(`the task ${t.id} is in ${at.length} buckets of the Kanban view ${v.id} of the project ${v.project_id}`);
        continue;
      }
      if (v.done_bucket_id && (at[0] === v.done_bucket_id) !== Boolean(t.done)) {
        wrong.push(
          `the task ${t.id} is ${t.done ? 'done' : 'not done'} and ${at[0] === v.done_bucket_id ? 'in' : 'not in'} the done bucket ${v.done_bucket_id} of the Kanban view ${v.id}`,
        );
      }
    }
  }
  if (wrong.length) {
    if (wrong.length > 1) console.log(`also: ${wrong.slice(1, 8).join('; ')}`);
    console.log(`On a Kanban board, ${wrong[0]} (${wrong.length} such places).`);
    process.exit(1);
  }
  console.log(`every one of ${judged} places of a task on a Kanban board is in one bucket, the done bucket exactly when the task is done`);
} else {
  console.log(`vikunja.mjs: no rule ${rule} (saved or kanban)`);
  process.exit(2);
}

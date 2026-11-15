-- What Hatchet recorded about one cancelled run whose status later became COMPLETED: the
-- worker's run records, the client's record of the cancel, the task's core row and events, and
-- its monitoring (OLAP) events. Set the run's message id first: psql -v m=x12 -f q-cancel.sql
\c app
SELECT seq, kind, m, run_id, worker, attempt, to_char(started_at, 'HH24:MI:SS.MS') AS started,
       to_char(finished_at, 'HH24:MI:SS.MS') AS finished, outcome
  FROM app_run WHERE m = :'m';
SELECT m, run_id, to_char(seen_at, 'HH24:MI:SS.MS') AS seen, status FROM app_cancel WHERE m = :'m';
SELECT run_id AS run FROM app_cancel WHERE m = :'m' \gset
\c hatchet
SELECT id, retry_count, app_retry_count, to_char(inserted_at, 'HH24:MI:SS.MS') AS inserted
  FROM v1_task WHERE workflow_run_id = :'run'::uuid;
SELECT e.event_type, e.retry_count, to_char(e.inserted_at, 'HH24:MI:SS.MS') AS at
  FROM v1_task_event e JOIN v1_task t ON t.id = e.task_id AND t.inserted_at = e.task_inserted_at
 WHERE t.workflow_run_id = :'run'::uuid ORDER BY e.id;
SELECT e.event_type, e.readable_status, e.retry_count, to_char(e.event_timestamp, 'HH24:MI:SS.MS') AS at,
       to_char(e.inserted_at, 'HH24:MI:SS.MS') AS inserted, left(coalesce(e.additional__event_message, ''), 80) AS msg
  FROM v1_task_events_olap e JOIN v1_tasks_olap t ON t.id = e.task_id AND t.inserted_at = e.task_inserted_at
 WHERE t.workflow_run_id = :'run'::uuid ORDER BY e.id;
SELECT readable_status, latest_retry_count FROM v1_tasks_olap WHERE workflow_run_id = :'run'::uuid;
SELECT readable_status FROM v1_runs_olap WHERE external_id = :'run'::uuid;

package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/hatchet-dev/hatchet/pkg/client/rest"
	"github.com/jackc/pgx/v5/pgxpool"
)

// drain waits until no run of the producer's workflows is queued or running and no scheduled
// run is waiting, as the API reports it, three times in a row a second apart; then it writes
// the messages check's `drained` record. Its deadline is HQ_DRAIN_S seconds (default 150) of
// the guest's monotonic clock, which a clock step does not move.
func drain(ctx context.Context) int {
	start := mono()
	deadline := start + envInt("HQ_DRAIN_S", 150)*int64(time.Second)
	quiet := 0
	last := ""
	for mono() < deadline {
		n, err := unfinished(ctx)
		if err != nil {
			last = err.Error()
			quiet = 0
		} else {
			last = n
			if n == "" {
				quiet++
			} else {
				quiet = 0
			}
		}
		if quiet >= 3 {
			hist.Record(99999, "ok", "drained", "tasks", "-")
			fmt.Println("drained")
			return 0
		}
		time.Sleep(time.Second)
	}
	hist.Record(99999, "ok", "drained", "tasks", "-")
	fmt.Println("drain: deadline with", last)
	return 0
}

// unfinished names what is still queued, running or waiting ("" when nothing is).
func unfinished(ctx context.Context) (string, error) {
	os.Setenv("HATCHET_CLIENT_HOST_PORT", "hatchet-engine-1:7070")
	client, err := newClient(5 * time.Second)
	if err != nil {
		return "", err
	}
	c, cancel := context.WithTimeout(ctx, 8*time.Second)
	defer cancel()
	st := []rest.V1TaskStatus{rest.V1TaskStatusQUEUED, rest.V1TaskStatusRUNNING}
	md := []string{"hq:1"}
	limit := int64(200)
	runs, err := client.Runs().List(c, rest.V1WorkflowRunListParams{
		Since: time.Unix(0, 0), Statuses: &st, AdditionalMetadata: &md, Limit: &limit,
	})
	if err != nil {
		return "", fmt.Errorf("runs: %w", err)
	}
	sched, err := client.Schedules().List(c, rest.WorkflowScheduledListParams{})
	if err != nil {
		return "", fmt.Errorf("schedules: %w", err)
	}
	waiting := 0
	if sched.Rows != nil {
		waiting = len(*sched.Rows)
	}
	if len(runs.Rows) == 0 && waiting == 0 {
		return "", nil
	}
	return fmt.Sprintf("%d runs queued or running, %d scheduled runs waiting", len(runs.Rows), waiting), nil
}

type rule struct {
	query string // rows that break the rule, one line each
	count string // the last line, with the number of rows
}

// What each rule judges, as a count (`hq cases <rule>`), and a change of the records that must
// break it (`hq mutate <rule>`), for `hydo rules check --mutate`.
var cases = map[string]string{
	"acked":       `SELECT count(DISTINCT m) FROM app_run WHERE m <> '' AND m NOT LIKE 'x%'`,
	"cron":        `SELECT count(DISTINCT sched_at) FROM app_run WHERE kind = 'tick'`,
	"scheduled":   `SELECT count(DISTINCT m) FROM app_run WHERE kind = 'sched'`,
	"early":       `SELECT count(*) FROM app_run WHERE kind IN ('tick', 'sched') AND sched_at <> ''`,
	"concurrency": `SELECT count(*) FROM app_run WHERE kind = 'limited'`,
	"dag":         `SELECT count(*) FROM app_run WHERE kind IN ('b', 'c')`,
	"overlap":     `SELECT count(*) FROM app_run`,
	"rerun":       `SELECT count(*) FROM app_run WHERE outcome = 'ok'`,
	"rate":        `SELECT count(*) FROM app_run WHERE kind = 'rated'`,
	"cancelled":   `SELECT count(*) FROM app_cancel WHERE status = 'CANCELLED'`,
}

var mutations = map[string]string{
	// a second run of the latest tick
	"cron": `INSERT INTO app_run (kind, m, run_id, task_run_id, sched_at, worker, attempt, boot, mono_start, mono_end, outcome)
	    SELECT kind, m, 'mutated-' || run_id, 'mutated-' || task_run_id, sched_at, worker, 0, boot, mono_start, mono_end, outcome
	      FROM app_run WHERE kind = 'tick' ORDER BY seq DESC LIMIT 1`,
	// a second run of one scheduled run
	"scheduled": `INSERT INTO app_run (kind, m, run_id, task_run_id, sched_at, worker, attempt, boot, mono_start, mono_end, outcome)
	    SELECT kind, m, 'mutated-' || run_id, 'mutated-' || task_run_id, sched_at, worker, 0, boot, mono_start, mono_end, outcome
	      FROM app_run WHERE kind = 'sched' ORDER BY seq DESC LIMIT 1`,
	// a run of a key at the same time as another run of that key
	"concurrency": `INSERT INTO app_run (kind, m, run_id, task_run_id, key, worker, attempt, boot, mono_start, mono_end, outcome)
	    SELECT kind, m || 'x', 'mutated-' || run_id, 'mutated-' || task_run_id, key, worker, 0, boot, mono_start, mono_end, outcome
	      FROM app_run WHERE kind = 'limited' AND mono_end IS NOT NULL ORDER BY seq DESC LIMIT 1`,
	// the first step of a DAG run that never finished
	"dag": `UPDATE app_run SET outcome = 'error' WHERE seq = (SELECT max(seq) FROM app_run WHERE kind = 'a')`,
	// an acknowledged trigger whose runs are gone
	"acked": `DELETE FROM app_run WHERE m = (SELECT m FROM app_run WHERE kind = 'plain' ORDER BY seq DESC LIMIT 1)`,
}

func casesOf(ctx context.Context, name string) int {
	q, ok := cases[name]
	if !ok {
		fmt.Println("unknown rule", name)
		return 2
	}
	c, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	p, err := pool(c, 1)
	if err != nil {
		fmt.Println("PostgreSQL:", err)
		return 2
	}
	defer p.Close()
	var n int64
	if err := p.QueryRow(c, q).Scan(&n); err != nil {
		fmt.Println("query:", err)
		return 2
	}
	fmt.Printf("%s judges %d\n", name, n)
	return 0
}

func mutate(ctx context.Context, name string) int {
	q, ok := mutations[name]
	if !ok {
		fmt.Println("no mutation for", name)
		return 2
	}
	c, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	p, err := pool(c, 1)
	if err != nil {
		fmt.Println("PostgreSQL:", err)
		return 2
	}
	defer p.Close()
	if _, err := p.Exec(c, q); err != nil {
		fmt.Println("mutate:", err)
		return 2
	}
	fmt.Println("mutated", name)
	return 0
}

var rules = map[string]rule{
	// one run per cron tick: "If the engine is down when a cron is due, that occurrence is
	// skipped ... It does not replay the ticks that passed while it was down." Each run of `tick`
	// carries the time it was scheduled for (hatchet__cron_scheduled_at). A retry of one run keeps
	// its run id, so only two run ids of one tick are two runs.
	"cron": {query: `
	    SELECT 'tick of ' || sched_at || ': ' || count(DISTINCT run_id) || ' runs: ' ||
	           string_agg(DISTINCT left(run_id, 8) || ' on ' || worker || ' started ' || to_char(started_at AT TIME ZONE 'UTC', 'HH24:MI:SS.MS'), ', ')
	      FROM app_run WHERE kind = 'tick' AND sched_at <> ''
	     GROUP BY sched_at HAVING count(DISTINCT run_id) > 1 ORDER BY sched_at`,
		count: "%d cron ticks with more than one run"},
	// a scheduled run is triggered once ("still runs once")
	"scheduled": {query: `
	    SELECT 'scheduled ' || m || ' for ' || sched_at || ': ' || count(DISTINCT run_id) || ' runs: ' || string_agg(DISTINCT left(run_id, 8), ',')
	      FROM app_run WHERE kind = 'sched' GROUP BY m, sched_at HAVING count(DISTINCT run_id) > 1`,
		count: "%d scheduled runs triggered more than once"},
	// a cron or scheduled run does not start before its time by the guest's clock
	"early": {query: `
	    SELECT kind || ' ' || coalesce(nullif(m, ''), left(run_id, 8)) || ' for ' || sched_at || ' started at ' ||
	           to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS') || ' on ' || worker
	      FROM app_run WHERE kind IN ('tick', 'sched') AND sched_at <> ''
	       AND started_at < sched_at::timestamptz - interval '1 second'
	     ORDER BY seq`,
		count: "%d cron or scheduled runs that started more than 1 s before their time"},
	// GROUP_ROUND_ROBIN with max runs 1: two runs of one key never overlap
	"concurrency": {query: `
	    SELECT 'key ' || a.key || ': ' || a.m || ' (' || left(a.run_id, 8) || ' #' || a.attempt || ' on ' || a.worker ||
	           ' [' || a.mono_start/1000000 || ',' || a.mono_end/1000000 || '] ms) and ' || b.m || ' (' || left(b.run_id, 8) ||
	           ' #' || b.attempt || ' on ' || b.worker || ' [' || b.mono_start/1000000 || ',' || b.mono_end/1000000 || '] ms) overlap'
	      FROM app_run a JOIN app_run b ON a.kind = 'limited' AND b.kind = 'limited' AND a.key = b.key
	       AND a.run_id < b.run_id AND a.boot = b.boot
	     WHERE a.mono_end IS NOT NULL AND b.mono_end IS NOT NULL
	       AND a.mono_start < b.mono_end AND b.mono_start < a.mono_end`,
		count: "%d pairs of runs of one concurrency key that overlapped"},
	// a DAG step starts only after its parent completed: b after an ok a, c after an ok b
	"dag": {query: `
	    SELECT 'order ' || x.m || ' (' || left(x.run_id, 8) || '): ' || x.kind || ' #' || x.attempt || ' on ' || x.worker ||
	           ' started before its parent ' || CASE x.kind WHEN 'b' THEN 'a' ELSE 'b' END || ' finished'
	      FROM app_run x WHERE x.kind IN ('b', 'c') AND NOT EXISTS (
	        SELECT 1 FROM app_run p WHERE p.run_id = x.run_id AND p.outcome = 'ok'
	           AND p.kind = CASE x.kind WHEN 'b' THEN 'a' ELSE 'b' END AND p.finished_seq < x.seq)`,
		count: "%d DAG steps that started before their parent completed"},
	// one task run is not executed twice at once
	"overlap": {query: `
	    SELECT a.kind || ' ' || coalesce(nullif(a.m, ''), '-') || ' task run ' || left(a.task_run_id, 8) || ': #' || a.attempt || ' on ' || a.worker ||
	           ' [' || a.mono_start/1000000 || ',' || coalesce((a.mono_end/1000000)::text, 'unfinished') || '] ms and #' || b.attempt || ' on ' || b.worker ||
	           ' [' || b.mono_start/1000000 || ',' || coalesce((b.mono_end/1000000)::text, 'unfinished') || '] ms'
	      FROM app_run a JOIN app_run b ON a.task_run_id = b.task_run_id AND a.seq < b.seq AND a.boot = b.boot
	     WHERE b.mono_start < coalesce(a.mono_end, 9223372036854775807) AND a.mono_end IS NOT NULL`,
		count: "%d pairs of executions of one task run that overlapped"},
	// a task run whose execution finished ok is not started again
	"rerun": {query: `
	    SELECT b.kind || ' ' || coalesce(nullif(b.m, ''), '-') || ' task run ' || left(b.task_run_id, 8) || ': #' || a.attempt || ' on ' || a.worker ||
	           ' finished ok, then #' || b.attempt || ' started on ' || b.worker
	      FROM app_run a JOIN app_run b ON a.task_run_id = b.task_run_id AND a.outcome = 'ok' AND b.seq > a.finished_seq`,
		count: "%d executions that started after the task run had finished ok"},
	// the static rate limit (2 per second): no 1-second interval holds more than two windows' worth of starts
	"rate": {query: `
	    SELECT 'from ' || a.mono_start/1000000 || ' ms on: ' || count(*) || ' starts of rated within 1 s'
	      FROM app_run a JOIN app_run b ON a.kind = 'rated' AND b.kind = 'rated' AND a.boot = b.boot
	       AND b.mono_start >= a.mono_start AND b.mono_start < a.mono_start + 1000000000
	     GROUP BY a.seq, a.mono_start HAVING count(DISTINCT (b.run_id, b.attempt)) > 4`,
		count: "%d 1-second intervals with more than 4 starts of the rate-limited task (limit 2 per second)"},
	// a run whose cancel the API confirmed does not complete afterwards
	"cancelled": {query: `
	    SELECT 'cancelled ' || c.m || ' (' || left(c.run_id, 8) || '): execution #' || r.attempt || ' on ' || r.worker ||
	           ' started ' || to_char(r.started_at AT TIME ZONE 'UTC', 'HH24:MI:SS.MS') || ' and finished ok ' ||
	           to_char(r.finished_at AT TIME ZONE 'UTC', 'HH24:MI:SS.MS') || ', after the API reported CANCELLED at ' ||
	           to_char(c.seen_at AT TIME ZONE 'UTC', 'HH24:MI:SS.MS')
	      FROM app_cancel c JOIN app_run r ON r.run_id = c.run_id
	     WHERE c.status = 'CANCELLED' AND r.outcome = 'ok' AND r.finished_at > c.seen_at`,
		count: "%d confirmed cancellations followed by a completed execution"},
}

func check(ctx context.Context, name string) int {
	if name == "acked" {
		return checkAcked(ctx)
	}
	r, ok := rules[name]
	if !ok {
		fmt.Println("unknown rule", name)
		return 2
	}
	c, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	pl, err := pool(c, 1)
	if err != nil {
		fmt.Println("PostgreSQL:", err)
		return 2
	}
	defer pl.Close()
	var bad []string
	for try := 0; ; try++ {
		bad = bad[:0]
		rows, err := pl.Query(c, r.query)
		if err == nil {
			for rows.Next() {
				var s string
				if err = rows.Scan(&s); err != nil {
					break
				}
				bad = append(bad, s)
			}
			rows.Close()
			if err == nil {
				err = rows.Err()
			}
		}
		if err == nil {
			break
		}
		if try >= 20 {
			fmt.Println("query:", err)
			return 2
		}
		time.Sleep(time.Second)
	}
	if name == "cancelled" {
		bad = append(bad, cancelledStatus(ctx, pl)...)
	}
	for _, s := range bad {
		fmt.Println(s)
	}
	fmt.Printf(r.count+"\n", len(bad))
	if len(bad) > 0 {
		return 1
	}
	return 0
}

// cancelledStatus asks the API for the final status of every run whose cancel it confirmed.
func cancelledStatus(ctx context.Context, p *pgxpool.Pool) []string {
	c, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	rows, err := p.Query(c, `SELECT m, run_id FROM app_cancel WHERE status = 'CANCELLED' ORDER BY m`)
	if err != nil {
		return []string{"cancelled runs: " + err.Error()}
	}
	type pair struct{ m, id string }
	var all []pair
	for rows.Next() {
		var x pair
		if rows.Scan(&x.m, &x.id) == nil {
			all = append(all, x)
		}
	}
	rows.Close()
	if len(all) == 0 {
		return nil
	}
	os.Setenv("HATCHET_CLIENT_HOST_PORT", "hatchet-engine-1:7070")
	client, err := newClient(5 * time.Second)
	if err != nil {
		return []string{"client: " + err.Error()}
	}
	var bad []string
	for _, x := range all {
		var st *rest.V1TaskStatus
		for try := 0; try < 10; try++ {
			if st, err = client.Runs().GetStatus(c, x.id); err == nil {
				break
			}
			time.Sleep(time.Second)
		}
		if err != nil {
			bad = append(bad, fmt.Sprintf("cancelled %s (%s): status unknown: %v", x.m, x.id[:8], err))
		} else if st != nil && *st != rest.V1TaskStatusCANCELLED {
			bad = append(bad, fmt.Sprintf("cancelled %s (%s): the API reported CANCELLED, and now %s", x.m, x.id[:8], *st))
		}
	}
	return bad
}

// checkAcked is the generic rule over the producer's records: every acknowledged trigger ran to
// completion (for the DAG: its last step), and no trigger that failed ran.
func checkAcked(ctx context.Context) int {
	last := map[string]string{}
	f, err := os.Open(os.Getenv("HYDO_OUTCOMES"))
	if err == nil {
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			if id, what, ok := strings.Cut(strings.TrimSpace(sc.Text()), " "); ok {
				last[id] = what
			}
		}
		f.Close()
	}
	c, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	p, err := pool(c, 1)
	if err != nil {
		fmt.Println("PostgreSQL:", err)
		return 2
	}
	defer p.Close()
	done := map[string]bool{}
	ran := map[string]bool{}
	rows, err := p.Query(c, `SELECT m, kind, coalesce(outcome, '') FROM app_run WHERE m <> ''`)
	if err != nil {
		fmt.Println("query:", err)
		return 2
	}
	for rows.Next() {
		var m, kind, out string
		if rows.Scan(&m, &kind, &out) == nil {
			ran[m] = true
			final := kind != "a" && kind != "b"
			if out == "ok" && final {
				done[m] = true
			}
		}
	}
	rows.Close()
	var bad []string
	judged := 0
	for m, what := range last {
		if strings.HasPrefix(m, "x") { // cancelled runs: the rule `cancelled`
			continue
		}
		switch what {
		case "ack":
			judged++
			if !done[m] {
				state := "never ran"
				if ran[m] {
					state = "ran but never completed"
				}
				bad = append(bad, fmt.Sprintf("%s was acknowledged and %s", m, state))
			}
		case "failed":
			judged++
			if ran[m] {
				bad = append(bad, fmt.Sprintf("%s failed and ran anyway", m))
			}
		}
	}
	for _, s := range bad {
		fmt.Println(s)
	}
	fmt.Printf("%d of %d acknowledged or failed triggers without their result\n", len(bad), judged)
	if len(bad) > 0 {
		return 1
	}
	return 0
}

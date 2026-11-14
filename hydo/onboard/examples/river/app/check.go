package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

// drain waits until every job but the periodic ones is finished, while the workers run, then
// writes the end of the drain for the messages check. A job that is still waiting at the
// deadline stays undelivered, and the check reports it as lost.
func drain(ctx context.Context) int {
	deadline := time.Now().Add(time.Duration(envInt("RIVERQ_DRAIN_S", 150)) * time.Second)
	quiet := 0
	var last string
	for time.Now().Before(deadline) {
		c, cancel := context.WithTimeout(ctx, 4*time.Second)
		p, err := pool(c, 1)
		if err == nil {
			var n int
			var states string
			err = p.QueryRow(c, `SELECT count(*), coalesce(string_agg(DISTINCT state::text, ','), '')
			    FROM river_job WHERE kind <> 'tick'
			    AND state IN ('available','running','retryable','scheduled','pending')`).Scan(&n, &states)
			p.Close()
			if err == nil {
				last = fmt.Sprintf("%d unfinished (%s)", n, states)
				if n == 0 {
					quiet++
				} else {
					quiet = 0
				}
			}
		}
		cancel()
		if quiet >= 3 {
			hist.Record(99999, "ok", "drained", "jobs", "-")
			fmt.Println("drained")
			return 0
		}
		time.Sleep(time.Second)
	}
	if last == "" {
		fmt.Println("drain: PostgreSQL did not answer")
		return 1
	}
	hist.Record(99999, "ok", "drained", "jobs", "-")
	fmt.Println("drain: deadline with", last)
	return 0
}

type rule struct {
	msg   string
	query string // rows that break the rule, one line each
	count string // the last line, with the count of rows
}

var rules = map[string]rule{
	// a job inserted inside a transaction that rolled back never exists, so never runs
	"rolled_back": {query: `
	    SELECT 'run ' || id || ' of job ' || job_id || ' by ' || client FROM app_run WHERE rb
	    UNION ALL
	    SELECT 'job ' || id || ' of the rolled-back ' || (args->>'op') || ' exists, state ' || state
	      FROM river_job WHERE kind = 'order' AND (args->>'rb')::boolean`,
		count: "%d runs or rows of jobs whose insert was rolled back"},
	// one row per unique key (ByArgs, ByPeriod 1 h), and one committed run
	"unique": {query: `
	    SELECT 'key ' || (args->>'key') || ': ' || count(*) || ' jobs ' || string_agg(id::text || '/' || state, ',')
	      FROM river_job WHERE kind = 'uniq' AND state NOT IN ('cancelled','discarded')
	      GROUP BY args->>'key', date_trunc('hour', scheduled_at AT TIME ZONE 'UTC') HAVING count(*) > 1
	    UNION ALL
	    SELECT 'key ' || e.key || ': ' || count(*) || ' committed runs, jobs ' || string_agg(e.job_id::text || '#' || e.attempt || '@' || e.client, ',')
	      FROM app_effect e JOIN river_job j ON j.id = e.job_id WHERE e.kind = 'uniq'
	      GROUP BY e.key, date_trunc('hour', j.scheduled_at AT TIME ZONE 'UTC') HAVING count(*) > 1`,
		count: "%d unique keys with more than one job or committed run"},
	// two runs of one job never overlap (both finished, one boot)
	"concurrent": {query: `
	    SELECT 'job ' || a.job_id || ' (' || a.kind || '): attempt ' || a.attempt || ' by ' || a.client ||
	           ' [' || a.mono_start/1000000 || ',' || a.mono_end/1000000 || '] ms and attempt ' || b.attempt || ' by ' || b.client ||
	           ' [' || b.mono_start/1000000 || ',' || b.mono_end/1000000 || '] ms; outcomes ' || a.outcome || '/' || b.outcome ||
	           '; rescued: ' || (SELECT count(*) FROM river_job j, unnest(j.errors) e WHERE j.id = a.job_id AND e->>'error' ILIKE '%stuck%')
	      FROM app_run a JOIN app_run b ON a.job_id = b.job_id AND a.id < b.id AND a.boot = b.boot
	     WHERE a.mono_end IS NOT NULL AND b.mono_end IS NOT NULL
	       AND a.mono_start < b.mono_end AND b.mono_start < a.mono_end`,
		count: "%d pairs of runs of one job that overlapped"},
	// a job that completed in its transaction is not run again ("a successful commit guarantees
	// that the job will never rerun")
	"rerun": {query: `
	    SELECT 'job ' || job_id || ': ' || count(*) || ' committed runs: ' || string_agg(attempt::text || '@' || client, ',')
	      FROM app_effect GROUP BY job_id HAVING count(*) > 1
	    UNION ALL
	    SELECT 'job ' || r.job_id || ': attempt ' || r.attempt || ' by ' || r.client || ' started at ' || r.mono_start/1000000 ||
	           ' ms, after attempt ' || c.attempt || ' by ' || c.client || ' committed by ' || c.mono_end/1000000 || ' ms'
	      FROM app_effect e JOIN app_run c ON c.id = e.run_id JOIN app_run r ON r.job_id = e.job_id AND r.id <> c.id AND r.boot = c.boot
	     WHERE c.outcome = 'committed' AND c.mono_end IS NOT NULL AND r.mono_start > c.mono_end`,
		count: "%d completed jobs that ran again"},
	// the periodic job is inserted once per run of its schedule (River sets scheduled_at to the
	// run's time): one leader at a time. The line names each inserting client and its clock. A
	// job that failed once has a new scheduled_at, its retry's time, so only jobs without an
	// error are compared.
	"periodic": {query: `
	    SELECT 'run at ' || to_char(scheduled_at AT TIME ZONE 'UTC', 'HH24:MI:SS') || ': ' || count(*) || ' jobs, ' ||
	           count(DISTINCT args->>'leader') || ' leaders: ' || string_agg((args->>'leader') || '@' || (args->>'at'), ',' ORDER BY id)
	      FROM river_job WHERE kind = 'tick' AND coalesce(array_length(errors, 1), 0) = 0
	      GROUP BY scheduled_at HAVING count(*) > 1`,
		count: "%d runs of the periodic job inserted more than once"},
	// a job whose cancellation answered "cancelled" never completes afterwards
	"cancelled": {query: `
	    SELECT 'job ' || c.job_id || ' (' || c.op || '): cancel answered cancelled, now ' || j.state ||
	           ', committed runs ' || (SELECT count(*) FROM app_effect e WHERE e.job_id = c.job_id)
	      FROM app_cancel c JOIN river_job j ON j.id = c.job_id
	     WHERE c.state = 'cancelled'
	       AND (j.state <> 'cancelled' OR EXISTS (SELECT 1 FROM app_effect e WHERE e.job_id = c.job_id))`,
		count: "%d cancelled jobs that completed afterwards"},
}

func check(ctx context.Context, name string) int {
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	p, err := pool(ctx, 1)
	if err != nil {
		fmt.Println("PostgreSQL does not answer:", err)
		return 2
	}
	defer p.Close()
	if err := p.Ping(ctx); err != nil {
		fmt.Println("PostgreSQL does not answer:", err)
		return 2
	}
	if name == "acked" {
		return checkAcked(ctx, p)
	}
	r, ok := rules[name]
	if !ok {
		fmt.Println("unknown rule", name)
		return 2
	}
	rows, err := p.Query(ctx, r.query)
	if err != nil {
		fmt.Println("query:", err)
		return 2
	}
	n := 0
	for rows.Next() {
		var line string
		if err := rows.Scan(&line); err != nil {
			fmt.Println("scan:", err)
			return 2
		}
		n++
		if n <= 15 {
			fmt.Println(line)
		}
	}
	if rows.Err() != nil {
		fmt.Println("query:", rows.Err())
		return 2
	}
	fmt.Printf(r.count+"\n", n)
	if n > 0 {
		return 1
	}
	return 0
}

// stateExpr is a job's state, marked "(waiting)" when the job waits for a later time.
// A running job waits too when its attempt started after now: after a clock step back, the
// rescuer finds a job stuck only when RescueStuckJobsAfter has passed on the clock.
const stateExpr = `state::text || CASE WHEN state IN ('scheduled','retryable','available') AND scheduled_at > now() THEN '(waiting)'
    WHEN state = 'running' AND attempted_at > now() THEN '(waiting)' ELSE '' END`

// checkAcked is the generic rule: every operation the producer saw acknowledged has its effect,
// and every operation it saw fail has none.
func checkAcked(ctx context.Context, p *pgxpool.Pool) int {
	f, err := os.Open(os.Getenv("HYDO_OUTCOMES"))
	if err != nil {
		fmt.Println("0 operations recorded (no outcomes file)")
		return 0
	}
	defer f.Close()
	last := map[string]string{}
	var order []string
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fs := strings.Fields(sc.Text())
		if len(fs) != 2 {
			continue
		}
		if _, seen := last[fs[0]]; !seen {
			order = append(order, fs[0])
		}
		last[fs[0]] = fs[1]
	}
	bad, judged, waiting := 0, 0, 0
	for _, id := range order {
		o := last[id]
		var q string
		var arg string = id
		switch {
		case o == "ack" && (id[0] == 'p' || id[0] == 's'):
			q = `SELECT coalesce(string_agg(` + stateExpr + `, ','), 'missing') FROM river_job WHERE args->>'op' = $1`
		case o == "ack" && id[0] == 'o':
			q = `SELECT coalesce((SELECT ` + stateExpr + ` FROM river_job WHERE args->>'op' = $1), 'missing') || '/' ||
			            coalesce((SELECT processed::text FROM app_order WHERE op = $1), 'no order')`
		case o == "failed" && id[0] == 'o':
			q = `SELECT CASE WHEN EXISTS (SELECT 1 FROM app_order WHERE op = $1) OR EXISTS (SELECT 1 FROM river_job WHERE args->>'op' = $1)
			            THEN 'exists' ELSE 'none' END`
		case o == "ack" && id[0] == 'u':
			arg = id[strings.IndexByte(id, ':')+1:]
			q = `SELECT coalesce(string_agg(` + stateExpr + `, ','), 'missing') FROM river_job WHERE kind = 'uniq' AND args->>'key' = $1`
		case o == "ack" && id[0] == 'c':
			q = `SELECT coalesce(string_agg(` + stateExpr + `, ','), 'missing') FROM river_job WHERE args->>'op' = $1`
		default:
			continue
		}
		var got string
		if err := p.QueryRow(ctx, q, arg).Scan(&got); err != nil {
			fmt.Println("query:", err)
			return 2
		}
		judged++
		want := map[byte]string{'p': "completed", 's': "completed", 'o': "completed/1", 'u': "completed", 'c': ""}[id[0]]
		ok := got == want
		switch {
		case o == "failed":
			ok = got == "none"
		case id[0] == 'c':
			ok = got == "cancelled" || got == "completed"
		case id[0] == 'u':
			ok = strings.Contains(got, "completed")
		}
		if !ok && o == "ack" && strings.Contains(got, "(waiting)") {
			// a job scheduled (or retried) for a later time of the clock, for example after a
			// clock step back: not lost, still waiting
			waiting++
			fmt.Printf("%s (%s): %s\n", id, o, got)
			ok = true
		}
		if !ok {
			bad++
			if bad <= 15 {
				fmt.Printf("%s (%s): %s\n", id, o, got)
			}
		}
	}
	if waiting > 0 {
		fmt.Printf("%d acknowledged jobs wait for a later time of the clock\n", waiting)
	}
	fmt.Printf("%d of %d acknowledged or failed operations without their result\n", bad, judged)
	if bad > 0 {
		return 1
	}
	return 0
}

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/riverqueue/river"
	"github.com/riverqueue/river/riverdriver/riverpgxv5"
	"github.com/riverqueue/river/rivertype"
)

// The job kinds. M is the message id of hydo's messages check (empty: the job is not in the
// history); Op is the producer operation that inserted the job.
type PlainArgs struct {
	M  string `json:"m"`
	Op string `json:"op"`
	Ms int    `json:"ms"`
}

func (PlainArgs) Kind() string { return "plain" }

type SchedArgs struct {
	M  string `json:"m"`
	Op string `json:"op"`
	Ms int    `json:"ms"`
}

func (SchedArgs) Kind() string { return "sched" }

// OrderArgs is inserted with InsertTx inside the transaction that creates the order. RB says
// that the producer rolls that transaction back: such a job must never exist, so never run.
type OrderArgs struct {
	M     string `json:"m"`
	Op    string `json:"op"`
	Order int64  `json:"order"`
	RB    bool   `json:"rb"`
}

func (OrderArgs) Kind() string { return "order" }

// UniqArgs is unique by its key (UniqueOpts ByArgs, with only Key marked) and by period.
type UniqArgs struct {
	Key string `json:"key" river:"unique"`
	Op  string `json:"op"`
}

func (UniqArgs) Kind() string { return "uniq" }

// SlowArgs is a job that the producer cancels while it waits or runs.
type SlowArgs struct {
	Op string `json:"op"`
	Ms int    `json:"ms"`
}

func (SlowArgs) Kind() string { return "slow" }

// TickArgs is the periodic job. The leader that inserted it writes its client id and the 2 s
// period it inserted it for, so two leaders acting at once show as two jobs of one period.
type TickArgs struct {
	Leader string `json:"leader"`
	Period int64  `json:"period"`
	At     int64  `json:"at"`
}

func (TickArgs) Kind() string { return "tick" }

type every2s struct{}

func (every2s) Next(t time.Time) time.Time { return t.Truncate(2 * time.Second).Add(2 * time.Second) }

type wk struct {
	name   string
	id     string
	index  int
	boot   string
	pool   *pgxpool.Pool
	slots  chan int
	client *river.Client[pgx.Tx]
}

func (w *wk) acquire() int  { return <-w.slots }
func (w *wk) release(p int) { w.slots <- p }

func item(m string, row *rivertype.JobRow) string {
	return fmt.Sprintf("%s;o=%d;n=%d;h=%d.%d", m, row.ID, row.Attempt, row.ID, row.Attempt)
}

// effect is the part of a job that commits together with the job's completion.
type effect func(ctx context.Context, tx pgx.Tx, runID int64) error

// run is one run of a job: the record, the work, and for a transactional kind its effect and
// JobCompleteTx in one transaction.
func (w *wk) run(ctx context.Context, row *rivertype.JobRow, m string, rb bool, ms int,
	eff effect, complete func(ctx context.Context, tx pgx.Tx) error) error {
	proc := w.acquire()
	defer w.release(proc)
	var runID int64
	err := w.pool.QueryRow(ctx, `INSERT INTO app_run
	    (job_id, kind, attempt, client, proc, boot, mono_start, wall_start, scheduled_at, rb)
	    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id`,
		row.ID, row.Kind, row.Attempt, w.id, proc, w.boot, monoNS(), time.Now(), row.ScheduledAt, rb).Scan(&runID)
	if err != nil {
		return fmt.Errorf("record the run: %w", err)
	}
	if m != "" {
		hist.Invoke(proc, "recv", "jobs", "-").Ok("jobs", "-", item(m, row))
	}
	outcome := "ok"
	defer func() { w.finish(runID, outcome) }()
	if ms > 0 {
		t := time.NewTimer(time.Duration(ms) * time.Millisecond)
		select {
		case <-t.C:
		case <-ctx.Done():
			t.Stop()
			outcome = "ctx"
			return ctx.Err()
		}
	}
	if eff == nil {
		return nil
	}
	tx, err := w.pool.Begin(ctx)
	if err != nil {
		outcome = "begin"
		return err
	}
	defer tx.Rollback(context.Background())
	if err := eff(ctx, tx, runID); err != nil {
		outcome = "effect"
		return err
	}
	if err := complete(ctx, tx); err != nil {
		outcome = "complete:" + err.Error()
		return err
	}
	var ack *hydohistOp
	if m != "" {
		ack = invokeAck(proc, item(m, row))
	}
	if err := tx.Commit(ctx); err != nil {
		outcome = "commit"
		if ack != nil {
			ack.Info()
		}
		return err
	}
	if ack != nil {
		ack.Ok()
	}
	outcome = "committed"
	return nil
}

func (w *wk) finish(runID int64, outcome string) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_, _ = w.pool.Exec(ctx, `UPDATE app_run SET mono_end = $1, outcome = $2 WHERE id = $3`, monoNS(), outcome, runID)
}

func insertEffect(ctx context.Context, tx pgx.Tx, row *rivertype.JobRow, client string, runID int64, key string) error {
	_, err := tx.Exec(ctx, `INSERT INTO app_effect (job_id, kind, attempt, client, run_id, key) VALUES ($1,$2,$3,$4,$5,NULLIF($6,''))`,
		row.ID, row.Kind, row.Attempt, client, runID, key)
	return err
}

type plainWorker struct {
	river.WorkerDefaults[PlainArgs]
	w *wk
}

func (x *plainWorker) Work(ctx context.Context, j *river.Job[PlainArgs]) error {
	return x.w.run(ctx, j.JobRow, j.Args.M, false, j.Args.Ms, nil, nil)
}

type schedWorker struct {
	river.WorkerDefaults[SchedArgs]
	w *wk
}

func (x *schedWorker) Work(ctx context.Context, j *river.Job[SchedArgs]) error {
	return x.w.run(ctx, j.JobRow, j.Args.M, false, j.Args.Ms, nil, nil)
}

type orderWorker struct {
	river.WorkerDefaults[OrderArgs]
	w *wk
}

func (x *orderWorker) Work(ctx context.Context, j *river.Job[OrderArgs]) error {
	return x.w.run(ctx, j.JobRow, j.Args.M, j.Args.RB, 20,
		func(ctx context.Context, tx pgx.Tx, runID int64) error {
			if _, err := tx.Exec(ctx, `UPDATE app_order SET processed = processed + 1 WHERE id = $1`, j.Args.Order); err != nil {
				return err
			}
			return insertEffect(ctx, tx, j.JobRow, x.w.id, runID, "")
		},
		func(ctx context.Context, tx pgx.Tx) error {
			_, err := river.JobCompleteTx[*riverpgxv5.Driver](ctx, tx, j)
			return err
		})
}

type uniqWorker struct {
	river.WorkerDefaults[UniqArgs]
	w *wk
}

func (x *uniqWorker) Work(ctx context.Context, j *river.Job[UniqArgs]) error {
	return x.w.run(ctx, j.JobRow, uniqMessage(j.Args.Key, j.ScheduledAt), false, 50,
		func(ctx context.Context, tx pgx.Tx, runID int64) error {
			return insertEffect(ctx, tx, j.JobRow, x.w.id, runID, j.Args.Key)
		},
		func(ctx context.Context, tx pgx.Tx) error {
			_, err := river.JobCompleteTx[*riverpgxv5.Driver](ctx, tx, j)
			return err
		})
}

type slowWorker struct {
	river.WorkerDefaults[SlowArgs]
	w *wk
}

func (x *slowWorker) Work(ctx context.Context, j *river.Job[SlowArgs]) error {
	return x.w.run(ctx, j.JobRow, "", false, j.Args.Ms,
		func(ctx context.Context, tx pgx.Tx, runID int64) error {
			return insertEffect(ctx, tx, j.JobRow, x.w.id, runID, "")
		},
		func(ctx context.Context, tx pgx.Tx) error {
			_, err := river.JobCompleteTx[*riverpgxv5.Driver](ctx, tx, j)
			return err
		})
}

type tickWorker struct {
	river.WorkerDefaults[TickArgs]
	w *wk
}

func (x *tickWorker) Work(ctx context.Context, j *river.Job[TickArgs]) error {
	return x.w.run(ctx, j.JobRow, "", false, 0, nil, nil)
}

func worker(ctx context.Context, name string) error {
	idx, _ := strconv.Atoi(strings.TrimLeft(name, "abcdefghijklmnopqrstuvwxyz-_"))
	w := &wk{name: name, index: idx, boot: bootID(), slots: make(chan int, 16)}
	w.id = fmt.Sprintf("%s-%d", name, os.Getpid())
	for i := 0; i < 16; i++ {
		w.slots <- idx*1000 + i
	}
	var err error
	if w.pool, err = pool(ctx, 10); err != nil {
		return err
	}
	workers := river.NewWorkers()
	river.AddWorker(workers, &plainWorker{w: w})
	river.AddWorker(workers, &schedWorker{w: w})
	river.AddWorker(workers, &orderWorker{w: w})
	river.AddWorker(workers, &uniqWorker{w: w})
	river.AddWorker(workers, &slowWorker{w: w})
	river.AddWorker(workers, &tickWorker{w: w})
	tick := river.NewPeriodicJob(every2s{}, func() (river.JobArgs, *river.InsertOpts) {
		now := time.Now()
		return TickArgs{Leader: w.id, Period: now.Truncate(2 * time.Second).UnixMilli(), At: now.UnixMilli()}, nil
	}, &river.PeriodicJobOpts{ID: "tick"})
	w.client, err = river.NewClient(riverpgxv5.New(w.pool), &river.Config{
		ID:                   w.id,
		Logger:               logger(),
		JobTimeout:           time.Duration(envInt("RIVERQ_JOB_TIMEOUT_MS", 4000)) * time.Millisecond,
		RescueStuckJobsAfter: time.Duration(envInt("RIVERQ_RESCUE_AFTER_MS", 10000)) * time.Millisecond,
		Queues:               map[string]river.QueueConfig{river.QueueDefault: {MaxWorkers: 4}},
		Workers:              workers,
		PeriodicJobs:         []*river.PeriodicJob{tick},
		RetryPolicy:          shortRetry{},
		// keep every finished job (-1): the rules read them, also after a forward clock step
		// that is longer than River's 24 h retention
		CancelledJobRetentionPeriod: -1,
		CompletedJobRetentionPeriod: -1,
		DiscardedJobRetentionPeriod: -1,
	})
	if err != nil {
		return err
	}
	events, cancelEvents := w.client.Subscribe(river.EventKindJobCompleted)
	defer cancelEvents()
	go w.ackEvents(events)
	sctx, stop := signal.NotifyContext(ctx, syscall.SIGTERM, syscall.SIGINT)
	defer stop()
	if err := w.client.Start(sctx); err != nil {
		return err
	}
	_ = os.WriteFile("/tmp/riverq-"+name+".started", []byte(w.id+"\n"), 0o644)
	<-sctx.Done()
	sh, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	return w.client.Stop(sh)
}

// ackEvents records the acknowledgement of a job that River's completer marked completed (the
// kinds that do not complete in their own transaction).
func (w *wk) ackEvents(ch <-chan *river.Event) {
	proc := w.index*1000 + 900
	for ev := range ch {
		if ev == nil || ev.Job == nil || (ev.Job.Kind != "plain" && ev.Job.Kind != "sched") {
			continue
		}
		var a struct {
			M string `json:"m"`
		}
		if json.Unmarshal(ev.Job.EncodedArgs, &a) != nil || a.M == "" {
			continue
		}
		invokeAck(proc, item(a.M, ev.Job)).Ok()
	}
}

// shortRetry retries a failed job after 1 s per attempt, at most 5 s, so that a run of a minute
// sees its retries (River's default waits attempt^4 seconds).
type shortRetry struct{}

func (shortRetry) NextRetry(job *rivertype.JobRow) time.Time {
	d := time.Duration(job.Attempt) * time.Second
	if d > 5*time.Second {
		d = 5 * time.Second
	}
	return time.Now().Add(d)
}

// uniqMessage is the message id of a unique job in hydo's history: its key and the hour that
// River's ByPeriod counts it in (the job's scheduled time, the insert time without ScheduledAt,
// in UTC). A send in another hour, for example after a clock step, is another message.
func uniqMessage(key string, at time.Time) string {
	return fmt.Sprintf("u-%s-%s", key, at.UTC().Truncate(time.Hour).Format("0102T15"))
}

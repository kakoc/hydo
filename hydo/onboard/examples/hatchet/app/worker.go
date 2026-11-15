package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	hatchet "github.com/hatchet-dev/hatchet/sdks/go"
	"github.com/hatchet-dev/hatchet/sdks/go/features"
	"golang.org/x/sys/unix"
)

// The tasks' inputs. M is the producer's message id; Ms how long the task works.
type TaskIn struct {
	M   string `json:"m"`
	Ms  int    `json:"ms"`
	Key string `json:"key,omitempty"` // limited: the concurrency key
	At  int64  `json:"at,omitempty"`  // sched: the time it was scheduled for, Unix ms
	// Stubborn: the task does not look at its cancellation and works until its end, as a task
	// that never checks ctx.Done() does (native/: the cancelled run that completes)
	Stubborn bool `json:"stubborn,omitempty"`
}

type TaskOut struct {
	By string `json:"by"`
}

type TickIn struct{}

// The cron of `tick`: every 2 s (Hatchet takes 6-field expressions with seconds).
const tickCron = "*/2 * * * * *"

// The static rate limit of `rated`: 2 runs per second.
const rateKey, rateLimit = "rl", 2

func mono() int64 {
	var ts unix.Timespec
	_ = unix.ClockGettime(unix.CLOCK_MONOTONIC, &ts)
	return ts.Nano()
}

// token reads the API token that the setup job wrote (hatchet-admin token create).
func token(wait time.Duration) (string, error) {
	path := os.Getenv("HQ_TOKEN_FILE")
	if path == "" {
		// in a container (Compose) the config volume is at /hatchet/config; in hydo's guest the
		// volume is the data disk's directory vol/hatchet_config
		path = "/hatchet/config/token"
		if _, err := os.Stat("/data/vol/hatchet_config"); err == nil {
			path = "/data/vol/hatchet_config/token"
		}
	}
	end := time.Now().Add(wait)
	for {
		b, err := os.ReadFile(path)
		if err == nil && len(strings.TrimSpace(string(b))) > 0 {
			return strings.TrimSpace(string(b)), nil
		}
		if time.Now().After(end) {
			return "", fmt.Errorf("no token in %s: %v", path, err)
		}
		time.Sleep(500 * time.Millisecond)
	}
}

// newClient connects to the engine of HATCHET_CLIENT_HOST_PORT (gRPC) and the API of
// HATCHET_CLIENT_SERVER_URL (REST), without TLS.
func newClient(wait time.Duration) (*hatchet.Client, error) {
	tok, err := token(wait)
	if err != nil {
		return nil, err
	}
	if os.Getenv("HATCHET_CLIENT_TLS_STRATEGY") == "" {
		os.Setenv("HATCHET_CLIENT_TLS_STRATEGY", "none")
	}
	if os.Getenv("HATCHET_CLIENT_SERVER_URL") == "" {
		os.Setenv("HATCHET_CLIENT_SERVER_URL", "http://hatchet-api:8080")
	}
	if os.Getenv("HATCHET_CLIENT_HOST_PORT") == "" {
		os.Setenv("HATCHET_CLIENT_HOST_PORT", "hatchet-engine-1:7070")
	}
	if os.Getenv("HATCHET_CLIENT_LOG_LEVEL") == "" {
		os.Setenv("HATCHET_CLIENT_LOG_LEVEL", "warn")
	}
	return hatchet.NewClient(hatchet.WithToken(tok))
}

type wk struct {
	name string
	idx  int
	boot string
	p    *pgxpool.Pool
	n    atomic.Int64
}

// start writes the run's row before its work. A failed write fails the run, and Hatchet retries
// it. It also writes the receive record of a producer's message (not for cron runs, and only for
// the first task of a DAG).
func (w *wk) start(ctx hatchet.Context, kind, m, key, schedAt string) (int64, error) {
	c, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	var seq int64
	err := w.p.QueryRow(c, `INSERT INTO app_run (kind, m, run_id, task_run_id, key, sched_at, worker, attempt, boot, mono_start)
	    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING seq`,
		kind, m, ctx.WorkflowRunId(), ctx.StepRunId(), key, schedAt, w.name, ctx.RetryCount(), w.boot, mono()).Scan(&seq)
	if err != nil {
		return 0, fmt.Errorf("app_run: %w", err)
	}
	if m != "" && kind != "b" && kind != "c" && kind != "slow" { // a cancelled run is no send
		proc := w.idx*100000 + int(w.n.Add(1))
		item := fmt.Sprintf("%s;o=%s;n=%d", m, ctx.WorkflowRunId(), ctx.RetryCount()+1)
		hist.Invoke(proc, "recv", "tasks", "-").Ok("tasks", "-", item)
	}
	return seq, nil
}

// finish completes the run's row. A failed write fails the run.
func (w *wk) finish(seq int64, outcome string) error {
	c, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, err := w.p.Exec(c, `UPDATE app_run SET mono_end = $2, finished_at = clock_timestamp(),
	    finished_seq = nextval('app_run_seq_seq'), outcome = $3 WHERE seq = $1`, seq, mono(), outcome)
	if err != nil {
		return fmt.Errorf("app_run finish: %w", err)
	}
	return nil
}

// work sleeps ms milliseconds, or until the run is cancelled.
func work(ctx hatchet.Context, ms int) error {
	t := time.NewTimer(time.Duration(ms) * time.Millisecond)
	defer t.Stop()
	select {
	case <-t.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (w *wk) run(kind string) func(ctx hatchet.Context, in TaskIn) (TaskOut, error) {
	return func(ctx hatchet.Context, in TaskIn) (TaskOut, error) {
		sched := ""
		if in.At != 0 {
			sched = time.UnixMilli(in.At).UTC().Format(time.RFC3339Nano)
		}
		seq, err := w.start(ctx, kind, in.M, in.Key, sched)
		if err != nil {
			return TaskOut{}, err
		}
		var werr error
		if in.Stubborn {
			time.Sleep(time.Duration(in.Ms) * time.Millisecond)
		} else {
			werr = work(ctx, in.Ms)
		}
		if err := werr; err != nil {
			_ = w.finish(seq, "cancelled")
			return TaskOut{}, err
		}
		if err := w.finish(seq, "ok"); err != nil {
			return TaskOut{}, err
		}
		return TaskOut{By: w.name}, nil
	}
}

func worker(ctx context.Context, name string) error {
	idx, _ := strconv.Atoi(strings.TrimPrefix(name, "w"))
	p, err := pool(ctx, 8)
	if err != nil {
		return err
	}
	w := &wk{name: name, idx: idx, boot: bootID(), p: p}
	// the engine of a worker without HATCHET_CLIENT_HOST_PORT: w1 and w3 engine 1, w2 engine 2
	if os.Getenv("HATCHET_CLIENT_HOST_PORT") == "" {
		os.Setenv("HATCHET_CLIENT_HOST_PORT", fmt.Sprintf("hatchet-engine-%d:7070", 2-idx%2))
	}
	client, err := newClient(5 * time.Minute)
	if err != nil {
		return err
	}
	// the static rate limit of `rated`; every worker puts the same one
	if err := client.RateLimits().Upsert(features.CreateRatelimitOpts{Key: rateKey, Limit: rateLimit, Duration: hatchet.Second}); err != nil {
		fmt.Fprintln(os.Stderr, "rate limit:", err)
	}
	retries := hatchet.WithRetries(3)
	timeout := hatchet.WithExecutionTimeout(20 * time.Second)

	plain := client.NewStandaloneTask("plain", w.run("plain"), retries, timeout)
	sched := client.NewStandaloneTask("sched", w.run("sched"), retries, timeout)
	slow := client.NewStandaloneTask("slow", w.run("slow"), retries, timeout)
	one := int32(1)
	rr := hatchet.GroupRoundRobin
	limited := client.NewStandaloneTask("limited", w.run("limited"), retries, timeout,
		hatchet.WithWorkflowConcurrency(hatchet.Concurrency{Expression: "input.key", MaxRuns: &one, LimitStrategy: &rr}))
	units := 1
	rated := client.NewStandaloneTask("rated", w.run("rated"), retries, timeout,
		hatchet.WithRateLimits(&hatchet.RateLimit{Key: rateKey, Units: &units}))

	// the DAG `order`: a, then b, then c
	order := client.NewWorkflow("order")
	a := order.NewTask("a", w.run("a"), retries, timeout)
	b := order.NewTask("b", w.run("b"), retries, timeout, hatchet.WithParents(a))
	_ = order.NewTask("c", w.run("c"), retries, timeout, hatchet.WithParents(b))

	// the cron task: each run records the time it was scheduled for
	tick := client.NewStandaloneTask("tick", func(ctx hatchet.Context, _ TickIn) (TaskOut, error) {
		at := ctx.AdditionalMetadata()["hatchet__cron_scheduled_at"]
		seq, err := w.start(ctx, "tick", "", "", at)
		if err != nil {
			return TaskOut{}, err
		}
		if err := w.finish(seq, "ok"); err != nil {
			return TaskOut{}, err
		}
		return TaskOut{By: w.name}, nil
	}, retries, timeout, hatchet.WithWorkflowCron(tickCron))

	wr, err := client.NewWorker("hq-"+name,
		hatchet.WithWorkflows(plain, sched, slow, limited, rated, order, tick),
		hatchet.WithSlots(6))
	if err != nil {
		return err
	}
	cleanup, err := wr.Start()
	if err != nil {
		return err
	}
	_ = os.WriteFile("/tmp/hq-"+name+".started", nil, 0o644)
	fmt.Fprintf(os.Stderr, "hq worker %s: started\n", name)
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGTERM, syscall.SIGINT)
	<-sig
	return cleanup()
}

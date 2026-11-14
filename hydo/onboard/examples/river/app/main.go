// riverq is a small application on River (github.com/riverqueue/river), a job queue for Go on
// PostgreSQL. It is the system under test of this example, and its own test client:
//
//	riverq migrate                 River's migrations and the application's tables
//	riverq worker <name>           a worker process: River's client with a queue, the workers,
//	                               leader election, and a periodic job every 2 s
//	riverq op <kind>               one producer operation (a workload command), kind: plain,
//	                               sched, order, uniq, cancel
//	riverq drain                   wait until every job is finished (the messages check's drain)
//	riverq check <rule>            one rule over the tables (exit 0 holds, 1 violated, 2 unknown)
//	riverq ready [<name>]          readiness: PostgreSQL answers (and the worker started)
//	riverq leader                  the hydo service of River's elected leader
//
// The worker records every run of a job in the table app_run, and the effect of a job that
// completes in a transaction (River's JobCompleteTx) in app_effect, in that transaction. The
// producer and the worker also write hydo history records (HYDO-HIST jobs ...) on the guest
// console for hydo's messages check: a send for each insert, a receive for each run, and an
// acknowledgement when River completed the job.
package main

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/kakoc/hydo/sdk/go/hydohist"
)

var hist = hydohist.New("jobs")

func dsn() string {
	if v := os.Getenv("RIVERQ_DATABASE_URL"); v != "" {
		return v
	}
	return "postgres://postgres:river@postgres:5432/river?sslmode=disable&connect_timeout=5"
}

func pool(ctx context.Context, max int32) (*pgxpool.Pool, error) {
	cfg, err := pgxpool.ParseConfig(dsn())
	if err != nil {
		return nil, err
	}
	cfg.MaxConns = max
	cfg.HealthCheckPeriod = 2 * time.Second
	return pgxpool.NewWithConfig(ctx, cfg)
}

// bootID names the guest's boot, so that runs of two boots (a power cut) are never compared by
// their monotonic times.
func bootID() string {
	b, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return "-"
	}
	return strings.TrimSpace(string(b))
}

func die(code int, format string, a ...any) {
	fmt.Fprintf(os.Stderr, "riverq: "+format+"\n", a...)
	os.Exit(code)
}

func logger() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
}

func main() {
	if len(os.Args) < 2 {
		die(2, "usage: riverq migrate|worker|op|drain|check|ready ...")
	}
	ctx := context.Background()
	switch os.Args[1] {
	case "migrate":
		if err := migrate(ctx); err != nil {
			die(1, "migrate: %v", err)
		}
	case "worker":
		if len(os.Args) < 3 {
			die(2, "usage: riverq worker <name>")
		}
		if err := worker(ctx, os.Args[2]); err != nil {
			die(1, "worker: %v", err)
		}
	case "op":
		if len(os.Args) < 3 {
			die(2, "usage: riverq op <kind>")
		}
		os.Exit(op(ctx, os.Args[2]))
	case "drain":
		os.Exit(drain(ctx))
	case "check":
		if len(os.Args) < 3 {
			die(2, "usage: riverq check <rule>")
		}
		os.Exit(check(ctx, os.Args[2]))
	case "ready":
		os.Exit(ready(ctx, os.Args[2:]))
	case "leader":
		// the hydo service of River's elected leader (river_leader), for the group's primary:
		c, cancel := context.WithTimeout(ctx, 4*time.Second)
		defer cancel()
		p, err := pool(c, 1)
		if err != nil {
			os.Exit(1)
		}
		var id string
		if err := p.QueryRow(c, `SELECT leader_id FROM river_leader`).Scan(&id); err != nil {
			os.Exit(1)
		}
		name, _, _ := strings.Cut(id, "-")
		fmt.Println("worker_" + strings.TrimPrefix(name, "w"))
	default:
		die(2, "unknown command %q", os.Args[1])
	}
}

func ready(ctx context.Context, args []string) int {
	ctx, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	if len(args) > 0 {
		if _, err := os.Stat("/tmp/riverq-" + args[0] + ".started"); err != nil {
			return 1
		}
	}
	p, err := pool(ctx, 1)
	if err != nil {
		return 1
	}
	defer p.Close()
	var n int
	if err := p.QueryRow(ctx, "SELECT count(*) FROM app_run").Scan(&n); err != nil {
		return 1
	}
	return 0
}

func envInt(name string, def int64) int64 {
	if v, err := strconv.ParseInt(os.Getenv(name), 10, 64); err == nil {
		return v
	}
	return def
}

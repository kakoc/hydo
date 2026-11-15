// hq is a small application on Hatchet (github.com/hatchet-dev/hatchet), a task queue and
// workflow engine on PostgreSQL. It is the system's workers in this example, and its own test
// client:
//
//	hq migrate                 the application's own database `app` (its run records)
//	hq worker <name>           a worker process: Hatchet's Go SDK with the example's tasks, the
//	                           DAG `order`, and the cron task `tick` every 2 s
//	hq op <kind>               one producer operation (a workload command), kind: plain, sched,
//	                           limited, rated, dag, cancel
//	hq drain                   wait until no run of the producer's tasks is queued or running
//	hq check <rule>            one rule over the run records (exit 0 holds, 1 violated, 2 unknown)
//	hq cases <rule>            what a rule judges, as a count (hydo rules check)
//	hq mutate <rule>           a change of the run records that must break the rule
//	hq ready [<name>]          readiness: PostgreSQL answers (and the worker started)
//
// Every run of every task writes a row of app_run before its work and finishes it afterwards,
// in the database `app` of the same PostgreSQL. The producer and the workers also write hydo
// history records (HYDO-HIST tasks ...) on the guest console for hydo's messages check: a send
// for each trigger the engine acknowledged, and a receive for each run a worker started.
package main

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/kakoc/hydo/sdk/go/hydohist"
)

var hist = hydohist.New("tasks")

// dsn is the application's database; Hatchet's own database is `hatchet` on the same server.
func dsn() string {
	if v := os.Getenv("HQ_DATABASE_URL"); v != "" {
		return v
	}
	return "postgres://hatchet:hatchet@postgres:5432/app?sslmode=disable&connect_timeout=5"
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
	fmt.Fprintf(os.Stderr, "hq: "+format+"\n", a...)
	os.Exit(code)
}

func envInt(name string, def int64) int64 {
	if v, err := strconv.ParseInt(os.Getenv(name), 10, 64); err == nil {
		return v
	}
	return def
}

func main() {
	if len(os.Args) < 2 {
		die(2, "usage: hq migrate|worker|op|drain|check|ready ...")
	}
	ctx := context.Background()
	switch os.Args[1] {
	case "migrate":
		if err := migrate(ctx); err != nil {
			die(1, "migrate: %v", err)
		}
	case "worker":
		if len(os.Args) < 3 {
			die(2, "usage: hq worker <name>")
		}
		if err := worker(ctx, os.Args[2]); err != nil {
			die(1, "worker: %v", err)
		}
	case "op":
		if len(os.Args) < 3 {
			die(2, "usage: hq op <kind>")
		}
		os.Exit(op(ctx, os.Args[2]))
	case "drain":
		os.Exit(drain(ctx))
	case "check":
		if len(os.Args) < 3 {
			die(2, "usage: hq check <rule>")
		}
		os.Exit(check(ctx, os.Args[2]))
	case "cases", "mutate":
		if len(os.Args) < 3 {
			die(2, "usage: hq cases|mutate <rule>")
		}
		if os.Args[1] == "cases" {
			os.Exit(casesOf(ctx, os.Args[2]))
		}
		os.Exit(mutate(ctx, os.Args[2]))
	case "ready":
		os.Exit(ready(ctx, os.Args[2:]))
	default:
		die(2, "unknown command %q", os.Args[1])
	}
}

func ready(ctx context.Context, args []string) int {
	ctx, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	if len(args) > 0 {
		if _, err := os.Stat("/tmp/hq-" + args[0] + ".started"); err != nil {
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

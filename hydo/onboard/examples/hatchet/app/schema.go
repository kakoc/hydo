package main

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

// The application's database: one row per run of a task, written by the worker that ran it,
// and the producer's record of the cancellations the API confirmed.
const schema = `
CREATE TABLE IF NOT EXISTS app_run (
	seq          bigserial PRIMARY KEY,
	kind         text NOT NULL,        -- the task: plain, sched, limited, rated, slow, tick, a, b, c
	m            text NOT NULL,        -- the producer's message id ('' for a cron run)
	run_id       text NOT NULL,        -- Hatchet's workflow run id
	task_run_id  text NOT NULL,        -- Hatchet's task run id
	key          text NOT NULL DEFAULT '',   -- the concurrency key of 'limited'
	sched_at     text NOT NULL DEFAULT '',   -- a cron run's hatchet__cron_scheduled_at, a scheduled run's time (RFC 3339)
	worker       text NOT NULL,
	attempt      int  NOT NULL,        -- Hatchet's retry count
	boot         text NOT NULL,
	mono_start   bigint NOT NULL,      -- the guest's CLOCK_MONOTONIC in ns
	mono_end     bigint,
	started_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
	finished_at  timestamptz,
	finished_seq bigint,               -- the order of the finish among all starts and finishes
	outcome      text                  -- ok, error, cancelled
);
CREATE INDEX IF NOT EXISTS app_run_m ON app_run (m);
CREATE INDEX IF NOT EXISTS app_run_run ON app_run (run_id);
CREATE TABLE IF NOT EXISTS app_cancel (
	m          text PRIMARY KEY,
	run_id     text NOT NULL,
	seen_at    timestamptz NOT NULL DEFAULT clock_timestamp(),
	status     text NOT NULL          -- the status the API reported after the cancel
);
`

// migrate creates the database `app` and its tables. It tries again for up to a minute, because
// PostgreSQL's image restarts its server once after it initialized the data directory.
func migrate(ctx context.Context) error {
	deadline := time.Now().Add(60 * time.Second)
	var err error
	for time.Now().Before(deadline) {
		if err = migrateOnce(ctx); err == nil {
			return nil
		}
		time.Sleep(time.Second)
	}
	return err
}

func migrateOnce(ctx context.Context) error {
	c, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	admin := strings.Replace(dsn(), "/app?", "/hatchet?", 1)
	conn, err := pgx.Connect(c, admin)
	if err != nil {
		return err
	}
	var exists bool
	if err := conn.QueryRow(c, `SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'app')`).Scan(&exists); err != nil {
		conn.Close(c)
		return err
	}
	if !exists {
		if _, err := conn.Exec(c, `CREATE DATABASE app`); err != nil {
			conn.Close(c)
			return err
		}
	}
	conn.Close(c)
	p, err := pool(c, 1)
	if err != nil {
		return err
	}
	defer p.Close()
	if _, err := p.Exec(c, schema); err != nil {
		return fmt.Errorf("schema: %w", err)
	}
	return nil
}

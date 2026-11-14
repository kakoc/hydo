package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/riverqueue/river/riverdriver/riverpgxv5"
	"github.com/riverqueue/river/rivermigrate"
	"golang.org/x/sys/unix"
)

// monoNS is CLOCK_MONOTONIC in ns: the time of hydo's history records, the same for every
// process of one boot, and unaffected by clock steps.
func monoNS() int64 {
	var ts unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &ts); err != nil {
		return 0
	}
	return ts.Nano()
}

const appSchema = `
CREATE TABLE IF NOT EXISTS app_order (
    id bigint PRIMARY KEY,
    op text NOT NULL,
    processed int NOT NULL DEFAULT 0
);
-- one row per run of a job: written before the work starts, finished after it
CREATE TABLE IF NOT EXISTS app_run (
    id bigserial PRIMARY KEY,
    job_id bigint NOT NULL,
    kind text NOT NULL,
    attempt int NOT NULL,
    client text NOT NULL,
    proc int NOT NULL,
    boot text NOT NULL,
    mono_start bigint NOT NULL,
    wall_start timestamptz NOT NULL,
    scheduled_at timestamptz NOT NULL,
    rb boolean NOT NULL DEFAULT false,
    mono_end bigint,
    outcome text
);
-- one row per committed effect of a job that completes in its transaction (JobCompleteTx)
CREATE TABLE IF NOT EXISTS app_effect (
    id bigserial PRIMARY KEY,
    job_id bigint NOT NULL,
    kind text NOT NULL,
    attempt int NOT NULL,
    client text NOT NULL,
    run_id bigint NOT NULL,
    key text
);
-- what JobCancel answered
CREATE TABLE IF NOT EXISTS app_cancel (
    job_id bigint PRIMARY KEY,
    op text NOT NULL,
    state text NOT NULL
);
`

// migrate runs the migrations, and tries again for up to a minute: the PostgreSQL image's
// entrypoint first runs a temporary server on its Unix socket only, which pg_isready (the
// readiness check from the Compose file) accepts, and then restarts it.
func migrate(ctx context.Context) error {
	var err error
	for i := 0; i < 30; i++ {
		if err = migrateOnce(ctx); err == nil {
			return nil
		}
		fmt.Fprintln(os.Stderr, "riverq: migrate:", err)
		time.Sleep(2 * time.Second)
	}
	return err
}

func migrateOnce(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	p, err := pool(ctx, 2)
	if err != nil {
		return err
	}
	defer p.Close()
	m, err := rivermigrate.New(riverpgxv5.New(p), nil)
	if err != nil {
		return err
	}
	if _, err := m.Migrate(ctx, rivermigrate.DirectionUp, nil); err != nil {
		return err
	}
	if _, err = p.Exec(ctx, appSchema); err != nil {
		return err
	}
	// An experiment of RESULTS.md, finding 1, not River's schema: RIVERQ_LEADER_TABLE=logged
	// makes River's leader table survive a crash of PostgreSQL.
	if os.Getenv("RIVERQ_LEADER_TABLE") == "logged" {
		_, err = p.Exec(ctx, `ALTER TABLE river_leader SET LOGGED`)
	}
	return err
}

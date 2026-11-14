package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"math/rand/v2"
	"os"
	"strconv"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/kakoc/hydo/sdk/go/hydohist"
	"github.com/riverqueue/river"
	"github.com/riverqueue/river/riverdriver/riverpgxv5"
)

type hydohistOp = hydohist.Op

func invokeAck(proc int, it string) *hydohist.Op { return hist.Invoke(proc, "ack", "jobs", "-", it) }

// outcome appends "<id> ack|failed|unknown" to hydo's outcomes file (the generic rule reads it).
func outcome(id, what string) {
	p := os.Getenv("HYDO_OUTCOMES")
	if p == "" {
		return
	}
	f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "%s %s\n", id, what)
}

// op runs one producer operation. Exit 0: acknowledged; 1: failed or unknown.
func op(ctx context.Context, kind string) int {
	id := os.Getenv("HYDO_CMD_ID")
	if id == "" {
		id = strconv.FormatInt(time.Now().UnixNano()%1000000, 10)
	}
	proc, _ := strconv.Atoi(id)
	seed := envInt("HYDO_SEED", time.Now().UnixNano())
	r := rand.New(rand.NewPCG(uint64(seed), 7))
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	p, err := pool(ctx, 2)
	if err != nil {
		fmt.Fprintln(os.Stderr, "pool:", err)
		return 1
	}
	defer p.Close()
	client, err := river.NewClient(riverpgxv5.New(p), &river.Config{
		Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, "client:", err)
		return 1
	}
	short := func() (context.Context, context.CancelFunc) { return context.WithTimeout(ctx, 5*time.Second) }
	switch kind {
	case "plain", "sched":
		opid := kind[:1] + id
		m := opid
		var args river.JobArgs = PlainArgs{M: m, Op: opid, Ms: 10 + r.IntN(150)}
		var opts *river.InsertOpts
		it := m
		if kind == "sched" {
			d := 300 + r.IntN(3000)
			args = SchedArgs{M: m, Op: opid, Ms: 10 + r.IntN(50)}
			opts = &river.InsertOpts{ScheduledAt: time.Now().Add(time.Duration(d) * time.Millisecond)}
			it = fmt.Sprintf("%s;d=%d", m, d)
		}
		h := hist.Invoke(proc, "send", "jobs", it)
		c, cc := short()
		res, err := client.Insert(c, args, opts)
		cc()
		if err != nil {
			fmt.Fprintln(os.Stderr, "insert:", err)
			h.Info()
			outcome(opid, "unknown")
			return 1
		}
		h.Ok("jobs", fmt.Sprintf("%s;o=%d", it, res.Job.ID))
		outcome(opid, "ack")
		fmt.Println("inserted", res.Job.ID)
		return 0
	case "order":
		// a business transaction: the order row and its job commit together, or neither does
		opid := "o" + id
		rb := r.IntN(3) == 0
		h := hist.Invoke(proc, "send", "jobs", opid)
		c, cc := short()
		defer cc()
		tx, err := p.Begin(c)
		if err != nil {
			h.Fail()
			outcome(opid, "failed")
			return 1
		}
		defer tx.Rollback(context.Background())
		oid, _ := strconv.ParseInt(id, 10, 64)
		if _, err := tx.Exec(c, `INSERT INTO app_order (id, op) VALUES ($1, $2)`, oid, opid); err != nil {
			h.Fail()
			outcome(opid, "failed")
			return 1
		}
		res, err := client.InsertTx(c, tx, OrderArgs{M: opid, Op: opid, Order: oid, RB: rb}, nil)
		if err != nil {
			h.Fail()
			outcome(opid, "failed")
			return 1
		}
		if rb {
			_ = tx.Rollback(c)
			h.Fail()
			outcome(opid, "failed")
			fmt.Println("rolled back the order and job", res.Job.ID)
			return 0
		}
		if err := tx.Commit(c); err != nil {
			fmt.Fprintln(os.Stderr, "commit:", err)
			h.Info()
			outcome(opid, "unknown")
			return 1
		}
		h.Ok("jobs", fmt.Sprintf("%s;o=%d", opid, res.Job.ID))
		outcome(opid, "ack")
		return 0
	case "uniq":
		// unique by key and by the hour; a lost answer is retried with the same arguments
		key := "k" + strconv.Itoa(r.IntN(5))
		opid := "u" + id + ":" + key
		m := uniqMessage(key, time.Now())
		h := hist.Invoke(proc, "send", "jobs", m)
		opts := &river.InsertOpts{UniqueOpts: river.UniqueOpts{ByArgs: true, ByPeriod: time.Hour}}
		for attempt := 0; attempt < 3; attempt++ {
			c, cc := short()
			res, err := client.Insert(c, UniqArgs{Key: key, Op: opid}, opts)
			cc()
			if err == nil {
				h.Ok("jobs", fmt.Sprintf("%s;o=%d", m, res.Job.ID))
				outcome(opid, "ack")
				fmt.Println("inserted", res.Job.ID, "duplicate:", res.UniqueSkippedAsDuplicate)
				return 0
			}
			fmt.Fprintln(os.Stderr, "insert:", err)
			time.Sleep(300 * time.Millisecond)
		}
		h.Info()
		outcome(opid, "unknown")
		return 1
	case "cancel":
		// a slow job, cancelled while it waits or while it runs
		opid := "c" + id
		c, cc := short()
		res, err := client.Insert(c, SlowArgs{Op: opid, Ms: 500 + r.IntN(1500)}, nil)
		cc()
		if err != nil {
			outcome(opid, "unknown")
			return 1
		}
		time.Sleep(time.Duration(r.IntN(1500)) * time.Millisecond)
		c, cc = short()
		row, err := client.JobCancel(c, res.Job.ID)
		cc()
		if err != nil {
			fmt.Fprintln(os.Stderr, "cancel:", err)
			outcome(opid, "unknown")
			return 1
		}
		c, cc = short()
		_, err = p.Exec(c, `INSERT INTO app_cancel (job_id, op, state) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING`,
			row.ID, opid, string(row.State))
		cc()
		if err != nil {
			outcome(opid, "unknown")
			return 1
		}
		outcome(opid, "ack")
		fmt.Println("cancel of", row.ID, "answered", row.State)
		return 0
	}
	fmt.Fprintln(os.Stderr, "unknown op", kind)
	return 2
}

var _ = pgx.ErrNoRows

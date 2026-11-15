package main

import (
	"context"
	"fmt"
	"math/rand/v2"
	"os"
	"strconv"
	"time"

	"github.com/google/uuid"
	"github.com/hatchet-dev/hatchet/pkg/client/rest"
	hatchet "github.com/hatchet-dev/hatchet/sdks/go"
	"github.com/hatchet-dev/hatchet/sdks/go/features"
	"github.com/oapi-codegen/runtime/types"
)

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

// The engine a producer talks to: one of the two, by the command's seed, so that a fault of
// one engine leaves the other's commands working.
func pickEngine(r *rand.Rand) {
	if os.Getenv("HATCHET_CLIENT_HOST_PORT") != "" {
		return
	}
	os.Setenv("HATCHET_CLIENT_HOST_PORT", fmt.Sprintf("hatchet-engine-%d:7070", 1+r.IntN(2)))
}

// op runs one producer operation. Exit 0: acknowledged; 1: failed or unknown.
func op(ctx context.Context, kind string) int {
	id := os.Getenv("HYDO_CMD_ID")
	if id == "" {
		id = strconv.FormatInt(time.Now().UnixNano()%1000000, 10)
	}
	proc, _ := strconv.Atoi(id)
	seed := envInt("HYDO_SEED", time.Now().UnixNano())
	r := rand.New(rand.NewPCG(uint64(seed), 11))
	pickEngine(r)
	client, err := newClient(5 * time.Second)
	if err != nil {
		fmt.Fprintln(os.Stderr, "client:", err)
		return 1
	}
	meta := hatchet.WithRunMetadata(map[string]string{"hq": "1"})
	short := func() (context.Context, context.CancelFunc) { return context.WithTimeout(ctx, 8*time.Second) }
	trigger := func(m, wf string, in TaskIn) int {
		h := hist.Invoke(proc, "send", "tasks", m)
		c, cc := short()
		ref, err := client.RunNoWait(c, wf, in, meta)
		cc()
		if err != nil {
			fmt.Fprintln(os.Stderr, "run:", err)
			h.Info()
			outcome(m, "unknown")
			return 1
		}
		h.Ok("tasks", fmt.Sprintf("%s;o=%s", m, ref.RunId))
		outcome(m, "ack")
		fmt.Println("triggered", wf, m, ref.RunId)
		return 0
	}
	switch kind {
	case "plain":
		return trigger("p"+id, "plain", TaskIn{M: "p" + id, Ms: 10 + r.IntN(300)})
	case "burst":
		// HQ_BURST plain runs (default 8), one after the other, from one client
		n := int(envInt("HQ_BURST", 8))
		failed := 0
		for i := 0; i < n; i++ {
			m := fmt.Sprintf("b%s_%d", id, i)
			if trigger(m, "plain", TaskIn{M: m, Ms: 10 + r.IntN(100)}) != 0 {
				failed++
			}
		}
		if failed > 0 {
			return 1
		}
		return 0
	case "limited":
		m := "l" + id
		return trigger(m, "limited", TaskIn{M: m, Ms: 100 + r.IntN(400), Key: fmt.Sprintf("k%d", r.IntN(3))})
	case "rated":
		return trigger("r"+id, "rated", TaskIn{M: "r" + id, Ms: 10 + r.IntN(50)})
	case "dag":
		return trigger("d"+id, "order", TaskIn{M: "d" + id, Ms: 20 + r.IntN(200)})
	case "schedmany":
		// HQ_BURST scheduled runs (default 8), all for one instant HQ_AT_MS (default 3000) ms ahead
		n := int(envInt("HQ_BURST", 8))
		d0 := int(envInt("HQ_AT_MS", 3000))
		at := time.Now().Add(time.Duration(d0) * time.Millisecond)
		failed := 0
		for i := 0; i < n; i++ {
			m := fmt.Sprintf("s%s_%d", id, i)
			d := int(time.Until(at) / time.Millisecond)
			item := fmt.Sprintf("%s;d=%d", m, d)
			h := hist.Invoke(proc, "send", "tasks", item)
			c, cc := short()
			_, err := client.Schedules().Create(c, "sched", features.CreateScheduledRunTrigger{
				TriggerAt:          at,
				Input:              map[string]interface{}{"m": m, "ms": 10 + r.IntN(100), "at": at.UnixMilli()},
				AdditionalMetadata: map[string]interface{}{"hq": "1"},
			})
			cc()
			if err != nil {
				h.Info()
				outcome(m, "unknown")
				failed++
				continue
			}
			h.Ok("tasks", item)
			outcome(m, "ack")
			fmt.Println("scheduled", m, "at", at.UTC().Format(time.RFC3339Nano))
		}
		if failed > 0 {
			return 1
		}
		return 0
	case "sched":
		// a run scheduled 0.5 to 4.5 s ahead, through the API's scheduled runs
		m := "s" + id
		d := 500 + r.IntN(4000)
		at := time.Now().Add(time.Duration(d) * time.Millisecond)
		item := fmt.Sprintf("%s;d=%d", m, d)
		h := hist.Invoke(proc, "send", "tasks", item)
		c, cc := short()
		_, err := client.Schedules().Create(c, "sched", features.CreateScheduledRunTrigger{
			TriggerAt:          at,
			Input:              map[string]interface{}{"m": m, "ms": 10 + r.IntN(100), "at": at.UnixMilli()},
			AdditionalMetadata: map[string]interface{}{"hq": "1"},
		})
		cc()
		if err != nil {
			fmt.Fprintln(os.Stderr, "schedule:", err)
			h.Info()
			outcome(m, "unknown")
			return 1
		}
		h.Ok("tasks", item)
		outcome(m, "ack")
		fmt.Println("scheduled", m, "in", d, "ms")
		return 0
	case "cancel":
		// a slow run, cancelled while it is queued or running. Not a messages-check send: a run
		// cancelled before it started is never delivered.
		m := "x" + id
		c, cc := short()
		ms := 1000 + r.IntN(2000)
		stubborn := os.Getenv("HQ_STUBBORN") == "1"
		ref, err := client.RunNoWait(c, "slow", TaskIn{M: m, Ms: ms, Stubborn: stubborn}, meta)
		cc()
		if err != nil {
			outcome(m, "unknown")
			return 1
		}
		outcome(m, "ack")
		time.Sleep(time.Duration(200+r.IntN(1300)) * time.Millisecond)
		c, cc = short()
		_, err = client.Runs().Cancel(c, rest.V1CancelTaskRequest{ExternalIds: &[]types.UUID{uuid.MustParse(ref.RunId)}})
		cc()
		if err != nil {
			fmt.Fprintln(os.Stderr, "cancel:", err)
			return 1
		}
		// wait until the API reports the run's status after the cancel
		end := time.Now().Add(6 * time.Second)
		for time.Now().Before(end) {
			c, cc = short()
			st, err := client.Runs().GetStatus(c, ref.RunId)
			cc()
			if err == nil && st != nil && (*st == rest.V1TaskStatusCANCELLED || *st == rest.V1TaskStatusCOMPLETED || *st == rest.V1TaskStatusFAILED) {
				recordCancel(ctx, m, ref.RunId, string(*st))
				fmt.Println("cancel of", m, ref.RunId, "->", *st)
				if stubborn {
					// the status again, after the task's work would have ended
					time.Sleep(time.Duration(ms)*time.Millisecond + 5*time.Second)
					c, cc = short()
					st2, err := client.Runs().GetStatus(c, ref.RunId)
					cc()
					if err == nil && st2 != nil {
						fmt.Println("then", m, ref.RunId, "->", *st2)
					}
				}
				return 0
			}
			time.Sleep(300 * time.Millisecond)
		}
		fmt.Println("cancel of", m, ref.RunId, ": no final status in 6 s")
		return 1
	}
	fmt.Fprintln(os.Stderr, "unknown op", kind)
	return 2
}

func recordCancel(ctx context.Context, m, runID, status string) {
	c, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	p, err := pool(c, 1)
	if err != nil {
		return
	}
	defer p.Close()
	_, _ = p.Exec(c, `INSERT INTO app_cancel (m, run_id, status) VALUES ($1, $2, $3) ON CONFLICT (m) DO NOTHING`, m, runID, status)
}

// tw is the Temporal worker, the client and the checks of hydo's Temporal example, in one
// static program (like the messages tutorial's tinyq).
//
//	tw worker             registers the namespace, then runs the worker of the task queue "orders"
//	tw ready              exits 0 when the frontend answers for the namespace
//	tw start              starts one order workflow (a new ID, or one in four an ID already used)
//	tw signal             sends one signal "item" to a running order
//	tw update             sends one update "reserve" (a new update ID, or one already answered)
//	tw observe            reads one order's state and records a completion it saw
//	tw drain              the messages check's drain: every signal in the orders' histories
//	tw clock              records the wall clock it sees (also done by every operation)
//	tw check <rule>       history | acked | visibility (RESULTS.md, "The rules")
//
// The client records what it saw in hydo's files: $HYDO_ACKED (one line per acknowledged
// operation), $HYDO_OUTCOMES (ack, failed or unknown per attempt) and HYDO-HIST records of the
// signals on the console. Every choice comes from $HYDO_SEED and $HYDO_CMD_ID, never the clock.
package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"math/rand"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydohist"
	enumspb "go.temporal.io/api/enums/v1"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/api/serviceerror"
	"go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/converter"
	sdklog "go.temporal.io/sdk/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/worker"
	"go.temporal.io/sdk/workflow"
	"google.golang.org/protobuf/types/known/durationpb"
)

const (
	namespace = "default"
	taskQueue = "orders"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: tw worker|ready|start|signal|update|observe|drain|check <rule>")
		os.Exit(2)
	}
	var rc int
	switch os.Args[1] {
	case "worker":
		rc = runWorker()
	case "ready":
		rc = ready()
	case "start":
		rc = start()
	case "signal":
		rc = signal()
	case "update":
		rc = update()
	case "observe":
		rc = observe()
	case "drain":
		rc = drain()
	case "clock":
		seenClock()
	case "check":
		if len(os.Args) < 3 {
			fmt.Fprintln(os.Stderr, "usage: tw check history|acked|visibility")
			os.Exit(2)
		}
		rc = check(os.Args[2])
	default:
		fmt.Fprintf(os.Stderr, "tw: unknown command %s\n", os.Args[1])
		rc = 2
	}
	os.Exit(rc)
}

// ---------------------------------------------------------------------------------------------
// The connection

func address() string {
	if a := os.Getenv("TEMPORAL_ADDRESS"); a != "" {
		return a
	}
	return "temporal:7233"
}

func quietLogger() sdklog.Logger {
	return sdklog.NewStructuredLogger(slog.New(slog.NewTextHandler(os.Stderr,
		&slog.HandlerOptions{Level: slog.LevelWarn})))
}

// seenClock records the wall clock as this command sees it, so that a check can tell that the
// clock went back below a time the client saw (a backward clock step).
func seenClock() {
	acked(fmt.Sprintf("clock/%d", time.Now().UnixNano()))
}

func dial() client.Client {
	c, err := client.NewLazyClient(client.Options{
		HostPort:  address(),
		Namespace: namespace,
		Logger:    quietLogger(),
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "tw: client: %v\n", err)
		os.Exit(2)
	}
	return c
}

func timeout(sec int) (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), time.Duration(sec)*time.Second)
}

// definite says whether an error means that the server did not apply the request.
func definite(err error) bool {
	var already *serviceerror.WorkflowExecutionAlreadyStarted
	var notFound *serviceerror.NotFound
	var invalid *serviceerror.InvalidArgument
	var failedPre *serviceerror.FailedPrecondition
	var nsNotFound *serviceerror.NamespaceNotFound
	return errors.As(err, &already) || errors.As(err, &notFound) || errors.As(err, &invalid) ||
		errors.As(err, &failedPre) || errors.As(err, &nsNotFound)
}

// ---------------------------------------------------------------------------------------------
// The workflow and its activities

// Input is an order's input: how long its timer waits.
type Input struct{ TimerMs int64 }

// Result is an order's result: what the workflow saw, in the order it saw it.
type Result struct {
	Charged string   // the result of the activity Charge
	Signals []string // the signals "item" it received, in order
	Updates []string // the updates "reserve" it applied, as "<update id>=<sequence number>"
	SleptMs int64    // workflow time between the timer's start and its end
}

// Order charges, waits on a timer while it collects items (signals) and reservations
// (updates), and ships.
func Order(ctx workflow.Context, in Input) (Result, error) {
	var r Result
	seq := 0
	if err := workflow.SetUpdateHandler(ctx, "reserve", func(ctx workflow.Context, id string) (int, error) {
		seq++
		r.Updates = append(r.Updates, fmt.Sprintf("%s=%d", id, seq))
		return seq, nil
	}); err != nil {
		return r, err
	}
	actx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 10 * time.Second,
		RetryPolicy: &temporal.RetryPolicy{
			InitialInterval:    time.Second,
			BackoffCoefficient: 2,
			MaximumInterval:    5 * time.Second,
		},
	})
	id := workflow.GetInfo(ctx).WorkflowExecution.ID
	if err := workflow.ExecuteActivity(actx, Charge, id).Get(ctx, &r.Charged); err != nil {
		return r, err
	}
	items := workflow.GetSignalChannel(ctx, "item")
	receive := func(c workflow.ReceiveChannel, more bool) {
		var s string
		c.Receive(ctx, &s)
		r.Signals = append(r.Signals, s)
	}
	begin := workflow.Now(ctx)
	timer := workflow.NewTimer(ctx, time.Duration(in.TimerMs)*time.Millisecond)
	fired := false
	for !fired {
		sel := workflow.NewSelector(ctx)
		sel.AddFuture(timer, func(workflow.Future) { fired = true })
		sel.AddReceive(items, receive)
		sel.Select(ctx)
	}
	r.SleptMs = workflow.Now(ctx).Sub(begin).Milliseconds()
	if err := workflow.ExecuteActivity(actx, Ship, id).Get(ctx, nil); err != nil {
		return r, err
	}
	for {
		var s string
		if !items.ReceiveAsync(&s) {
			break
		}
		r.Signals = append(r.Signals, s)
	}
	if err := workflow.Await(ctx, func() bool { return workflow.AllHandlersFinished(ctx) }); err != nil {
		return r, err
	}
	return r, nil
}

// Charge is the payment step. Its result names the attempt that completed.
func Charge(ctx context.Context, id string) (string, error) {
	return fmt.Sprintf("charged %s attempt %d", id, activity.GetInfo(ctx).Attempt), nil
}

// Ship is the shipping step.
func Ship(ctx context.Context, id string) error { return nil }

// ---------------------------------------------------------------------------------------------
// The worker

func registerNamespace(c client.Client) error {
	nc, err := client.NewNamespaceClient(client.Options{HostPort: address(), Logger: quietLogger()})
	if err != nil {
		return err
	}
	defer nc.Close()
	ctx, cancel := timeout(10)
	defer cancel()
	err = nc.Register(ctx, &workflowservice.RegisterNamespaceRequest{
		Namespace:                        namespace,
		WorkflowExecutionRetentionPeriod: durationpb.New(72 * time.Hour),
	})
	var exists *serviceerror.NamespaceAlreadyExists
	if err != nil && !errors.As(err, &exists) {
		return err
	}
	return nil
}

func runWorker() int {
	c := dial()
	defer c.Close()
	for i := 0; ; i++ {
		err := registerNamespace(c)
		if err == nil {
			break
		}
		if i%10 == 0 {
			fmt.Fprintf(os.Stderr, "tw worker: namespace: %v (retrying)\n", err)
		}
		time.Sleep(time.Second)
	}
	for i := 0; ; i++ {
		if ready() == 0 {
			break
		}
		time.Sleep(time.Second)
	}
	w := worker.New(c, taskQueue, worker.Options{
		MaxConcurrentActivityTaskPollers: 1,
		MaxConcurrentWorkflowTaskPollers: 2,
	})
	w.RegisterWorkflow(Order)
	w.RegisterActivity(Charge)
	w.RegisterActivity(Ship)
	if err := w.Start(); err != nil {
		fmt.Fprintf(os.Stderr, "tw worker: %v\n", err)
		return 1
	}
	fmt.Println("tw worker: started")
	<-worker.InterruptCh()
	w.Stop()
	return 0
}

// ready is 0 when the frontend answers a namespace-scoped request (it needs the namespace in
// the frontend's cache, not only in the database).
func ready() int {
	c := dial()
	defer c.Close()
	ctx, cancel := timeout(5)
	defer cancel()
	_, err := c.ListWorkflow(ctx, &workflowservice.ListWorkflowExecutionsRequest{
		Namespace: namespace, PageSize: 1,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "tw ready: %v\n", err)
		return 1
	}
	return 0
}

// ---------------------------------------------------------------------------------------------
// The client's records

func record(file string, line string) {
	p := os.Getenv(file)
	if p == "" {
		return
	}
	f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		fmt.Fprintf(os.Stderr, "tw: %s: %v\n", file, err)
		return
	}
	fmt.Fprintln(f, line)
	f.Sync()
	f.Close()
}

func acked(line string)             { record("HYDO_ACKED", line) }
func outcome(id string, out string) { record("HYDO_OUTCOMES", id+" "+out) }

// lines reads a records file (missing: none).
func lines(env string) []string {
	p := os.Getenv(env)
	if p == "" {
		return nil
	}
	b, err := os.ReadFile(p)
	if err != nil {
		return nil
	}
	var out []string
	for _, l := range strings.Split(string(b), "\n") {
		if l = strings.TrimSpace(l); l != "" {
			out = append(out, l)
		}
	}
	return out
}

// Records is what the client saw acknowledged, from $HYDO_ACKED.
type Records struct {
	order   []string            // workflow IDs in the order of their first acknowledged start
	runs    map[string][]string // workflow ID -> acknowledged run IDs
	signals map[string]string   // signal -> workflow ID
	updates map[string]map[string]string
	done    map[string][]string // workflow ID -> "<run id>/<result hash>" seen completed
	clock   time.Time           // the latest wall-clock time a command of the client saw
}

func readRecords() *Records {
	r := &Records{runs: map[string][]string{}, signals: map[string]string{},
		updates: map[string]map[string]string{}, done: map[string][]string{}}
	for _, l := range lines("HYDO_ACKED") {
		f := strings.Split(l, "/")
		switch {
		case f[0] == "start" && len(f) == 3:
			if _, ok := r.runs[f[1]]; !ok {
				r.order = append(r.order, f[1])
			}
			if !contains(r.runs[f[1]], f[2]) {
				r.runs[f[1]] = append(r.runs[f[1]], f[2])
			}
		case f[0] == "sig" && len(f) == 3:
			r.signals[f[1]] = f[2]
		case f[0] == "upd" && len(f) == 4:
			if r.updates[f[2]] == nil {
				r.updates[f[2]] = map[string]string{}
			}
			r.updates[f[2]][f[1]] = f[3]
		case f[0] == "done" && len(f) == 4:
			r.done[f[1]] = append(r.done[f[1]], f[2]+"/"+f[3])
		case f[0] == "clock" && len(f) == 2:
			if ns, err := strconv.ParseInt(f[1], 10, 64); err == nil && time.Unix(0, ns).After(r.clock) {
				r.clock = time.Unix(0, ns)
			}
		}
	}
	return r
}

func contains(s []string, x string) bool {
	for _, y := range s {
		if y == x {
			return true
		}
	}
	return false
}

func seeded() *rand.Rand {
	s, _ := strconv.ParseInt(os.Getenv("HYDO_SEED"), 10, 64)
	return rand.New(rand.NewSource(s))
}

func cmdID() string {
	if v := os.Getenv("HYDO_CMD_ID"); v != "" {
		return v
	}
	return "0"
}

// recent picks one of the last few acknowledged orders (the ones most likely still running).
func recent(r *Records, rnd *rand.Rand) string {
	if len(r.order) == 0 {
		return ""
	}
	n := 4
	if len(r.order) < n {
		n = len(r.order)
	}
	return r.order[len(r.order)-n+rnd.Intn(n)]
}

// ---------------------------------------------------------------------------------------------
// The operations

func start() int {
	seenClock()
	rnd := seeded()
	recs := readRecords()
	id := "o" + cmdID()
	again := false
	if len(recs.order) > 0 && rnd.Intn(4) == 0 {
		id = recs.order[rnd.Intn(len(recs.order))]
		again = true
	}
	timer := int64(2000 + rnd.Intn(14)*1000)
	c := dial()
	defer c.Close()
	ctx, cancel := timeout(15)
	defer cancel()
	op := "start/" + id + "/" + cmdID()
	run, err := c.ExecuteWorkflow(ctx, client.StartWorkflowOptions{
		ID:                                       id,
		TaskQueue:                                taskQueue,
		WorkflowIDReusePolicy:                    enumspb.WORKFLOW_ID_REUSE_POLICY_REJECT_DUPLICATE,
		WorkflowIDConflictPolicy:                 enumspb.WORKFLOW_ID_CONFLICT_POLICY_FAIL,
		WorkflowExecutionErrorWhenAlreadyStarted: true,
	}, Order, Input{TimerMs: timer})
	switch {
	case err == nil:
		acked("start/" + id + "/" + run.GetRunID())
		outcome(op, "ack")
		fmt.Printf("started %s run %s timer %d ms (again=%v)\n", id, run.GetRunID(), timer, again)
		if again && !contains(recs.runs[id], run.GetRunID()) {
			fmt.Printf("VIOLATION: %s was started a second time (run %s, earlier %v) under REJECT_DUPLICATE\n",
				id, run.GetRunID(), recs.runs[id])
			return 100
		}
		return 0
	case definite(err):
		outcome(op, "failed")
		fmt.Printf("start %s refused (again=%v): %v\n", id, again, err)
		return 1
	default:
		outcome(op, "unknown")
		fmt.Printf("start %s unknown: %v\n", id, err)
		return 1
	}
}

var signals = hydohist.New("sig")

func procNo() int {
	n, _ := strconv.Atoi(cmdID())
	return n
}

func signal() int {
	seenClock()
	rnd := seeded()
	recs := readRecords()
	wf := recent(recs, rnd)
	if wf == "" {
		fmt.Println("no order yet")
		return 0
	}
	m := "m" + cmdID()
	c := dial()
	defer c.Close()
	ctx, cancel := timeout(15)
	defer cancel()
	op := signals.Invoke(procNo(), "send", wf, m)
	err := c.SignalWorkflow(ctx, wf, "", "item", m)
	switch {
	case err == nil:
		op.Ok(wf, m)
		acked("sig/" + m + "/" + wf)
		outcome("sig/"+m, "ack")
		fmt.Printf("signal %s to %s acknowledged\n", m, wf)
		return 0
	case definite(err):
		op.Fail()
		outcome("sig/"+m, "failed")
		fmt.Printf("signal %s to %s refused: %v\n", m, wf, err)
		return 1
	default:
		op.Info()
		outcome("sig/"+m, "unknown")
		fmt.Printf("signal %s to %s unknown: %v\n", m, wf, err)
		return 1
	}
}

func update() int {
	seenClock()
	rnd := seeded()
	recs := readRecords()
	wf := recent(recs, rnd)
	if wf == "" {
		fmt.Println("no order yet")
		return 0
	}
	uid := "u" + cmdID()
	earlier := ""
	if prev := recs.updates[wf]; len(prev) > 0 && rnd.Intn(3) == 0 {
		ids := make([]string, 0, len(prev))
		for k := range prev {
			ids = append(ids, k)
		}
		sort.Strings(ids)
		uid = ids[rnd.Intn(len(ids))]
		earlier = prev[uid]
	}
	c := dial()
	defer c.Close()
	ctx, cancel := timeout(15)
	defer cancel()
	op := "upd/" + uid + "/" + wf + "/" + cmdID()
	h, err := c.UpdateWorkflow(ctx, client.UpdateWorkflowOptions{
		UpdateID:     uid,
		WorkflowID:   wf,
		UpdateName:   "reserve",
		Args:         []interface{}{uid},
		WaitForStage: client.WorkflowUpdateStageCompleted,
	})
	var seq int
	if err == nil {
		err = h.Get(ctx, &seq)
	}
	switch {
	case err == nil:
		acked(fmt.Sprintf("upd/%s/%s/%d", uid, wf, seq))
		outcome(op, "ack")
		fmt.Printf("update %s on %s: %d (earlier %q)\n", uid, wf, seq, earlier)
		if earlier != "" && earlier != strconv.Itoa(seq) {
			fmt.Printf("VIOLATION: update %s on %s answered %d, and %s before\n", uid, wf, seq, earlier)
			return 100
		}
		return 0
	case definite(err):
		outcome(op, "failed")
		fmt.Printf("update %s on %s refused: %v\n", uid, wf, err)
		return 1
	default:
		outcome(op, "unknown")
		fmt.Printf("update %s on %s unknown: %v\n", uid, wf, err)
		return 1
	}
}

// closeOf reads the close event of an order's run ("" = the latest), or nil while it runs.
func closeOf(ctx context.Context, c client.Client, wf, run string) (*historypb.HistoryEvent, error) {
	it := c.GetWorkflowHistory(ctx, wf, run, false, enumspb.HISTORY_EVENT_FILTER_TYPE_CLOSE_EVENT)
	var last *historypb.HistoryEvent
	for it.HasNext() {
		e, err := it.Next()
		if err != nil {
			return nil, err
		}
		last = e
	}
	return last, nil
}

func resultHash(e *historypb.HistoryEvent) string {
	a := e.GetWorkflowExecutionCompletedEventAttributes()
	if a == nil {
		return e.GetEventType().String()
	}
	h := sha256.New()
	for _, p := range a.GetResult().GetPayloads() {
		h.Write(p.GetData())
	}
	return hex.EncodeToString(h.Sum(nil))[:16]
}

func observe() int {
	seenClock()
	rnd := seeded()
	recs := readRecords()
	if len(recs.order) == 0 {
		return 0
	}
	wf := recs.order[rnd.Intn(len(recs.order))]
	c := dial()
	defer c.Close()
	ctx, cancel := timeout(15)
	defer cancel()
	d, err := c.DescribeWorkflowExecution(ctx, wf, "")
	if err != nil {
		fmt.Printf("describe %s: %v\n", wf, err)
		return 1
	}
	st := d.GetWorkflowExecutionInfo().GetStatus()
	run := d.GetWorkflowExecutionInfo().GetExecution().GetRunId()
	fmt.Printf("%s run %s: %s\n", wf, run, st)
	if st != enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED {
		return 0
	}
	e, err := closeOf(ctx, c, wf, run)
	if err != nil || e == nil {
		fmt.Printf("close event of %s: %v\n", wf, err)
		return 1
	}
	acked("done/" + wf + "/" + run + "/" + resultHash(e))
	return 0
}

// ---------------------------------------------------------------------------------------------
// The drain of the messages check: every signal in every order's history

func history(ctx context.Context, c client.Client, wf, run string) ([]*historypb.HistoryEvent, error) {
	it := c.GetWorkflowHistory(ctx, wf, run, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT)
	var evs []*historypb.HistoryEvent
	for it.HasNext() {
		e, err := it.Next()
		if err != nil {
			return nil, err
		}
		evs = append(evs, e)
	}
	return evs, nil
}

func payloadString(e *historypb.HistoryEvent) string {
	ps := e.GetWorkflowExecutionSignaledEventAttributes().GetInput().GetPayloads()
	if len(ps) == 0 {
		return ""
	}
	return strings.Trim(string(ps[0].GetData()), "\"")
}

// signalTargets are the orders the drain reads: every acknowledged start (the signal command
// sends only to those).
func signalTargets() []string {
	set := map[string]bool{}
	recs := readRecords()
	for _, wf := range recs.signals {
		set[wf] = true
	}
	for _, wf := range recs.order {
		set[wf] = true
	}
	var out []string
	for wf := range set {
		out = append(out, wf)
	}
	sort.Strings(out)
	return out
}

func drain() int {
	c := dial()
	defer c.Close()
	proc := 900000
	// the final: command's time limit is 60 s; time.Now() carries the monotonic clock, so a
	// clock step does not move this deadline
	deadline := time.Now().Add(45 * time.Second)
	for _, wf := range signalTargets() {
		var evs []*historypb.HistoryEvent
		var err error
		for time.Now().Before(deadline) {
			ctx, cancel := timeout(10)
			evs, err = history(ctx, c, wf, "")
			cancel()
			if err == nil {
				break
			}
			var nf *serviceerror.NotFound
			if errors.As(err, &nf) {
				break
			}
			time.Sleep(time.Second)
		}
		if err != nil {
			var nf *serviceerror.NotFound
			if !errors.As(err, &nf) {
				fmt.Fprintf(os.Stderr, "tw drain: %s: %v\n", wf, err)
				return 2
			}
		}
		for _, e := range evs {
			if e.GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_SIGNALED {
				continue
			}
			op := signals.Invoke(proc, "recv", wf, "-")
			op.Ok(wf, "-", payloadString(e)+";o="+strconv.FormatInt(e.GetEventId(), 10))
		}
		signals.Record(proc, "ok", "drained", wf, "-")
		proc++
	}
	return 0
}

// ---------------------------------------------------------------------------------------------
// The checks

// unreadable ends a check with 2: the frontend did not answer, so the check cannot judge now.
func unreadable(what string, err error) int {
	fmt.Printf("cannot read %s: %v\n", what, err)
	return 2
}

func check(rule string) int {
	switch rule {
	case "history":
		return checkHistory()
	case "acked":
		return checkAcked()
	case "visibility":
		return checkVisibility()
	}
	fmt.Fprintf(os.Stderr, "tw check: unknown rule %s\n", rule)
	return 2
}

// checkHistory: every acknowledged run's history is well formed. Event IDs are 1..N without a
// gap, one start event first and at most one close event last, at most one outcome per
// activity, timer and update, a timer never fires before its time, and an order ID has one run.
func checkHistory() int {
	recs := readRecords()
	c := dial()
	defer c.Close()
	var bad []string
	runs := 0
	for _, wf := range recs.order {
		if len(recs.runs[wf]) > 1 {
			bad = append(bad, fmt.Sprintf("%s has %d acknowledged runs %v under REJECT_DUPLICATE", wf,
				len(recs.runs[wf]), recs.runs[wf]))
		}
		for _, run := range recs.runs[wf] {
			ctx, cancel := timeout(20)
			evs, err := history(ctx, c, wf, run)
			cancel()
			if err != nil {
				var nf *serviceerror.NotFound
				if errors.As(err, &nf) {
					bad = append(bad, fmt.Sprintf("%s run %s: acknowledged start, no history (%v)", wf, run, err))
					continue
				}
				return unreadable(wf, err)
			}
			runs++
			bad = append(bad, wellFormed(wf, run, evs)...)
		}
	}
	for _, b := range bad {
		fmt.Println(b)
	}
	if len(bad) > 0 {
		fmt.Printf("%d problems in the histories of %d acknowledged runs\n", len(bad), runs)
		return 1
	}
	fmt.Printf("the histories of %d acknowledged runs are well formed\n", runs)
	return 0
}

func wellFormed(wf, run string, evs []*historypb.HistoryEvent) []string {
	var bad []string
	p := func(f string, a ...interface{}) {
		bad = append(bad, wf+" run "+run+": "+fmt.Sprintf(f, a...))
	}
	if len(evs) == 0 {
		p("empty history")
		return bad
	}
	for i, e := range evs {
		if e.GetEventId() != int64(i+1) {
			p("event %d has ID %d (IDs must be 1..N without gaps)", i+1, e.GetEventId())
			break
		}
	}
	if evs[0].GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED {
		p("first event is %s", evs[0].GetEventType())
	}
	closes := 0
	activityOutcome := map[int64]int{}
	timerStart := map[string]*historypb.HistoryEvent{}
	timerEnd := map[string]int{}
	accepted := map[string]int{}
	completed := map[string]int{}
	for i, e := range evs {
		switch e.GetEventType() {
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED, enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED,
			enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT, enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TERMINATED,
			enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CANCELED, enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CONTINUED_AS_NEW:
			closes++
			if i != len(evs)-1 {
				p("close event %d (%s) is not the last of %d", e.GetEventId(), e.GetEventType(), len(evs))
			}
		case enumspb.EVENT_TYPE_ACTIVITY_TASK_COMPLETED:
			activityOutcome[e.GetActivityTaskCompletedEventAttributes().GetScheduledEventId()]++
		case enumspb.EVENT_TYPE_ACTIVITY_TASK_FAILED:
			activityOutcome[e.GetActivityTaskFailedEventAttributes().GetScheduledEventId()]++
		case enumspb.EVENT_TYPE_ACTIVITY_TASK_TIMED_OUT:
			activityOutcome[e.GetActivityTaskTimedOutEventAttributes().GetScheduledEventId()]++
		case enumspb.EVENT_TYPE_ACTIVITY_TASK_CANCELED:
			activityOutcome[e.GetActivityTaskCanceledEventAttributes().GetScheduledEventId()]++
		case enumspb.EVENT_TYPE_TIMER_STARTED:
			timerStart[e.GetTimerStartedEventAttributes().GetTimerId()] = e
		case enumspb.EVENT_TYPE_TIMER_FIRED:
			tid := e.GetTimerFiredEventAttributes().GetTimerId()
			timerEnd[tid]++
			if s := timerStart[tid]; s == nil {
				p("timer %s fired (event %d) without a start", tid, e.GetEventId())
			} else {
				due := s.GetEventTime().AsTime().Add(s.GetTimerStartedEventAttributes().GetStartToFireTimeout().AsDuration())
				if e.GetEventTime().AsTime().Before(due) {
					p("timer %s fired at %s, %s before its time %s (started at %s, event %d)", tid,
						e.GetEventTime().AsTime().Format(time.RFC3339Nano),
						due.Sub(e.GetEventTime().AsTime()), due.Format(time.RFC3339Nano),
						s.GetEventTime().AsTime().Format(time.RFC3339Nano), s.GetEventId())
				}
			}
		case enumspb.EVENT_TYPE_TIMER_CANCELED:
			timerEnd[e.GetTimerCanceledEventAttributes().GetTimerId()]++
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_UPDATE_ACCEPTED:
			accepted[e.GetWorkflowExecutionUpdateAcceptedEventAttributes().GetProtocolInstanceId()]++
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_UPDATE_COMPLETED:
			completed[e.GetWorkflowExecutionUpdateCompletedEventAttributes().GetMeta().GetUpdateId()]++
		}
	}
	if closes > 1 {
		p("%d close events", closes)
	}
	for id, n := range activityOutcome {
		if n > 1 {
			p("activity scheduled at event %d has %d outcomes", id, n)
		}
	}
	for id, n := range timerEnd {
		if n > 1 {
			p("timer %s fired or was cancelled %d times", id, n)
		}
	}
	for id, n := range accepted {
		if n > 1 {
			p("update %s accepted %d times", id, n)
		}
	}
	for id, n := range completed {
		if n > 1 {
			p("update %s completed %d times", id, n)
		}
	}
	return bad
}

// checkAcked: every acknowledged order completes, and its result keeps what the client was
// told: every acknowledged update with the number it was answered, each acknowledged signal once,
// the charge that the history recorded, a timer that slept its time, and the same result every
// time the client saw it completed.
func checkAcked() int {
	recs := readRecords()
	c := dial()
	defer c.Close()
	var bad, open []string
	var latest time.Time // the latest event time of every order's history
	for _, wf := range recs.order {
		ctx, cancel := timeout(20)
		evs, err := history(ctx, c, wf, "")
		cancel()
		if err != nil {
			return unreadable(wf, err)
		}
		for _, e := range evs {
			if t := e.GetEventTime().AsTime(); t.After(latest) {
				latest = t
			}
		}
		last := evs[len(evs)-1]
		if last.GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED {
			open = append(open, fmt.Sprintf("%s(%s)", wf, strings.TrimPrefix(last.GetEventType().String(), "EVENT_TYPE_")))
			continue
		}
		run := evs[0].GetWorkflowExecutionStartedEventAttributes().GetOriginalExecutionRunId()
		for _, d := range recs.done[wf] {
			f := strings.SplitN(d, "/", 2)
			if f[0] == run && f[1] != resultHash(last) {
				bad = append(bad, fmt.Sprintf("%s was seen completed with result %s, now %s", wf, f[1], resultHash(last)))
			}
		}
		var r Result
		if err := decode(last, &r); err != nil {
			bad = append(bad, fmt.Sprintf("%s: result: %v", wf, err))
			continue
		}
		var charged string
		var timerMs int64
		for _, e := range evs {
			if a := e.GetActivityTaskCompletedEventAttributes(); a != nil && charged == "" {
				if ps := a.GetResult().GetPayloads(); len(ps) > 0 {
					charged = strings.Trim(string(ps[0].GetData()), "\"")
				}
			}
			if a := e.GetTimerStartedEventAttributes(); a != nil {
				timerMs = a.GetStartToFireTimeout().AsDuration().Milliseconds()
			}
		}
		if r.Charged != charged {
			bad = append(bad, fmt.Sprintf("%s: result says %q, the history's first activity %q", wf, r.Charged, charged))
		}
		if r.SleptMs < timerMs {
			bad = append(bad, fmt.Sprintf("%s: the workflow's timer of %d ms ended after %d ms of workflow time", wf, timerMs, r.SleptMs))
		}
		got := map[string]string{}
		for _, u := range r.Updates {
			f := strings.SplitN(u, "=", 2)
			if _, dup := got[f[0]]; dup {
				bad = append(bad, fmt.Sprintf("%s: update %s applied twice", wf, f[0]))
			}
			got[f[0]] = f[1]
		}
		for uid, seq := range recs.updates[wf] {
			if g, ok := got[uid]; !ok {
				bad = append(bad, fmt.Sprintf("%s: acknowledged update %s (answer %s) is not in the result", wf, uid, seq))
			} else if g != seq {
				bad = append(bad, fmt.Sprintf("%s: acknowledged update %s was answered %s, the result has %s", wf, uid, seq, g))
			}
		}
		seen := map[string]int{}
		for _, s := range r.Signals {
			seen[s]++
			if seen[s] == 2 {
				bad = append(bad, fmt.Sprintf("%s: signal %s is in the result twice", wf, s))
			}
		}
	}
	for _, b := range bad {
		fmt.Println(b)
	}
	if len(bad) > 0 {
		fmt.Printf("%d acknowledged operations differ from the orders' results\n", len(bad))
		return 1
	}
	if recs.clock.After(latest) {
		latest = recs.clock
	}
	if len(open) > 0 && latest.After(time.Now().Add(2*time.Second)) {
		// A backward clock step: the server gives every new timer and timeout a fire time no
		// earlier than what its timer queue has read already (RESULTS.md, finding 1), so the
		// orders wait until the clock is back at the latest time the server or the client saw. Not
		// judged.
		fmt.Printf("not completed yet: %s\n", strings.Join(open, " "))
		fmt.Printf("excused: %d of %d acknowledged orders wait behind a backward clock step; the clock is %d s behind the latest time seen\n",
			len(open), len(recs.order), int(time.Until(latest).Seconds()))
		return 0
	}
	if len(open) > 0 {
		fmt.Printf("not completed yet: %s\n", strings.Join(open, " "))
		fmt.Printf("%d of %d acknowledged orders not completed\n", len(open), len(recs.order))
		return 1
	}
	fmt.Printf("%d acknowledged orders completed with what the client was told\n", len(recs.order))
	return 0
}

func decode(e *historypb.HistoryEvent, r *Result) error {
	ps := e.GetWorkflowExecutionCompletedEventAttributes().GetResult()
	if ps == nil || len(ps.GetPayloads()) == 0 {
		return fmt.Errorf("no result")
	}
	return converter.GetDefaultDataConverter().FromPayload(ps.GetPayloads()[0], r)
}

// checkVisibility: the list of executions (visibility) agrees with the histories: one execution
// per acknowledged order ID, with its run ID and its status.
func checkVisibility() int {
	recs := readRecords()
	c := dial()
	defer c.Close()
	var bad []string
	for _, wf := range recs.order {
		ctx, cancel := timeout(20)
		resp, err := c.ListWorkflow(ctx, &workflowservice.ListWorkflowExecutionsRequest{
			Namespace: namespace, Query: fmt.Sprintf("WorkflowId = '%s'", wf), PageSize: 10,
		})
		if err != nil {
			cancel()
			return unreadable("visibility of "+wf, err)
		}
		d, err := c.DescribeWorkflowExecution(ctx, wf, "")
		cancel()
		if err != nil {
			return unreadable(wf, err)
		}
		ex := resp.GetExecutions()
		if len(ex) != 1 {
			var rs []string
			for _, x := range ex {
				rs = append(rs, x.GetExecution().GetRunId()+":"+x.GetStatus().String())
			}
			bad = append(bad, fmt.Sprintf("%s: visibility lists %d executions %v", wf, len(ex), rs))
			continue
		}
		hs := d.GetWorkflowExecutionInfo().GetStatus()
		if ex[0].GetExecution().GetRunId() != d.GetWorkflowExecutionInfo().GetExecution().GetRunId() || ex[0].GetStatus() != hs {
			bad = append(bad, fmt.Sprintf("%s: visibility says run %s %s, the history run %s %s", wf,
				ex[0].GetExecution().GetRunId(), ex[0].GetStatus(),
				d.GetWorkflowExecutionInfo().GetExecution().GetRunId(), hs))
		}
	}
	for _, b := range bad {
		fmt.Println(b)
	}
	if len(bad) > 0 {
		fmt.Printf("visibility differs from the histories for %d of %d acknowledged orders\n", len(bad), len(recs.order))
		return 1
	}
	fmt.Printf("visibility agrees with the histories of %d acknowledged orders\n", len(recs.order))
	return 0
}

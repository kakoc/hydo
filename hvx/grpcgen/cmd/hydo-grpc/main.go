// hydo-grpc is the generic gRPC driver that runs inside a hydo guest. It reads the
// project's schema and plan (written by hydo-grpc-gen) and has one subcommand per job:
//
//	hydo-grpc steps FROM TO        workload steps [FROM, TO) (manifest workload.run)
//	hydo-grpc retry                re-send operations whose outcome is unknown (workload.retry)
//	hydo-grpc drive <rpc> [N]      N operations of one RPC or scenario (a composer command;
//	                               exit 0 all ok, 1 one ended failed or unknown)
//	hydo-grpc call <rpc> [JSON]    one call with the given request (JSON, or "-" = stdin;
//	                               none = generated); prints the response; exit 0 ok,
//	                               1 definite failure, 2 unknown outcome (as invariants use)
//	hydo-grpc gen <rpc> [N]        print N generated requests (no call)
//	hydo-grpc list                 the RPCs and scenarios of the plan
//
// Files: $HYDO_GRPC_DIR (default /etc/hydo/grpc) holds desc.pb and plan.json; state shared
// by the processes of one run (op counter, value pools, the calls record) lives in
// $HYDO_GRPC_STATE (default /tmp/hydo/grpc). Seed: --seed, else $HYDO_GRPC_SEED, else the
// file $HYDO_GRPC_STATE/seed, else hydo_grpc_seed= on the kernel command line, else the plan's.
// $HYDO_GRPC_TARGET overrides the plan's target address.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"google.golang.org/protobuf/proto"

	"hydogrpc/internal/client"
	"hydogrpc/internal/outcome"
	"hydogrpc/internal/rng"
	"hydogrpc/internal/spec"
)

func envOr(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func die(f string, a ...interface{}) {
	fmt.Fprintf(os.Stderr, "hydo-grpc: "+f+"\n", a...)
	os.Exit(3)
}

func seedFrom(flagVal string, state string) *uint64 {
	try := func(s string) *uint64 {
		s = strings.TrimSpace(s)
		if s == "" {
			return nil
		}
		v, err := strconv.ParseUint(s, 0, 64)
		if err != nil {
			die("seed %q: %v", s, err)
		}
		return &v
	}
	if v := try(flagVal); v != nil {
		return v
	}
	if v := try(os.Getenv("HYDO_GRPC_SEED")); v != nil {
		return v
	}
	if b, err := os.ReadFile(state + "/seed"); err == nil {
		if v := try(string(b)); v != nil {
			return v
		}
	}
	if b, err := os.ReadFile("/proc/cmdline"); err == nil {
		for _, t := range strings.Fields(string(b)) {
			if strings.HasPrefix(t, "hydo_grpc_seed=") {
				return try(strings.TrimPrefix(t, "hydo_grpc_seed="))
			}
		}
	}
	return nil
}

type genEnv struct{ n uint64 }

func (g *genEnv) Unique() uint64                         { g.n++; return g.n }
func (g *genEnv) Take(*spec.From, *rng.R) (string, bool) { return "", false }

func main() {
	fs := flag.NewFlagSet("hydo-grpc", flag.ExitOnError)
	dir := fs.String("dir", envOr("HYDO_GRPC_DIR", "/etc/hydo/grpc"), "directory with desc.pb and plan.json")
	state := fs.String("state", envOr("HYDO_GRPC_STATE", "/tmp/hydo/grpc"), "state directory shared by one run's processes")
	seed := fs.String("seed", "", "seed (default: $HYDO_GRPC_SEED, <state>/seed, hydo_grpc_seed=, the plan's)")
	calls := fs.String("calls", os.Getenv("HYDO_GRPC_CALLS"), "calls record file (default <state>/calls)")
	fs.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: hydo-grpc [flags] steps FROM TO | retry | drive <rpc> [N] | call <rpc> [JSON|-] | gen <rpc> [N] | list")
		fs.PrintDefaults()
	}
	fs.Parse(os.Args[1:])
	args := fs.Args()
	if len(args) == 0 {
		fs.Usage()
		os.Exit(3)
	}
	c, err := client.New(client.Options{Desc: *dir + "/desc.pb", Plan: *dir + "/plan.json", StateDir: *state,
		Calls: *calls, Seed: seedFrom(*seed, *state)})
	if err != nil {
		die("%v", err)
	}
	defer c.Close()
	if t := os.Getenv("HYDO_GRPC_TARGET"); t != "" {
		c.Plan.Target = t
	}
	callsFile := *calls
	if callsFile == "" {
		callsFile = *state + "/calls"
	}
	num := func(i int, what string) int {
		if len(args) <= i {
			die("%s: missing %s", args[0], what)
		}
		n, err := strconv.Atoi(args[i])
		if err != nil {
			die("%s: %s %q: %v", args[0], what, args[i], err)
		}
		return n
	}
	switch args[0] {
	case "steps":
		c.Steps(num(1, "FROM"), num(2, "TO"))
	case "retry":
		if err := c.Retry(callsFile); err != nil {
			die("retry: %v", err)
		}
	case "drive":
		if len(args) < 2 {
			die("drive: which rpc?")
		}
		n := 0
		if len(args) > 2 {
			n = num(2, "N")
		}
		ok, err := c.Drive(args[1], n)
		if err != nil {
			die("drive: %v", err)
		}
		if !ok {
			c.Close()
			os.Exit(1) // a failed or unknown operation (the composer records it; not a violation)
		}
	case "call":
		if len(args) < 2 {
			die("call: which rpc?")
		}
		r, err := c.Plan.Find(args[1])
		if err != nil {
			die("%v", err)
		}
		var reqs []proto.Message
		switch {
		case len(args) > 2:
			body := args[2]
			if body == "-" {
				b, _ := io.ReadAll(os.Stdin)
				body = string(b)
			}
			if reqs, err = c.ParseJSON(r, body); err != nil {
				die("call: request: %v", err)
			}
		default:
			if reqs, err = c.Generate(r, rng.New(rng.Derive(c.Seed, 3)), &genEnv{}); err != nil {
				die("call: %v", err)
			}
		}
		res, resps := c.Call(r, reqs)
		switch res.Kind {
		case outcome.OK:
			fmt.Println(client.JSON(resps, r.ServerStreaming))
			os.Exit(0)
		case outcome.Fail:
			fmt.Printf("%s %s\n", outcome.CodeName(res.Code), res.Msg)
			os.Exit(1)
		default:
			fmt.Printf("%s %s\n", outcome.CodeName(res.Code), res.Msg)
			os.Exit(2)
		}
	case "gen":
		if len(args) < 2 {
			die("gen: which rpc?")
		}
		r, err := c.Plan.Find(args[1])
		if err != nil {
			die("%v", err)
		}
		n := 1
		if len(args) > 2 {
			n = num(2, "N")
		}
		rnd := rng.New(rng.Derive(c.Seed, 4))
		e := &genEnv{}
		for i := 0; i < n; i++ {
			reqs, err := c.Generate(r, rnd, e)
			if err != nil {
				die("gen: %v", err)
			}
			fmt.Println(client.JSON(reqs, r.ClientStreaming))
		}
	case "list":
		for _, r := range c.Plan.RPCs {
			kind := "unary"
			switch {
			case r.ClientStreaming && r.ServerStreaming:
				kind = "bidi-stream"
			case r.ClientStreaming:
				kind = "client-stream"
			case r.ServerStreaming:
				kind = "server-stream"
			}
			fmt.Printf("%-24s %-40s %s weight=%d deadline=%dms retry=%d\n", r.Short, r.Name, kind, r.Weight, r.DeadlineMs, r.Retry.Attempts)
		}
		for _, s := range c.Plan.Scenarios {
			fmt.Printf("%-24s scenario: %s weight=%d\n", s.Name, strings.Join(s.Calls, " -> "), s.Weight)
		}
	default:
		fs.Usage()
		os.Exit(3)
	}
}

// hydo-grpc-gen generates gRPC drivers for a hydo project from its service definitions:
//
//	hydo-grpc-gen <proto dir> [--hints hints.yaml] --out <dir>
//	hydo-grpc-gen --descriptor-set set.pb [--hints ...] --out <dir>
//	hydo-grpc-gen --reflect host:port [--hints ...] --out <dir>
//
// It writes, under <dir>:
//
//	grpc/desc.pb                    the schema (FileDescriptorSet, deterministic bytes)
//	grpc/plan.json                  RPCs + hints, checked against the schema
//	hydo-grpc                       the static guest client (linux/amd64; --arch arm64 adds hydo-grpc.arm64)
//	workload/parallel_driver_<rpc>  one composer command per RPC (+ finally_retry)
//	hydo.grpc.yaml                  a starter section for hydo.yaml
//
// See hydo/onboard/MANIFEST.md, "gRPC services".
package main

import (
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/types/descriptorpb"

	"hydogrpc/internal/load"
	"hydogrpc/internal/spec"
)

type multi []string

func (m *multi) String() string     { return strings.Join(*m, ",") }
func (m *multi) Set(v string) error { *m = append(*m, v); return nil }

func die(f string, a ...interface{}) {
	fmt.Fprintf(os.Stderr, "hydo-grpc-gen: "+f+"\n", a...)
	os.Exit(1)
}

func main() {
	var imports multi
	hints := flag.String("hints", "", "hints file (YAML or JSON)")
	out := flag.String("out", "", "output directory (required)")
	descSet := flag.String("descriptor-set", "", "read a compiled FileDescriptorSet instead of .proto files")
	reflect := flag.String("reflect", "", "ask a running server (gRPC server reflection) for its services")
	target := flag.String("target", "", "server address in the guest (overrides the hints' target)")
	arch := flag.String("arch", "amd64", "client architectures to build: amd64, arm64 or amd64,arm64")
	noBuild := flag.Bool("no-build", false, "do not build the client binary")
	goBin := flag.String("go", envOr("HYDO_GO", "go"), "Go toolchain used to build the client")
	src := flag.String("src", os.Getenv("HYDO_GRPC_SRC"), "source of the client (hydo/grpcgen)")
	rel := flag.String("rel", "", "directory the starter section's paths are relative to (default: the parent of --out)")
	steps := flag.Int("steps", 30, "workload.steps in the starter section")
	flag.Var(&imports, "I", "extra import path for .proto files (repeatable)")
	flag.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: hydo-grpc-gen <proto dir> [--hints hints.yaml] --out <dir>   (or --descriptor-set / --reflect instead of <proto dir>)")
		flag.PrintDefaults()
	}
	// flags may follow the proto dir
	var pos []string
	args := os.Args[1:]
	for len(args) > 0 {
		flag.CommandLine.Parse(args)
		args = flag.Args()
		if len(args) > 0 {
			pos = append(pos, args[0])
			args = args[1:]
		}
	}
	if *out == "" || len(pos) > 1 || (len(pos) == 0) == (*descSet == "" && *reflect == "") {
		flag.Usage()
		os.Exit(2)
	}

	var set *descriptorpb.FileDescriptorSet
	var sources []string
	var err error
	switch {
	case len(pos) == 1:
		var names []string
		set, names, err = load.FromProtoDir(pos[0], imports)
		for _, n := range names {
			sources = append(sources, "proto:"+n)
		}
	case *descSet != "":
		set, err = load.FromDescriptorSet(*descSet)
		sources = []string{"descriptor-set:" + filepath.Base(*descSet)}
	default:
		set, err = load.FromReflection(*reflect, 10*time.Second)
		sources = []string{"reflection:" + *reflect}
	}
	if err != nil {
		die("%v", err)
	}
	files, err := protodesc.NewFiles(set)
	if err != nil {
		die("schema: %v", err)
	}
	h, err := load.ReadHints(*hints)
	if err != nil {
		die("%v", err)
	}
	if *target != "" {
		h.Target = *target
	}
	plan, err := load.Build(files, h, sources)
	if err != nil {
		die("hints: %v", err)
	}

	must := func(err error) {
		if err != nil {
			die("%v", err)
		}
	}
	must(os.MkdirAll(filepath.Join(*out, "grpc"), 0755))
	must(os.MkdirAll(filepath.Join(*out, "workload"), 0755))
	b, err := load.Marshal(set)
	must(err)
	must(os.WriteFile(filepath.Join(*out, "grpc", "desc.pb"), b, 0644))
	pj, err := plan.Marshal()
	must(err)
	must(os.WriteFile(filepath.Join(*out, "grpc", "plan.json"), pj, 0644))

	// composer commands (MANIFEST.md "The test composer"): one parallel driver per RPC with a
	// weight and per scenario, in the hydo/workload/ layout
	old, _ := filepath.Glob(filepath.Join(*out, "workload", "*"))
	for _, f := range old {
		os.Remove(f)
	}
	for _, c := range commands(plan) {
		body := fmt.Sprintf("#!/bin/busybox sh\n# hydo: timeout=%d\n# hydo-msg: %s (hydo-grpc-gen)\nexec /bin/hydo-grpc drive %s\n", c.timeout, c.msg, c.target)
		must(os.WriteFile(filepath.Join(*out, "workload", "parallel_driver_"+c.name), []byte(body), 0755))
	}
	retryCmd := ""
	for _, r := range plan.RPCs {
		if r.Retry.AtEnd {
			retryCmd = "/bin/hydo-grpc retry"
		}
	}

	base := *rel
	if base == "" {
		base = filepath.Dir(filepath.Clean(*out))
	}
	relOut, err := filepath.Rel(base, *out)
	must(err)
	must(os.WriteFile(filepath.Join(*out, "hydo.grpc.yaml"), []byte(starter(plan, relOut, *steps, retryCmd)), 0644))

	if !*noBuild {
		if *src == "" {
			die("--src (or HYDO_GRPC_SRC) must name the client source (hydo/grpcgen), or pass --no-build")
		}
		for _, a := range strings.Split(*arch, ",") {
			name := "hydo-grpc"
			if a != "amd64" {
				name += "." + a
			}
			dst, _ := filepath.Abs(filepath.Join(*out, name))
			cmd := exec.Command(*goBin, "build", "-trimpath", "-buildvcs=false", "-ldflags=-s -w -buildid=", "-o", dst, "./cmd/hydo-grpc")
			cmd.Dir = *src
			cmd.Env = append(os.Environ(), "CGO_ENABLED=0", "GOOS=linux", "GOARCH="+a, "GOFLAGS=-mod=readonly", "GOTOOLCHAIN=local")
			cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
			if err := cmd.Run(); err != nil {
				die("building the client for %s: %v", a, err)
			}
		}
	}
	fmt.Printf("hydo-grpc-gen: %d RPCs, %d scenarios -> %s\n", len(plan.RPCs), len(plan.Scenarios), *out)
	for _, r := range plan.RPCs {
		fmt.Printf("  %-22s %s weight=%d\n", r.Short, r.Name, r.Weight)
	}
}

func envOr(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

type command struct {
	name, target, msg string
	timeout           int
}

// commands: one per RPC with a weight and per scenario. The timeout bounds Drive.Calls
// operations whose every attempt runs into its deadline.
func commands(p *spec.Plan) []command {
	worst := func(r *spec.RPC) int { return (r.DeadlineMs + r.Retry.BackoffMs) * (r.Retry.Attempts + 1) }
	bound := func(ms int) int {
		t := p.Drive.Calls*ms/1000 + 10
		if t > 3600 {
			t = 3600
		}
		return t
	}
	var out []command
	for i := range p.RPCs {
		r := &p.RPCs[i]
		if r.Weight > 0 {
			out = append(out, command{r.Short, r.Name, r.Name + ": generated requests", bound(worst(r))})
		}
	}
	for _, sc := range p.Scenarios {
		ms := 0
		for _, n := range sc.Calls {
			if r, err := p.Find(n); err == nil {
				ms += worst(r)
			}
		}
		out = append(out, command{sc.Name, sc.Name, "scenario " + strings.Join(sc.Calls, " -> "), bound(ms)})
	}
	return out
}

func starter(p *spec.Plan, rel string, steps int, retryCmd string) string {
	var b strings.Builder
	w := func(f string, a ...interface{}) { fmt.Fprintf(&b, f+"\n", a...) }
	w("# Generated by hydo-grpc-gen from %s.", strings.Join(p.Sources, ", "))
	w("# Merge into hydo.yaml; paths are relative to the directory hydo.yaml is in.")
	w("# RPCs (hydo-grpc list):")
	for _, r := range p.RPCs {
		w("#   %-20s %s weight=%d deadline=%dms retry=%d", r.Short, r.Name, r.Weight, r.DeadlineMs, r.Retry.Attempts)
	}
	w("root:")
	w("  files:")
	w("    - {src: %s/hydo-grpc, dst: /bin/hydo-grpc, mode: \"0755\"}", rel)
	w("    - {src: %s/grpc, dst: /etc/hydo/grpc}", rel)
	w("")
	w("env:")
	w("  HYDO_GRPC_SEED: \"%d\"           # requests: a function of this seed and the composer entry (HYDO_SEED) or the step", p.Seed)
	w("")
	w("# The test composer (MANIFEST.md \"The test composer\") composes these commands; the schedule")
	w("# token W= decides which run when, and each entry's HYDO_SEED decides its requests.")
	w("# The same commands are files in %s/workload/ (the hydo/workload/ layout).", rel)
	w("test:")
	w("  commands:")
	for _, c := range commands(p) {
		w("    - {name: %s, kind: parallel_driver, run: [/bin/hydo-grpc, drive, %s], timeout: %d}", c.name, c.target, c.timeout)
	}
	w("")
	w("workload:")
	w("  steps: %d                       # composer rounds; faults are placed at round numbers", steps)
	if retryCmd != "" {
		w("  retry: %s      # unknown outcomes, re-sent with the same request after the faults heal", retryCmd)
	}
	w("")
	// without the composer: numbered steps
	worst := 0
	for _, r := range p.RPCs {
		if r.Weight == 0 {
			continue
		}
		if t := (r.DeadlineMs + r.Retry.BackoffMs) * (r.Retry.Attempts + 1); t > worst {
			worst = t
		}
	}
	timeout := steps*p.Steps.Calls*worst/1000 + 60
	if timeout < 120 {
		timeout = 120
	}
	if timeout > 3600 {
		timeout = 3600
	}
	w("# Without the composer (numbered steps), drop test: and use instead:")
	w("# workload:")
	w("#   steps: %d", steps)
	w("#   run: /bin/hydo-grpc steps $HYDO_FROM $HYDO_TO   # %d call(s) x %d worker(s) per step", p.Steps.Calls, p.Steps.Concurrency)
	if retryCmd != "" {
		w("#   retry: %s", retryCmd)
	}
	w("#   timeout: %d", timeout)
	w("#   process: hydo-grpc")
	w("")
	w("# Invariants stay yours. Call records: /tmp/hydo/grpc/calls (HYDO-HIST lines, history grpc).")
	w("# hydo-grpc call <rpc> '<json>' prints a response and exits 0 ok / 1 failed / 2 unknown,")
	w("# for read-backs in invariants. Outcomes of RPCs with outcome_id: also go to $HYDO_ACKED and $HYDO_OUTCOMES.")
	return b.String()
}

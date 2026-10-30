// hydo-world: the outside world of a hydo project (hvx/onboard/MANIFEST.md, "Fake outside
// services"). One process inside the guest serves every fake HTTP/gRPC service and every fault
// proxy the manifest declares, records each request it receives (what the outside world saw),
// sends webhooks back to the system under test, and applies protocol-level faults (H= tokens)
// that the guest init switches on and off at workload steps.
//
//	hydo-world serve <world.json> [--carry <file>]   run the world (the init starts it)
//	hydo-world ctl <command...>                      talk to it: ping | on <id> <token> | off <id>
//	                                                 | heal | dump | log
//	hydo-world event <name> <ref>                    record an event of the system/workload
//	hydo-world check <kind> <target> [options]       evaluate a side-effect property
//
// Everything is a pure function of the guest's execution: request order, sequence numbers and
// fault decisions (a hash of the fault token and the request's index under it) never depend on
// host time, so under hvx a schedule gives the same records on every run.
// Standard library only, so it builds with the pinned Go toolchain and no module downloads.
package main

import (
	"fmt"
	"os"
)

func usage() {
	fmt.Fprintln(os.Stderr, "usage: hydo-world serve <world.json> [--carry FILE] | ctl <cmd...> | event <name> <ref> | check <kind> <target> [opts]")
	os.Exit(2)
}

func main() {
	if len(os.Args) < 2 {
		usage()
	}
	switch os.Args[1] {
	case "serve":
		os.Exit(serveMain(os.Args[2:]))
	case "ctl":
		os.Exit(ctlMain(os.Args[2:]))
	case "event":
		if len(os.Args) != 4 {
			usage()
		}
		os.Exit(ctlMain([]string{"event", esc(os.Args[2]), esc(os.Args[3])}))
	case "check":
		os.Exit(checkMain(os.Args[2:]))
	default:
		usage()
	}
}

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

// the control socket and the record log; the config can name others, the CLI reads these
func sockPath() string { return env("HYDO_WORLD_SOCK", "/tmp/hydo/world.sock") }
func logPath() string  { return env("HYDO_WORLD_LOG", "/tmp/hydo/world.log") }

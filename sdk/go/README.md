# hydo-assert for Go

[hydo](https://github.com/kakoc/hydo) runs a whole system in a deterministic virtual machine,
injects faults (process kills, network faults, power cuts, disk errors) and replays every
failure bit for bit. This module lets a Go program that runs under hydo say, from the inside,
what must be true and what should happen. hydo collects what the program reports, and an
exploration (many runs with different faults) reports each property as passed, failed (with a
schedule that replays the failure) or never exercised.

The module has two packages: `hydoassert` for properties and fault points, and `hydohist` for
operation histories. It uses only the standard library and needs Go 1.22 or later. Outside hydo
it does almost nothing: the records go to standard error (or nowhere), and fault points stay
off.

## Install

```sh
go get github.com/kakoc/hydo/sdk/go@v0.3.0
```

## A complete program

A small key/value store that declares three properties and checks them:

```go
package main

import "github.com/kakoc/hydo/sdk/go/hydoassert"

var (
	readBack  = hydoassert.Always("kv.read_back", "every acknowledged write reads back with its value")
	overwrite = hydoassert.Sometimes("kv.overwrite", "a write replaced an existing value")
	lostReply = hydoassert.Buggify("kv.lost_reply", "the write lands but its reply is lost")
)

func main() {
	hydoassert.Catalog() // list every property, also the ones that never run
	store := map[string]int{}
	hydoassert.SetupComplete() // the system is ready: hydo may start injecting faults

	for _, w := range []struct {
		key   string
		value int
	}{{"a", 1}, {"b", 2}, {"a", 3}} {
		_, existed := store[w.key]
		store[w.key] = w.value
		overwrite.Check(existed, hydoassert.D{"key": w.key})
		if lostReply.Fire(hydoassert.D{"key": w.key}) {
			continue // the client never hears back (only when hydo turns this point on)
		}
		acked := store[w.key] == w.value
		readBack.Check(acked, hydoassert.D{"key": w.key, "value": w.value})
	}
}
```

Run it with `HYDO_ASSERT_OUT=stderr` to see what it reports. It writes these lines (under hydo
they go to the guest's console, where hydo reads them):

```text
HYDO-ASSERT-DECL {"t":"always","id":"kv.read_back","msg":"every acknowledged write reads back with its value"}
HYDO-ASSERT-DECL {"t":"sometimes","id":"kv.overwrite","msg":"a write replaced an existing value"}
HYDO-ASSERT-DECL {"t":"sometimes","id":"buggify.kv.lost_reply","msg":"fault point fired: the write lands but its reply is lost"}
HYDO-SETUP-COMPLETE
HYDO-ASSERT {"t":"always","id":"kv.read_back","ok":true,"msg":"every acknowledged write reads back with its value","details":{"key":"a","value":1}}
HYDO-ASSERT {"t":"sometimes","id":"kv.overwrite","ok":true,"msg":"a write replaced an existing value","details":{"key":"a"}}
```

A process writes each outcome of a property once: the first pass and the first failure, with
the details of that evaluation. That is why the later passes of `kv.read_back` write nothing.

## How to

### Declare properties and check them

A property has an id (stable across versions of your program) and a message for people. Its
kind says what hydo expects:

| kind | the property holds when | hydo reports a failure when |
|---|---|---|
| `Always` | the condition is true every time, and it is checked at least once | it is false once, or never checked |
| `AlwaysOrUnreachable` | the condition is true every time it is checked | it is false once |
| `Sometimes` | the condition is true at least once in an exploration | it is never true |
| `Reachable` | the code is reached at least once in an exploration | it is never reached |
| `Unreachable` | the code is never reached | it is reached |

Declare properties as package-level variables, so that the catalog lists them before `main`
runs (the snippets below declare them inside a function only to be short):

```go
stored := hydoassert.Always("kv.stored", "every acknowledged write is stored")
parses := hydoassert.AlwaysOrUnreachable("kv.log_parses", "every complete log line parses")
retried := hydoassert.Sometimes("kv.retried", "a client retried a request")
recovered := hydoassert.Reachable("kv.recovered", "the server replayed a non-empty log")
unknown := hydoassert.Unreachable("kv.unknown_command", "the server got a command it does not know")

fmt.Println(stored.Check(true, hydoassert.D{"key": "a"})) // returns the condition
parses.Check(true, nil)                                   // the details may be nil
retried.Check(false, nil)                                 // a false Sometimes writes nothing
recovered.Hit(hydoassert.D{"entries": 3})
command := "GET"
if command != "GET" && command != "SET" {
	unknown.Hit(hydoassert.D{"command": command})
}
```

### Compare two values

`Cmp` checks `left op right` and puts both values and the operator into the details, so a
failure shows them:

```go
logSize := hydoassert.Always("kv.log_size", "the log stays under its cap")
size := int64(12)
fmt.Println(logSize.Cmp(size, "<=", 10000, nil)) // details: {"left":12,"op":"<=","right":10000}
```

The operator is one of `<`, `<=`, `==`, `!=`, `>=` and `>`; the values are `int64`.

### Check that something happens in time (eventually)

An `Eventually` property says that after this point, the condition becomes true within a
deadline. For example: after a restart, the server answers within 5 seconds. Time inside a
hydo guest is deterministic, so the verdict is the same on every replay.

```go
back := hydoassert.Eventually("kv.back", "the server answers within 5 s of its start", 5*time.Second)
started := time.Now()
answers := func() bool { return time.Since(started) >= 20*time.Millisecond } // stands for a real request
fmt.Println(back.Await(answers, 5*time.Millisecond, nil))                    // polls every 5 ms

// Or mark the start and the end yourself:
watch := back.Begin()
// ... the server comes up ...
watch.Done(nil) // or watch.Expire(nil) when you know it will not
```

`Await` blocks the goroutine that calls it.

### Add a fault point (BUGGIFY)

A fault point is a place where your program misbehaves on purpose: it drops a reply, fails a
write, waits too long. Normally the point does nothing (`Fire` returns false). hydo can turn
points on for a run, so that faults happen where only your program can make them. FoundationDB
calls this technique BUGGIFY.

```go
lostReply := hydoassert.Buggify("kv.lost_reply_point", "the write lands but its reply is lost")
slowDisk := hydoassert.Buggify("kv.slow_disk", "a write takes 10 ms longer")

reply := "OK"
if lostReply.Fire(nil) {
	reply = "" // misbehave: the client never gets the reply
}
if slowDisk.Fire(hydoassert.D{"key": "a"}) {
	time.Sleep(10 * time.Millisecond)
}
```

To let hydo turn the points on, add `buggify: true` to the project's `hydo.yaml` and explore
with `hydo explore --buggify`. Each run then gets a seed that turns some points on, and a point
that is on fires on some of its calls. To try it outside hydo, set
`HYDO_BUGGIFY=<seed in hex>,<points on, per mille>,<calls that fire, per mille>`; for example
`HYDO_BUGGIFY=2a,1000,1000` makes every point fire every time.

### Record an operation history

hydo can check a whole history of operations after a run. It checks that every read is
consistent with the writes before it (linearizability per key), and that no list append goes
missing. Your workload's
client records each operation twice: when it starts (`Invoke`) and when the outcome is known.

```go
kv := hydohist.New("kv")
write := kv.Invoke(3, "write", "k1", hydohist.Enc("v1")) // client 3, before the request
// ... send the request, then record the outcome:
write.Ok() // it happened; Fail(): it did not; Info(): the outcome is unknown
read := kv.Invoke(3, "read", "k1")
read.Ok("k1", hydohist.ValOrNil("v1", true))
```

Then name the history in the project's `hydo.yaml`:

```yaml
checks:
  - id: kv.linearizable
    type: linearizable
    model: register
    history: kv
    msg: every read sees the latest acknowledged write
```

`hydo guide checks` describes the checks and their options.

### Choose where the records go

Under hydo the records go to the guest's console (`/dev/ttyS0` for properties, `/dev/console`
for histories), which hydo reads even when the program's standard output goes to a log file.
Outside hydo, where the console cannot be opened, they go to standard error. Two variables
change this, and each is read once, at the first record:

- `HYDO_ASSERT_OUT`: a path of an existing file, `stderr`, or `off` for the properties;
- `HYDO_HIST_OUT`: a path (created if missing), `stderr`, or `off` for the histories.

### Build a program for a hydo guest

A hydo guest runs static Linux programs:

```sh
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -o server .
```

Use `GOARCH=arm64` for hydo on a Mac. A complete project is the stack example, an order
service on MySQL, Kafka and Redis:
<https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/stack>.

## Reference

- The API and its examples: <https://pkg.go.dev/github.com/kakoc/hydo/sdk/go/hydoassert> and
  <https://pkg.go.dev/github.com/kakoc/hydo/sdk/go/hydohist>.
- The records, and how hydo judges each property type:
  <https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md>.
- The history format and the checkers:
  <https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md>.
- The same SDK for Rust, Python and JavaScript writes the same records, byte for byte:
  <https://github.com/kakoc/hydo/tree/main/sdk>.

Rules of this module:

- The catalog (`Catalog()`) lists the properties in the order they were declared. Call it once,
  early, in the process whose console hydo reads.
- Details are encoded by `encoding/json`, which writes map keys in sorted order. A record whose
  details cannot be encoded (a NaN, an infinity) is not written.
- Every call is safe from several goroutines: one lock, held only while a record is written.
- The packages are named `hydoassert` and `hydohist` because a Go package name cannot contain a
  hyphen.
- A history's process is an int; its arguments are tokens (encode free-form values with `Enc`).
  A process has at most one operation in flight: give each goroutine that acts as a client its
  own process number.

## License

Apache-2.0.

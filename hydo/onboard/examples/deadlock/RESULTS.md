# A planted deadlock: results

This example is the proof of `hydo inspect stacks` (Milestone 383,
`docs/design/inspect-stacks.md`). It plays a service hang where the logs stop. There are two services: one in Go (`go/main.go`)
and one in Rust (`rust/src/main.rs`). Each runs a worker and a watchdog. The worker takes the
accounts' lock, works for 25 ms, and then takes the journal's lock to write the transfer down.
The watchdog wakes every 10 ms. A gap of more than 200 ms since its last wake means that the
process was paused. Then it checks the journal against the accounts, and it takes the two locks
in the other order. So a pause while the worker holds the accounts' lock leaves the two waiting
on each other for good. The service stops writing its log and its count, and nothing prints an
error.

## The runs

On hvx on the bare-metal machine, with this tree's `hydo` and `hydo-hvx` over the 0.6.0 Linux
release's guest files, and the guest kernel's `System.map` beside its `bzImage`. The image is
`712ae9a3…`, built at the fingerprint epoch 6. Every schedule ran twice and gave one fingerprint.

| Schedule | Verdict | Fingerprint |
|---|---|---|
| `disk=none \| run` | OK, both services count on | `79ada02759c5cdbb` |
| `disk=none \| run P=bank_go@3+2` | VIOLATION `[bank_go.progress]`, its count stays at 93 | `c81b3f5264c80d56` |
| `disk=none \| run P=bank_rs@3+2` | VIOLATION `[bank_rs.progress]`, its count stays at 61 | `c315abec6fffde94` |

The pause of the Go service starts at the stop `run:186169` and ends at `run:186451`; the
verdict is at `run:187776`. The Rust service's pause runs from `run:186169` to `run:186717`, and
its verdict is at `run:189357`.

## The stacks at the end

`hydo inspect stacks runs/go --at end --text` replays the run to the stop of the verdict (about
20 to 60 s on the loaded bare-metal machine) and reads the stop. Its first lines:

```text
7 threads, 7 goroutines, 3 locks waited on (hvx x86_64 guest, Linux 6.6.35)

A cycle of waits (a deadlock):
  goroutine 6 (pid 78) waits on main.journalMu (0x597280), held by goroutine 7 (pid 78)
  goroutine 7 (pid 78) waits on main.accountsMu (0x597278), held by goroutine 6 (pid 78)

lock main.accountsMu (go-mutex, pid 78): waited on by goroutine 7 (pid 78)
  likely held by goroutine 6 (pid 78) in main.transfer (main.go:35): its frame main.transfer (main.go:35) refers to main.accountsMu and has not returned

lock main.journalMu (go-mutex, pid 78): waited on by goroutine 6 (pid 78)
  likely held by goroutine 7 (pid 78) in main.checkJournal (mutex.go:70): its frame main.checkJournal (mutex.go:70) refers to main.journalMu and has not returned
```

The two goroutines, unwound by Go's table of frame sizes:

```text
goroutine 6 (pid 78) waiting [sync.Mutex.Lock], waits on 0x597280
      runtime.gopark  runtime/proc.go:475  [regs]
      runtime.semacquire1  runtime/proc.go:480  [go]
      internal/sync.runtime_SemacquireMutex  runtime/sema.go:95  [go]
      internal/sync.(*Mutex).lockSlow  internal/sync/mutex.go:149  [go]
      main.appendJournal  internal/sync/mutex.go:70  [go]
      main.transfer  bank/main.go:35  [go]
      main.worker  bank/main.go:65  [go]
      runtime.goexit  runtime/asm_amd64.s:1264  [go]

goroutine 7 (pid 78) waiting [sync.Mutex.Lock], waits on 0x597278
      runtime.gopark  runtime/proc.go:475  [regs]
      runtime.semacquire1  runtime/proc.go:480  [go]
      internal/sync.runtime_SemacquireMutex  runtime/sema.go:95  [go]
      internal/sync.(*Mutex).lockSlow  internal/sync/mutex.go:149  [go]
      main.checkJournal  internal/sync/mutex.go:70  [go]
      main.watchdog  bank/main.go:75  [go]
      runtime.goexit  runtime/asm_amd64.s:1264  [go]
```

The Go program's three threads are the runtime's: one in `epoll_pwait` (the network poller) and
two idle ones in `futex`, each with its kernel stack (`futex_wait` under `__x64_sys_futex`). The
answer says that their futexes are the runtime's own and that the goroutines hold the locks.

`hydo inspect stacks runs/rs --at end --text` gives the Rust service's deadlock, two threads in
`futex` on two `std::sync::Mutex` statics:

```text
A cycle of waits (a deadlock):
  tid 85 (worker) waits on bank::JOURNAL (0x272c28), held by tid 86 (watchdog)
  tid 86 (watchdog) waits on bank::ACCOUNTS (0x273808), held by tid 85 (worker)

lock bank::JOURNAL (futex, pid 83): waited on by tid 85 (worker)
  likely held by tid 86 (watchdog) in bank::check_journal (futex.rs:33): its frame bank::check_journal (futex.rs:33) refers to bank::JOURNAL and has not returned

lock bank::ACCOUNTS (futex, pid 83): waited on by tid 86 (watchdog)
  likely held by tid 85 (worker) in bank::transfer (main.rs:23): its frame bank::transfer (main.rs:23) refers to bank::ACCOUNTS and has not returned

pid 83 tid 85 (worker) sleeping in futex(0x272c28, 0x89, 0x2), waits on 0x272c28
    program /bin/bank-rs
    kernel:
      __schedule+0x3ee  0xffffffff81bee1de  [orc]
      schedule+0x5e  0xffffffff81bef26e  [orc]
      futex_wait_queue+0x63  0xffffffff811b3af3  [orc]
      futex_wait+0x189  0xffffffff811b4229  [orc]
      do_futex+0xc6  0xffffffff811b0806  [orc]
      __x64_sys_futex+0x129  0xffffffff811b0a09  [orc]
      do_syscall_64+0x39  0xffffffff81bd9279  [orc]
      entry_SYSCALL_64_after_hwframe+0x78  0xffffffff81c00134  [orc]
    user:
      syscall  ../src_musl/arch/x86_64/syscall_arch.h:59  [regs]
      bank::append_journal  .../library/std/src/sys/sync/mutex/futex.rs:33  [fp]
      bank::transfer  src/main.rs:23  [cfi]
      std::sys::backtrace::__rust_begin_short_backtrace  src/main.rs:50  [cfi]
      core::ops::function::FnOnce::call_once{{vtable.shim}}  .../library/std/src/thread/mod.rs:559  [cfi]
      std::sys::pal::unix::thread::Thread::new::thread_start  library/alloc/src/boxed.rs:1971  [cfi]
      start  ../src_musl/src/thread/pthread_create.c:203  [cfi]
```

A frame's line is the innermost inlined function's: `main.appendJournal` shows the line of the
`Lock` that Go inlined into it, and `bank::check_journal` the line of the standard library's lock.
The Rust process's main thread waits in `futex` too, on its first thread's end (`join`): the
answer lists that address as a lock without a name and without a holder.

## Before the hang, and the hang forming

`hydo inspect stacks runs/go --at run:186100 --text` reads a stop 69 stops before the pause. No
lock is waited on and there is no cycle. Both goroutines sleep in `time.Sleep`: the worker
between two transfers (`main.go:66`), the watchdog between two wakes (`main.go:73`). The Rust
service's `worker` and `watchdog` sleep in `nanosleep` the same way.

A session at the stop right after the pause ends shows the deadlock forming:

```sh
hydo inspect start runs/go --at run:186452
hydo inspect stacks <session> --pid 78 --follow --steps 4 --ms 20 --text
```

At step 0 the three threads of the Go service are still `stopped` (the pause's signal), and the
worker sleeps inside `main.transfer` (`main.go:33`), so it holds `main.accountsMu`. After 20 ms
of guest time the watchdog has woken, seen the gap, taken `main.journalMu` and waits on
`main.accountsMu`; the worker waits on `main.journalMu`. The answer shows the cycle. Steps 2 and
3 show the same cycle: nothing moves any more.

## Reading the stops moves nothing

`hydo inspect stacks <run dir> --check-run --every 2000` replays the recorded run with the
engine's `DUMP` at one stop in every 2,000 and compares the replay's fingerprint with the
recorded one:

| Run | Dumps | Stops | Fingerprint with the dumps | Recorded | Wall time |
|---|---|---|---|---|---|
| `runs/go` | 93 | 187,778 | `c81b3f5264c80d56` (wfp `909010bad62a6700`) | `c81b3f5264c80d56` | 198 s |
| `runs/rs` | 94 | 189,359 | `c315abec6fffde94` (wfp `f6565f1b215efe06`) | `c315abec6fffde94` | 266 s |

Each dump holds the guest's 512 MiB of RAM, a page of zeros as a hole of the file.

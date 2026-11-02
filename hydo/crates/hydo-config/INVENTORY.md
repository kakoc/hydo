# hydo's environment variables

Generated from `hydo/crates/hydo-config/src/registry.rs` by `hydo_config::registry::inventory_markdown` (M144); `cargo test -p hydo-config` fails when this file differs, and `HYDO_BLESS=1` rewrites it. How to add an option: `hydo/crates/hydo-config/src/lib.rs`.

41 user-facing and 75 developer-only variables are read from hydo's environment, only by `hydo_config::Env::process` at a program's start; 31 test switches are read by test code; 24 names are the guest interface, 4 are variables of an image's manifest.sh and 0 are set for programs hydo starts. 8 variables were removed (M144, M145). Shell scripts (`hv/`, `hydo/dist/`, build scripts) and the C programs have their own variables, not listed here.

## Engine (`hydo_config::engine::Engine`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_ENGINE` | user | word: inproc \| process \| c | auto | hydo-engine | Where the hvx engine runs: `inproc` on a thread of this process, `process` (or `c`) as the program `hydo-hvx`; anything else: in this process exactly when this is Linux, `HYDO_SUDO` is not `1` and this process can read and write the device. |
| `HYDO_SUDO` | user | flag: exactly `1` | - | hydo-engine, hydo-cli, libexec/hvx-run | `1`: the engine program runs under sudo even when the user can open the device (the release's `libexec/hvx-run` reads it too; `hydo doctor` reports it). |
| `HVX_DEV` | user | path | /dev/hvx | hydo-engine, hydo-hvx, hydo-cli, libexec/hvx-run | The hvx device (another name: a side module). An empty value means the default. |
| `HYDO_LOCK` | user | path | ~/.hvx-module.lock | hydo-engine, hydo-cli, libexec/hvx-run | The module lock file every run holds shared (a reload holds it exclusively). An empty value means the default (in `$SUDO_USER`'s home under sudo). |
| `HYDO_HVX_EXPLORE` | user | path | <root>/target/release/hydo-hvx, else <root>/bin/hydo-hvx | hydo-engine | The engine program started when the engine does not run in this process (the name is the C engine's, deleted in M137). |

## The hvx engine (`hydo_config::hvx::Options`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_HVX_HOSTCALL` | user | flag: not `0` | on | hydo-hvx | `0` turns the host-call port off (M138; on by default since M140). |
| `HYDO_HVX_REUSE` | user | flag: set and not `0` | off | hydo-hvx | `1` (not `0`): keep a used VM and reset it in place for the next run (M138). |
| `HYDO_HVX_CHECK_RESET` | developer | flag: set and not `0` | off | hydo-hvx | Not `0`: after every in-place reset, compare RAM and disk with a fresh VM's. |
| `HYDO_HVX_CHECK_ZERO` | developer | flag: set and not `0` | off | hydo-hvx | Not `0`: check that a fresh VM's RAM and disk are zero before a restore. |
| `HYDO_HVX_PREPARE` | developer | flag: not `0` | on | hydo-hvx | `0`: make every fresh VM when it is needed (default: the next one on a helper thread). |
| `HYDO_HVX_REAPER` | developer | flag: not `0` | on | hydo-hvx | `0`: tear old VMs down in line (default: on a helper thread). |
| `HVX_EXPLORE_WALL_SECS` | developer | number (C atof) | 900 | hydo-hvx | A run's wall-clock budget in seconds (C `atof`; not positive: the default). |
| `HVX_VCPU_PARK` | developer | flag: set and not `0` | off | hydo-hvx | Not `0`: park idle vCPUs (several vCPUs). |
| `HVX_CPU_V3` | developer | flag: set and not `0` | off | hydo-hvx | Not `0`: the guest CPU is x86-64-v3 (as `--cpu v3`). |
| `HVX_PMU_CLOCK` | developer | flag: not `0` | on | hydo-hvx | `0` turns the PMU branch clock off. |
| `HVX_PAUSE_EXIT` | developer | flag: not `0` | on | hydo-hvx | `0` turns PAUSE exits off. |
| `HVX_PT_TRACE_VM` | developer | flag: set (any value) | off | hydo-hvx | Set: trace the VM with Processor Trace (HVX_VM_PT_TRACE). |
| `HVX_PAUSE_K` | developer | integer (C strtoull) | the module's | hydo-hvx | The PAUSE-exit clock step (C strtoull). |
| `HVX_TURN_MARGIN` | developer | integer (C strtoull) | the module's | hydo-hvx, hydo-explore | The vCPU turn-end margin (M85; `--turn-margin` of the explorer sets it). |
| `HVX_QUANTUM` | developer | integer (C strtoull) | the schedule's | hydo-hvx | The vCPU turn length in branches (several vCPUs). |
| `HVX_BURST` | developer | integer (C strtoull) | off | hydo-hvx | Burst turns (M85). |
| `HVX_QUANTUM_AFTER` | developer | flag: set (any value) | off | hydo-hvx | Set: apply `HVX_QUANTUM` after the boot only. |
| `HVX_EXPLORE_CMDLINE_EXTRA` | developer | text | - | hydo-hvx | Extra words on the guest kernel's command line. |
| `HVX_EXPLORE_NO_UNSYNC` | developer | flag: set (any value) | off | hydo-hvx | Set: keep the disk's volatile write cache out of snapshots (no HVX_GET/SET_UNSYNC). |
| `HVX_EXPLORE_RAM_MIB` | developer | integer | 1024 | hydo-hvx | The guest RAM in MiB when `--ram-mib` is not given (256-3072). |
| `HVX_EXPLORE_VCPUS` | developer | integer | 1 | hydo-hvx | The vCPU count when `--vcpus` is not given. |

## The hvx engine's diagnostics (`hydo_config::hvx::Options`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HVX_EXPLORE_PHASES` | developer | flag: set and not `0` | off | hydo-hvx | Not `0`: print the run's phases with their times (also on with `HVX_EXPLORE_DIRTY`). |
| `HVX_EXPLORE_DIRTY` | developer | flag: set and not `0` | off | hydo-hvx | Not `0` (with PHASES): count the RAM and disk pages that differ from S1. |
| `HVX_EXPLORE_ZSKIP` | developer | flag: set (any value) | off | hydo-hvx | Set (with PHASES): hash RAM both ways at the workload's end and compare. |
| `HVX_EXPLORE_RETLOG` | developer | flag: set (any value) | off | hydo-hvx | Set: one `RET` line per run call on stderr. |
| `HVX_FTSTAT` | developer | flag: set (any value) | off | hydo-hvx | Set: print the fast-turn statistics. |
| `HVX_EXPLORE_DUMP` | developer | path | - | hydo-hvx | A directory: dump the final state (RAM, disk) there. |
| `HVX_EXPLORE_DUMP_HASHONLY` | developer | flag: set (any value) | off | hydo-hvx | Set (with DUMP): write only the hashes. |
| `HVX_EXPLORE_DURABLE_OUT` | developer | path | - | hydo-hvx | A file: write the durable disk after a run there. |
| `HVX_EXPLORE_BOOTCON` | developer | flag: set (any value) | off | hydo-hvx | Set: keep the boot's console. |
| `HVX_CONSOLE_OUT` | developer | path | - | hydo-hvx | A file: copy the console there. |
| `HVX_DRAIN_NO_RETRY` | developer | flag: set (any value) | off | hydo-hvx | Set: do not retry a netem drain. |
| `HVX_EXPLORE_VCPUSTAT` | developer | flag: set (any value) | off | hydo-hvx | Set: print per-vCPU statistics. |
| `HVX_EXPLORE_PT_ELF` | developer | path | - | hydo-hvx | Coverage: trace this program's text (needs the `pt` build). |
| `HVX_EXPLORE_PT_RANGES` | developer | text: ranges | - | hydo-hvx | Coverage: trace up to two IP ranges instead. |
| `HVX_EXPLORE_PTMULTI` | developer | path | - | hydo-hvx | Coverage: the external decoder for PT_RANGES traces. |
| `HVX_EXPLORE_PT_MIB` | developer | integer | 256 | hydo-hvx | Coverage: the trace buffer in MiB. |
| `HVX_EXPLORE_PT_SAVE` | developer | flag: set (any value) | off | hydo-hvx | Set (coverage): keep the raw trace as `<tag>.pt`. |
| `HVX_SIM_DRAIN_DROP` | developer | text; empty = unset | - | hydo-hvx | A console marker: fail the first console read that shows it, once (a test of the drain). |

## Paths (`hydo_config::run::Paths`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_ROOT` | developer | path | the first parent of the program with hydo/ and hv/, else the source tree | hydo-explore, hydo-bench | The folder with `hydo/` and `hv/` for the programs `hydo-explore` and `hydo-bench`. An empty value means the default. |
| `HYDO_BUILD` | user | path | ~/hydo/build | hydo-explore, hydo-bench | The images, `bzImage` and `initramfs-kafka.cpio.gz` (the `hydo` command sets the release's `guest/`). An empty value means the default. |
| `HYDO_HVX_RUN` | user | path | ~/bin/hvx-run | hydo-explore, hydo-bench | The shared-lock wrapper the engine program is started through (the `hydo` command sets the release's `libexec/hvx-run`). An empty value means the default. |

## The explorer's pool (`hydo_config::run::Pool`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_QUIET_FILE` | user | path | - | hydo-explore, hydo-bench | While this file exists, the explorer starts no new VM (waits, checking every 30 s, at most 2 hours): a quiet window on a shared machine. |
| `HYDO_EXPLORE_CACHE` | user | path | - | hydo-explore, hydo-bench | A result cache: a deterministic exploration resumes from the results recorded there. |
| `HYDO_SCRIPTED_ENGINE` | developer | path | - | hydo-explore | The explorer's tests only (M145): a scripted engine program that answers every schedule as a pure function of its text, instead of the host controller over a real engine (`hydo_explore::scripted`). |

## The Mac engine (`hydo_config::hvm::Engine`, `hydo_config::hvm::Options`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_HVM` | user | word: c \| inproc | the program hydo-hvm | hydo-engine, hydo-cli | Which Mac engine runs: `c` the C hvmx (one milestone after M143), `inproc` the Rust engine in this process (one VM per process; the process needs the hypervisor entitlement); else the Rust program `hydo-hvm`. |
| `HYDO_HVM_ENGINE` | user | path | <root>/target/release/hydo-hvm, else <root>/bin/hydo-hvm, else hv/hvm/explore/hvmx | hydo-engine | The Mac engine program (an empty value means the default). |
| `HVM_DISK` | user | path | - | hydo-hvm, hvm, hvmx (set by hydo-engine) | The data disk (virtio-blk); hydo-engine sets it for an image with a `-disk.img`. |
| `HVM_DISK_OUT` | developer | path | - | hydo-hvm hvm, hvm | hvm writes the durable disk image here at its end (flushed bytes only). |
| `HVM_MEM_MB` | user | number | 1024 (hvm: 256) | hydo-hvm, hvm, hvmx | Guest RAM in MiB (hvmx 256..4096, default 1024; hvm default 256); `--ram-mib` wins. |
| `HVM_STEP` | developer | number | 64 | hydo-hvm, hvm, hvmx | Virtual ticks of the 24 MHz counter per VM exit. |
| `HVM_MAX_EXITS` | developer | number | 200000000 | hydo-hvm hvm, hvm | hvm's exit budget. |
| `HVM_RUN_EXITS` | developer | number | 50000000 | hydo-hvm, hvmx | hvmx's exit budget per phase of a run. |
| `HVM_ECHO` | developer | flag: set | - | hydo-hvm, hvm, hvmx | Set: copy the console to stdout as it is written. |
| `HVM_TRACE` | developer | path | - | hydo-hvm, hvm, hvmx | One line per VM exit into this file (and the interrupted PC at each acknowledge). |
| `HVM_TRACE_WATCH` | developer | number | - | hydo-hvm, hvm, hvmx | With HVM_TRACE: log the guest u64 at this RAM offset at every exit. |
| `HVM_TRACE_SYSREG` | developer | flag: set | - | hydo-hvm, hvm, hvmx | Set: log unknown system-register traps. |
| `HVM_BOOTARGS` | developer | text | console=ttyS0 ... random.trust_bootloader=on | hydo-hvm, hvm, hvmx | The guest kernel's command line (hvmx appends `hydo_phase=serve` or `quiet loglevel=1`). |
| `HVM_SPIN_SECS` | developer | number | 10 | hydo-hvm, hvm, hvmx | Seconds of exit-free host time after which a spin with no provable loop ends the run. |
| `HVM_SPIN_MS` | developer | number | 5 | hydo-hvm, hvm, hvmx | Milliseconds of exit-free host time before the spin observer looks (a hint only). |
| `HVM_SPIN_OBSERVE` | developer | flag: not `0` | on | hydo-hvm, hvm, hvmx | `0`: no spin observer (the bare watchdog). |
| `HVM_SPLOG` | developer | path | - | hydo-hvm, hvm, hvmx | The spin observer's log file. |
| `HVM_PCT` | developer | text | - | hydo-hvm hvm, hvm | hvm: forced preemption points for the whole run (`<hex seed>,<points>,<first>,<last>`). |
| `HVM_TJ` | developer | text | - | hydo-hvm hvm, hvm | hvm: tick jitter for the whole run (`<hex seed>,<permille>,<max>[,<lo>,<hi>]`). |
| `HVM_SOFT_IRQ` | developer | number | 1 (hvm: 0) | hydo-hvm, hvm, hvmx | Deliver IRQs by emulating the exception entry (`0`: the vCPU's IRQ line). |
| `HVM_DUMP` | developer | path | - | hydo-hvm hvm, hvm | hvm: write `<prefix>.regs` and `<prefix>.ram` at the stop. |
| `HVM_FAULT` | developer | number | - | hydo-hvm, hvm, hvmx | The disk's fault seed (hvmx: the flush-aware class at probability 0 when unset). |
| `HVM_FAULT_CLASSES` | developer | list | error | hydo-hvm, hvm, hvmx | The disk's fault classes (`delay,error,drop,reorder,torn,unflushed` or a number). |
| `HVM_FAULT_PROB` | developer | number | 100 | hydo-hvm, hvm, hvmx | The disk faults' probability in percent. |
| `HVM_FAULT_MAXDELAY` | developer | number | 8 | hydo-hvm, hvm, hvmx | The longest disk delay in exits. |
| `HVM_NET` | developer | flag: set | - | hydo-hvm, hvm, hvmx | Set: attach the virtio-net device. |
| `HVM_NET_RX` | developer | path | - | hydo-hvm, hvm, hvmx | Scripted inbound frames (`[u16 big-endian length][frame]...`). |
| `HVM_NET_RX_PACE` | developer | number | 4800000 | hydo-hvm, hvm, hvmx | Virtual ticks between two delivered inbound frames. |
| `HVM_NET_TX_OUT` | developer | path | - | hydo-hvm hvm, hvm | Captured outbound frames are written here at the end (hvm). |
| `HVM_NET_FAULT` | developer | number | - | hydo-hvm, hvm, hvmx | The network fault seed. |
| `HVM_NET_FAULT_CLASSES` | developer | list | drop | hydo-hvm, hvm, hvmx | The network fault classes (`drop,corrupt,dup` or a number). |
| `HVM_NET_FAULT_PROB` | developer | number | 100 | hydo-hvm, hvm, hvmx | The network faults' probability in percent. |
| `HVM_SNAP_DIR` | user | path | /tmp/hvm-snap | hydo-hvm, hvmx | Where the engine makes (and unlinks at once) its snapshot files. |
| `HVMX_DIAG` | developer | flag: set | - | hydo-hvm, hvmx | Set: per-component hashes of a run's end state into `<outdir>/<tag>.diag`. |
| `HVMX_DIAG_CHUNKS` | developer | list | - | hydo-hvm, hvmx | With HVMX_DIAG: raw copies of these 1 MiB chunks of RAM (comma-separated offsets). |
| `HVMX_CHECK_RESTORE` | developer | flag: set | - | hydo-hvm, hvmx | Set: read the registers back after every restore and report differences. |
| `HVMX_NO_MLOCK` | developer | flag: set | - | hydo-hvm, hvmx | Set: do not mlock guest RAM. |

## The host controller (`hydo_config::run::Control`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HVM_SPIN_REWIND` | user | flag: exactly `0` turns it off | on | hydo-control, hydo-hvm, hvmx | `0`: the host does not run a SPIN of an agent image on hvm again in the rewind mode (hvmx reads it for a shell image's RUN). |

## The `hydo` command (`hydo_config::run::Cli`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_CLI_PLATFORM` | developer | word: linux \| mac | this binary's platform | hydo-cli | `linux` or `mac`: run the `hydo` command as the other platform's (the CLI test checks the other platform's commands that need no engine). |

## Image builds (`hydo_config::build::Build`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_VERSION_CHECK` | user | word: warn | refuse | hydo-build (through hydo-manifest) | `warn`: a plan whose `hydo_version:` this hydo does not satisfy builds with a warning. |
| `HYDO_ONBOARD` | developer | path | the release's, else the source tree's | hydo-build | The product's `onboard/` directory (MANIFEST.md, ../VERSION). |
| `HYDO_BUSYBOX` | user | path; empty = default | the pinned /nix/store path | hydo-build | The x86_64 busybox (the `hydo` command sets the release's); on arm64, the host busybox that lists the applets. |
| `HYDO_BUSYBOX_ARM64` | user | path; empty = default | the pinned /nix/store path | hydo-build | The arm64 busybox. |
| `HYDO_SEEDRNG` | user | path; empty = default | <build dir>/hvx_seedrng | hydo-build | The x86_64 `hvx_seedrng`. |
| `HYDO_SEEDRNG_ARM64` | user | path; empty = unset | not staged | hydo-build | The arm64 `hvx_seedrng` (staged only when set). |
| `HYDO_MKE2FS` | user | path; empty = default | the pinned /nix/store path | hydo-build | The `mke2fs` that writes the images. |
| `HYDO_FAKETIME` | user | path; empty = default | the pinned /nix/store path | hydo-build, libexec/faketime | The `faketime` that runs `mke2fs` (the release's `libexec/faketime` reads it too). |
| `HYDO_IPROUTE2` | user | path; empty = default | the pinned /nix/store path | hydo-build | The iproute2 closure for network faults (x86_64). |
| `HYDO_IPTABLES` | user | path; empty = default | the pinned /nix/store path | hydo-build | The iptables closure for the freeze (x86_64). |
| `HYDO_MODROOT` | user | path; empty = default | the pinned /nix/store path | hydo-build | The guest kernel modules (x86_64). |
| `HYDO_FCBIN` | user | path; empty = default | <build dir>/hydo_fc | hydo-build | The x86_64 `hydo_fc` (the freeze helper). |
| `HYDO_FCBIN_ARM64` | user | path; empty = default | <build dir>/hydo_fc-arm64 | hydo-build | The arm64 `hydo_fc`. |
| `HYDO_WORLDBIN` | user | path; empty = default | <build dir>/hydo-world | hydo-build | The x86_64 `hydo-world`. |
| `HYDO_WORLDBIN_ARM64` | user | path; empty = default | <build dir>/hydo-world-arm64 | hydo-build | The arm64 `hydo-world`. |
| `HYDO_READ_BIN` | user | path; empty = default | <build dir>/hydo-read | hydo-build | The x86_64 `hydo-read`. |
| `HYDO_READ_BIN_ARM64` | user | path; empty = default | <build dir>/hydo-read-arm64 | hydo-build | The arm64 `hydo-read`. |
| `HYDO_AGENT_BIN` | user | path; empty = default | <build dir>/hydo-agent | hydo-build | The x86_64 `hydo-agent` (its sha256 must be the pinned one). |
| `HYDO_AGENT_BIN_ARM64` | user | path; empty = default | <build dir>/hydo-agent-arm64 | hydo-build | The arm64 `hydo-agent`. |
| `HYDO_AGENT_UNPINNED` | developer | flag: exactly `1` | off | hydo-build | `1`: stage an agent whose sha256 is not the pinned one (with a warning). |
| `HYDO_ARM64_STORE` | user | path; empty = the host's /nix/store | - | hydo-build | A directory holding `nix/store/...` for arm64 store files (the Mac release's `guest/tc`). |
| `HYDO_TC_ARM64` | user | path (empty is a value) | the pinned /nix/store path | hydo-build | The arm64 `tc`; empty: an image without network faults. |
| `HYDO_DEBUGFS` | user | path; empty = default | next to mke2fs | hydo-build | The `debugfs` that sets the image's owners and modes. |
| `HYDO_KEEP_OWNERS` | user | flag: set and not empty | off | hydo-build | Set (not empty): keep the build user's file owners in a Linux release build. |

## Helper programs (`hydo_config::tools::Tools`)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_NIX_CACHE` | developer | URL | https://cache.nixos.org | hydo-nix-fetch | The binary cache `hydo-nix-fetch` reads. |
| `HYDO_GRPC_ARCH` | user | word: amd64 \| arm64 \| amd64,arm64 | amd64 | hydo-grpc-gen | The default of `hydo-grpc-gen -arch`. An empty value means the default. |
| `HYDO_GRPC_CLIENT` | user | path | - | hydo-grpc-gen | The default of `hydo-grpc-gen -client` (the prebuilt clients). |

## Test switches (read by test code only)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_BLESS` | test | flag | off | many crates' tests | `1`: rewrite a test's recorded answers (goldens) from this run's output. |
| `HYDO_RELEASE_SCHEMA` | test | flag | off | hydo-results, hydo-agent-proto tests | `1`: copy the current schema to the released one (only when a release ships). |
| `HYDO_COMPOSE_KEEP` | test | path | - | hydo-compose tests | Keep the Compose test's scratch files here. |
| `HYDO_CONTROL_DUMP` | test | path | - | hydo-control tests | Dump the controller tests' transcripts here. |
| `HYDO_CONTROL_FULL` | test | flag: set | off | hydo-control tests | Set: the controller tests' full seed ranges. |
| `HYDO_CORPUS_QUICK` | test | flag: set | off | hydo-explore tests | Set: the explorer's identity corpus, quick part only. |
| `HYDO_IDENT_FULL` | test | flag: set | off | hydo-explore tests | Set: the explorer's identity test in full. |
| `HYDO_GOLDEN_FROM` | test | flag: set | off | hydo-explore tests | Set: write comments naming the recorder into the corpus goldens. |
| `HYDO_GOLDEN_DUMP` | test | path | - | hydo-composer, hydo-errors tests | Dump a golden comparison's inputs here. |
| `HYDO_ERRLOG_CORPUS` | test | path | - | hydo-errors tests | The saved error-log corpus to compare. |
| `HYDO_FAKE_ENGINE` | test | flag | - | hydo-explore tests | `1`: the scripted engine program (the test binary run as the engine). |
| `HYDO_FAKE_ENGINE_BIN` | test | path | the test binary | hydo-explore tests | Another scripted engine program. |
| `HYDO_FAKE_EXPLORER` | test | command | hydo-explore | hydo-explore tests | Another explorer to run on the scripted engine. |
| `HYDO_FAKE_HIST` | test | flag: not empty | - | hydo-explore tests | Not empty: the scripted engine prints history records. |
| `HYDO_HIST_DUMP` | test | path | - | hydo-history tests | The history checker's dump directory. |
| `HYDO_HIST_FULL` | test | flag: set | off | hydo-history tests | Set: the history checker's full corpus. |
| `HYDO_HVX_C_SRC` | test | path | - | hydo-hvx tests | The deleted C engine's source, to re-record the parser's answers. |
| `HYDO_RECORD_PY` | test | path | - | hydo-composer, hydo-errors tests | The Python used to re-record the old answers (before M107). |
| `HYDO_RECORD_PYTHON` | test | flag: set | - | hydo-cli tests | Set: re-record the CLI's answers with the old Python CLIs. |
| `HYDO_RECORD_PY_CORPUS` | test | path | - | hydo-errors tests | The corpus for re-recording with Python. |
| `HYDO_RECORD_WORK` | test | path | - | hydo-composer, hydo-errors tests | The work directory for re-recording with Python. |
| `HYDO_SHOW` | test | flag: set | - | hydo-control tests | Set: print the fakes test's transcripts. |
| `HYDO_SKILLS_REF` | test | path | on PATH | hydo-agents tests | The skills-ref program for the skill check. |
| `HYDO_WORLD_BIN` | test | path | built | hydo-world tests | The hydo-world program for the cross-check. |
| `HYDO_XCHECK_PORT` | test | integer | - | hydo-world tests | The cross-check's first port. |
| `HYDO_TEST_ETCD` | test | address | skipped | hydo-read tests | A live etcd for hydo-read's store tests. |
| `HYDO_TEST_KAFKA` | test | address | skipped | hydo-read tests | A live Kafka for hydo-read's store tests. |
| `HYDO_TEST_MYSQL` | test | address | skipped | hydo-read tests | A live MySQL for hydo-read's store tests. |
| `HYDO_TEST_POSTGRES` | test | address | skipped | hydo-read tests | A live PostgreSQL for hydo-read's store tests. |
| `HYDO_TEST_REDIS` | test | address | skipped | hydo-read tests | A live Redis for hydo-read's store tests. |
| `HYDO_TEST_DEADPORT` | test | address | skipped | hydo-read tests | A port nothing listens on, for hydo-read's store tests. |

## The guest interface (never read from hydo's environment)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_STEPS` | guest | integer | - | set by hydo-control; read by the project's commands | The workload's step count (for invariants). |
| `HYDO_ACKED` | guest | list | - | set by hydo-control; read by commands, hydo-read, hydo-world, hydo-grpc | The acknowledged ids (for invariants and the recovery). |
| `HYDO_OUTCOMES` | guest | list | - | set by hydo-control; read by commands, hydo-read, hydo-grpc | The outcomes of the workload's steps. |
| `HYDO_FAILED` | guest | list | - | set by hydo-control; read by commands, hydo-read | The failed ids. |
| `HYDO_SVC` | guest | name | - | set by hydo-control | The service a command is about. |
| `HYDO_LOG` | guest | path | - | set by hydo-control | The service's log file. |
| `HYDO_FROM` | guest | integer | - | set by hydo-control | The first step of a workload range. |
| `HYDO_TO` | guest | integer | - | set by hydo-control | The last step of a workload range. |
| `HYDO_VALUE` | guest | text | - | set by hydo-control | A knob's value. |
| `HYDO_CMD` | guest | id | - | set by hydo-control (the composer) | A composed command's id. |
| `HYDO_CMD_ID` | guest | id | - | set by hydo-control (the composer) | A composed command's entry. |
| `HYDO_ROUND` | guest | integer | - | set by hydo-control (the composer) | A composed command's round. |
| `HYDO_SEED` | guest | integer | - | set by hydo-control (the composer); read by hydo-grpc | A composed command's seed. |
| `HYDO_CHECK` | guest | id | - | set by hydo-control (the composer) | A composed check's id. |
| `HYDO_AGENT_TTY` | guest | path | - | hydo-agent (guest) | The agent's console device (hydo-agent, a guest program with pinned bytes). |
| `HYDO_WORLD_LOG` | guest | path | - | hydo-world (guest) | hydo-world's record log. |
| `HYDO_WORLD_SOCK` | guest | path | - | hydo-world (guest) | hydo-world's control socket. |
| `HYDO_GRPC_CALLS` | guest | path | - | hydo-grpc (guest) | hydo-grpc: the call records file. |
| `HYDO_GRPC_CONSOLE` | guest | flag | - | hydo-grpc (guest) | hydo-grpc: write records to the console. |
| `HYDO_GRPC_CONSOLE_DEV` | guest | path | - | hydo-grpc (guest) | hydo-grpc: the console device. |
| `HYDO_GRPC_DIR` | guest | path | - | hydo-grpc (guest) | hydo-grpc: the generated driver's directory. |
| `HYDO_GRPC_SEED` | guest | integer | - | hydo-grpc (guest) | hydo-grpc: the request stream's seed. |
| `HYDO_GRPC_STATE` | guest | path | - | hydo-grpc (guest) | hydo-grpc: the state file. |
| `HYDO_GRPC_TARGET` | guest | address | - | hydo-grpc (guest) | hydo-grpc: the server address. |

## Variables of an image's /etc/hydo/manifest.sh (not environment variables)

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|
| `HYDO_TC` | image | path | - | written by hydo-build; read by hydo-control, hydo-engine | The image's `tc` (manifest.sh), read back by the controller. |
| `HYDO_IP` | image | path | - | written by hydo-build; read by hydo-control | The image's `ip` (manifest.sh). |
| `HYDO_IPT` | image | path | - | written by hydo-build; read by hydo-control | The image's `iptables` (manifest.sh). |
| `HYDO_CHECKS` | image | list | - | written by hydo-build; read by hydo-control | The image's history checks (manifest.sh). |

## Set for programs hydo starts

| Variable | Kind | Type | Default | Readers | What it does |
|---|---|---|---|---|---|

## Removed

| Variable | Why |
|---|---|
| `HYDO_SNAP_DIR` | the snapshot directory of the deleted C engine; hydo-hvx keeps snapshots in memory (M132, M137) |
| `HVX_SNAP_DIR` | passed to the engine program for the deleted C engine; hydo-hvx never read it (the C verify harnesses in hv/hvx read their own) |
| `HVX_SNAP_MALLOC` | passed to the engine program for the deleted C engine; hydo-hvx never read it |
| `HYDO_SNAPSHOTS` | chose between file and memory snapshots of the deleted C engine; it changed only the pool's printed line |
| `HYDO_RAM_MIB` | M145: the guest RAM of the engine check example (engine_check.rs), deleted with the shell images' plain runs |
| `HYDO_BENCH_KIND` | M145: hydo-bench's image kind; it runs the manifest-built agent images only (the stack kind, shell-init images, was deleted) |
| `HYDO_M59_SUFFIX` | M145: the m59 suffix of the deleted stack kind of hydo-bench's images |
| `HYDO_CLI` | told the explorer program it was started by the deleted Python CLIs; `hydo explore` calls the explorer in its own process |

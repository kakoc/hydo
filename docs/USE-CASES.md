# Use cases: what hydo serves and what it could serve

This page lists every use case that hydo serves today, and every idea for one that it does not
serve yet. Read it to see all of hydo's capabilities in one place, and to choose what to build
next. It is a reference: each row names a command or a key, and where its proof is.

hydo runs a whole system in one virtual machine (VM): the services, their databases, and a client
that uses them. An image is the built guest of one project. A schedule is one line of text that
names the faults of one run, for example `disk=none | run K=redis@11+1 C=24`. The same image and
the same schedule give a bit-identical run. A finding is a run that broke a check, shrunk to the
fewest faults that still break it.

Each row has one of these status words:

- `built`: the capability is in `main`, and a recorded run proves it. The row names the run.
- `partly built`: the capability is in `main`, with gaps that an open item or a milestone names.
- `idea`: the capability is not built. The section [The ideas](#the-ideas) describes it.

A row says "not hydo's job" when the technique needs something that a VM cannot give. Examples
are production traffic and real users. Every status comes from `AGENTS.md`, a milestone of
`docs/HISTORY.md`, or an example's results page (`hydo/onboard/examples/<name>/RESULTS.md`).
"M238" is short for Milestone 238 of `docs/HISTORY.md`, and "open item 13" is item 13 of the list
"Open items" in `AGENTS.md`.

## The map against the testing stages

A common chart of testing sorts the techniques into four stages: before production, at deploy,
at release, and after release. The tables below take the chart's techniques in its order.

### Before production

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Unit tests | `workload.kind: test` runs a team's own test program in the VM as one step, with faults only when `faults:` lists them. | built | M205; the examples `nats-flaky-8363`, `etcd-flaky-22796`, and `qdrant-flaky-10200` |
| Functional tests | Commands in `hydo/workload/` use the system, and checks in `hydo/checks/` say what must hold after each run. | built | Every example; `docs/ONBOARD.md` |
| Component tests | One service runs against fakes of its outside services (`fakes:`), ready-made emulators (`emulators:` for Stripe, SMTP, and S3), or a fake from an OpenAPI description (`hydo fake from-openapi`). Emulators run on service networks (`network: bridge`) too since M316. The s3 emulator answers conditional writes 412 as S3 does, and `H=s3.put_if_absent@...:status=412` makes a lost race (M329). The s3 emulator writes its objects without an fsync, so a power cut of the whole guest loses objects it acknowledged (M343). Since M354 `durable: true` keeps them: the emulator's folders get ext4's attribute `S`. The s3 emulator answers conditional writes 412 as S3 does, and `H=s3.put_if_absent@...:status=412` makes a lost race (M329). The s3 proxy's request shapes and conditional routes ran in a VM on hvx, the Mac engine, and KVM in M341. | built | M89, M242 (`checkout`), M249 (`openapi-payouts`), M316, M329, M341 (`checkout/hydo-s3.yaml`), M343 (`greptimedb`), M354 (`checkout/hydo-s3-cut.yaml`); open item 171 |
| Fuzz tests | `hydo fuzz` calls a Rust function with generated inputs natively. `hydo fuzz --vm` makes the calls inside the VM under power cuts and disk errors, and under clock steps, pauses, and kills inside a call. `hydo explore` draws the faults of each run from a seed. | partly built | M275, M282, M289, M292, M296 (`fuzz-vm`), M307 (enumerations, trait methods, `build` lines), M308 (a third evaluation: 9 known upstream bugs reproduced), M318 and M326 (its findings fixed). M327: a fourth evaluation, and M333 its findings fixed. Open items 139 and 163. |
| Property-based tests | Function contracts (`// hydo: requires` and `ensures`) judge each generated input of `hydo fuzz`, and each call in a whole-system run. Value properties (`// hydo: keep`) judge a recorded value along its history. | partly built | M241, M255, M291 (`blnk`); open items 113 and 146 |
| Formal verification | Not hydo's job. A proof covers every state of a model, and hydo checks only the runs that it makes. | not hydo's job | |
| Static analysis | `hydo rules scan` lists a project's promises from its sources. `hydo rules scan --kind clock` lists the code that measures a duration with the wall clock. Both read text, not syntax. Since M324 the clock audit leaves out test code inside source files and ranks first the code of the Cargo and Go programs that the services of `hydo.yaml` run. It does not follow a project's own clock function, such as GreptimeDB's `current_time_millis()` (M343). Since M350 it also lists a reading of the clock kept as the order or the version of data, such as the timestamp of Garage's CopyObject (M349). `hydo rules scan --kind writes` lists the routes of a project's API that write, each with its handler, from the router code of five languages, `.proto` files and OpenAPI files (M344). Since M350 it reads a Rust enumeration of endpoints too, and lists Garage's 42 write routes. It found no route in Kvrocks, whose commands are a C++ table (M351). The clock audit found no place in Kvrocks, which reads the wall clock through its own `util::GetTimeStamp` (M351). Since M355 the clock audit follows a C++ wrapper of the clock, Kvrocks's `util::GetTimeStamp`. The write scan reads the command tables of Redis-compatible servers in C and C++, 142 commands of Kvrocks. | partly built | M236, M248, M324, M343, M344, M349, M350, M351, M355; M342 (Prometheus's finding 2); open items 110, 117, 122, 166, 171, and 173 |
| Mutation tests | `hydo rules check --mutate` plants a change of the stored state and says whether each rule catches it. A change of the program is not built. | partly built | M236; open item 110 |
| Coverage | hvx traces the guest's code with Intel Processor Trace (`HVX_EXPLORE_PT_ELF`). A CI report names the properties that no run reached. Coverage-guided exploration found no bug sooner. `error_paths: true` records which error-handling places of a program ran, on the Mac engine, and `hydo report --paths` lists the places that only faults reached and the places never reached. | partly built | M40, M41, M44, M197, M306 (`blnk`); open item 155 |
| Benchmarks | `perf:` in the manifest counts the work of each workload step, and `hydo perf compare A B` says each change in words. The work is exact on hvx only. | partly built | M237 (`perf-kv`) |
| Contract tests | For functions, see the property-based row. For APIs, `hydo fake from-openapi` builds a fake from the provider's published description, so the client runs against that contract. | partly built | M241, M249 |
| Lint | Not hydo's job. A linter reads source text, and hydo runs programs. | not hydo's job | |
| Acceptance tests | Business rules are checks: a SQL query over the system's tables, or the generic rule over the client's acknowledged operations. `hydo rules check` validates proposed rules on runs without faults. | built | M215, M218, M224, M236, M251 (`blnk`, `lago`, `saleor`) |
| Regression tests | `hydo save` keeps a finding's schedule in `hydo/regressions/`, and `hydo repro --regressions` replays every saved one. A regression that needs other build parameters names them (`# params: k=v`), and is replayed only on an image built with them (M355). Since M360 `hydo save` writes that line for an image built with `--set`. It gives a second finding of one filename a file of its own. `hydo ci run` reports such a regression as left out. | built | M197, M269, M355, M360 |
| Smoke tests | `hydo doctor` checks the machine, `hydo build --check` checks the project, and the schedule `disk=none \| run` runs the system once without faults. | built | M239; the quickstarts |
| Usability tests | Not hydo's job. They need people who use the product. | not hydo's job | |
| UI and UX tests | `browser:` runs a team's Playwright tests in the VM and watches every page. `browser.explore` walks through the pages without tests, and `browser.read_back` checks that the pages show what the server acknowledged. `browser.screens: compare` compares screenshots pixel for pixel. The look and feel needs people. | partly built | M253, M254, M258, M273, M284, M334 (`web-shop`, `memos`, `vikunja`); open items 124, 131, 145, and 167 |
| Penetration tests | Not hydo's job. They need an attacker's tools and goals. `hydo fuzz` of a decoder of untrusted input is the nearest capability (Qdrant's `decompress_sequence`, M275). | not hydo's job | |
| Threat modeling | Not hydo's job. It is a design review that people do. | not hydo's job | |
| Performance tests | `perf:` counts work per step, and `hydo run --memory` prints each service's peak memory. Time in the VM is virtual, so speed on real hardware is out of reach (`docs/LIMITS.md`). | partly built | M237, M239 |

### At deploy

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Integration tests | The whole system runs in one VM. `hydo onboard`, `hydo compose`, and `hydo k8s` start the project from a repository, a Compose file, or Kubernetes manifests. `hydo.yaml` reads YAML anchors and `<<` merge keys, and an image's FIFOs reach its root, since M316. Images of a private registry are fetched with the credential of `docker login`, and `--from-docker` takes an image from the local Docker, since M320. Since M317 (the fingerprint epoch 5) an image service mounts its system folders once per boot, so a readiness check cannot hide its `/dev/shm`. Since M324 `hydo build` warns when the roots do not fit in the Mac's guest RAM or hvx's disk. Since M331 (the fingerprint epoch 5) each image service has a `/tmp` of its own, as a container has. `hydo compose` then writes `root.share_images: true` for services of one image, so they share one copy of the image's root. Since M338 (the fingerprint epoch 5) an image's hard links stay links in the Mac's root. An image service then sees the cgroup2 files under `/sys/fs/cgroup`, on the Mac engine, hvx and KVM (M341), and the warning counts the whole root and its compressed archive. | built | M84, M214, M229, M243, M316, M317, M320, M324, M331, M338, M341; every example |
| Tap compare | `hydo compare-versions` runs the same schedules on two versions and compares the verdicts, the client's outcomes and histories, and the stored state (`services[].compare`). `hydo versions` replays one finding on several versions. | built | Milestone 298 (Redis settings, Formance Ledger v2.3.24 against v2.4.0); M221 |
| Load tests | `parallel_driver_` commands run copies of a workload command at once, so races show without faults. Throughput is not hydo's job: time in the VM is virtual, and a guest has at most 8 GiB of RAM. | built for concurrency | M86; `formance-ledger`, `blnk`, `medusa` |
| Shadowing | Not hydo's job. It sends copies of production traffic to a new version. The nearest command is `hydo compare-versions`, on schedules instead of traffic. | not hydo's job | |
| Configuration tests | Build parameters (`params:`, `hydo build --set k=v`) give one image per setting, and `hydo versions --param` replays a finding per setting. The idea [Configuration as an explored dimension](#configuration-as-an-explored-dimension) draws the settings. | partly built | `dragonfly` (`--set takeover=yes`), `swupdate` (`--set dio=1`); Milestone 299, being built; `iggy` (`hydo explore --configs` over the topics' `durability`, M314), `clickhouse` (`fsync`, three values, M311) |
| Soak tests | The idea [Soak tests with exact resource counts](#soak-tests-with-exact-resource-counts). `hydo run --memory` (M239) and `perf:` (M237) are its first parts. | idea | |

### At release

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Canary releases | Not hydo's job. A canary needs a share of production traffic. The upgrade token `U=` tests the switch of a version itself, member by member, with rollbacks. | not hydo's job | M238 (`etcd-upgrade`) |
| Monitoring | Not hydo's job. It watches a live production system. | not hydo's job | |
| Traffic shaping | Not hydo's job in production. Inside the VM, `N=` (loss, delay, and partitions) and `O=` (one-way loss) shape the guest's traffic as faults. | not hydo's job | M24, M195 |
| Feature flags | Not hydo's job to serve flags. A flag can be a build parameter, and the idea [Configuration as an explored dimension](#configuration-as-an-explored-dimension) would draw its values. | not hydo's job | |
| Exception tracking | In production, not hydo's job. Before production, `errors: true` makes each error-level line of a service's log a finding until its kind is accepted. | built | M94, M215 (`formance-ledger`, the only signal of ledger#2542) |

### After release

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Teeing | Not hydo's job. It copies production traffic. | not hydo's job | |
| Profiling | `perf:` gives the exact work of each step on hvx. The experiment `HYDO_COST` counted flushes, writes, and branches per phase. Exact profiles per function are the idea [Exact profiles](#exact-profiles). | partly built | M198, M237 |
| Logs and events | Each run keeps its control transcript and the services' logs (`<run>/logs/`). A failing workload command leaves its last lines in `one.log`, and the diagnosis writes its output beside the services' logs (M348). `hydo inspect events` shows the timeline, and `hydo report --html` shows a failure as one web page. | built | M178, M220, M228, M239, M348 |
| Chaos tests | `hydo explore` draws kills, pauses, partitions, power cuts, disk faults, clock steps, storage faults, faults of the disk's blocks, and upgrades, and shrinks each failure to a replayable schedule. hydo does it in a VM before release, not in production. | built | M38, M102, M195, M238, M245, M313; every example, among them `greptimedb` (M343: region failover with the WAL in Kafka and the data on S3) |
| Monitoring | Not hydo's job. | not hydo's job | |
| A/B tests | Not hydo's job. They need real users. | not hydo's job | |
| Tracing | Value history (`// hydo: record`) records chosen values at every call without a change of the run. `HYDO_PREEMPT_RECORD=1` records each forced preemption point. `hydo diff` names where two runs first differ. | partly built | M165, M208, M285; open items 77 and 146 |
| Dynamic exploration | `hydo explore` searches the fault space from seeds, and `browser.explore` walks through a web app's pages. `--shard I/N` and `--merge` spread one exploration over many machines. An exploration that refuses before its first run exits with status 5, so a script tells it from one that ran (M328). An option whose draws the engine refuses, or that has nothing to act on in the image, is refused then, one line each (M345). So is an option whose draws the manifest leaves nothing of (M348). Its `stats.json` and `hydo stats` show the curve of new behaviour, the cost, the faults drawn, and how many runs fit in a budget (M336). On the Mac engine the workers of one exploration share the snapshots' RAM, so more of them fit (M335). | built | M38, M102, M258, M304, M328, M335, M336, M345, M348 |
| Real user monitoring | Not hydo's job. It needs real users. | not hydo's job | |
| Auditing | `hydo rules scan --kind clock` audits the code for durations on the wall clock. The control transcript records every command and fault of a run. An audit of access or compliance is not hydo's job. | partly built | M248 (`valkey`, valkey#5406) |
| On-call experience | When an incident's cause is a fault that hydo can make, a schedule makes it again. `hydo inspect` opens the failed run at any stop, and `hydo report --html` says what broke in plain words. Paging and runbooks are not hydo's job. | partly built | M178, M228 |

## Use cases by who asks

Each section lists the questions of one type of team. The tables give hydo's answer today. The
last line names the ideas that would answer more.

### A backend team with a database or a queue

| The question | What hydo answers today | Status |
|---|---|---|
| Does an acknowledged write survive a power cut? | `C=` cuts the whole guest's power, and `cf=` cuts it at the k-th disk flush (hvx only). History checks and the messages check judge what survived. Since M353 (the fingerprint epoch 6) the recovery boot's `$HYDO_OUTCOMES` holds the first boot's last outcomes by default, so a check knows which ids had no answer. | built; `nats-jetstream`, `vikunja`, `yugabyte`, `saleor`, `restate`, `iggy`, `clickhouse`, `prometheus` (samples in a listed block survive; a snapshot is read back at once and after the cut) |
| Does one node's damage lose data or stop the cluster? | `G=` damages one file of a service's storage, and `L=` cuts one service's power. `hydo explore --storage-faults` draws both. | built; `nats-jetstream/blocks`, `redpanda`, `surrealdb`, `iggy`, `victoriametrics` (replicas on two nodes) |
| Does the storage engine survive a disk that tears or misplaces a write, cannot read a sector, or stops answering? | `torn=`, `mis=`, `lse=` and `hang=` on the data disk, and `G=<svc>@<at>:bad\|rot` under a file of a service's storage. `hydo explore --block-faults` draws them. | partly built; `disk-faults` on the Mac engine and on KVM (AMD and Intel); hvx refuses them (M313; open item 160) |
| Do the guest's timeouts fire on time late in a long, busy run? | On hvx an x86_64 image built at the fingerprint epoch 5 keeps time with its TSC (`guest.exact_tick`). Its clocks follow the virtual TSC at every uptime and read in steps of 2 µs. The Mac and KVM guest kernels read one counter for the clock and the timer. | built; `clickhouse` seed 20 (M319, M323), `docs/design/guest-clock.md` |
| Does a backward clock step break a lease or a timeout? | `T=` steps the guest's wall clock. `hydo rules scan --kind clock` proposes the steps. | built; `valkey`, `temporal`, `dragonfly`, `river`, `restate`, `iggy` (a topic's message expiry), `victoriametrics` (a step back deletes a month, holds new samples in memory, and leaves one copy), `prometheus` (scrape times freeze after a step back: prometheus#8312) |
| Does an acknowledged job run, and run once? | A `type: messages` check declares the claims. Kills of the process that holds the work test them. A durable execution's journaled steps append to an effects file, and a rule finds a step that runs again. Since M360 `no-loss` with a `lease` counts a received message that no acknowledgement accepted and that never came back as lost. | built; `hatchet`, `river`, `redpanda`, `restate`, `iggy`; the NSQ trial (M359, M360) |
| Does a crash between two commits of one operation run it twice? | `A=<event>+<ms>:kill-now-<svc>` kills the service at the moment the client writes an event, or a number of milliseconds later, before any thread of the service runs on. `A=<event>#<k>...` acts at the k-th record of the event. `hydo explore --reactive-at <event>` tries every delay from 0 to 50 ms, each with `kill-now-<svc>`, `kill-<svc>` and a power cut. It runs one schedule per seed. `hydo explore --reactive fault --kill-now` draws such kills at random. It draws them also on a project with one service. | built; `meilisearch` finding 1 on the Mac engine and hvx (M305). Found blind on the Mac engine (M309), and by the aimed grid at schedule 29 of 153 (M345; open items 154 and 173) |
| Does a node start again after a power loss that follows a failover? | The client fails over as Sentinel does and writes once more to the old master. So the old master and its replicas rejoin by a full synchronization. `hydo explore --reactive-at <the failover's event> --react-actions cut` cuts the power seconds later. A `WAIT` check after the heal tells a replica that holds the data from one that acknowledged it. | built; `kvrocks` findings 1 and 2 on the Mac engine (M351; open item 179) |
| Is a retried write stored once, and on every replica? | The client retries each write with the same data on any replica and records every try. A messages check with one group per replica (`fan_out: every-group`) and one delivery per stored copy judges loss, duplicates and replication; `type: messages` needs no queue. | built; `clickhouse` (M311) |
| Does a worker that stops past the failure detector's timeout, and comes back as itself, stay marked dead? | `P=<svc>@<step>+<n>` pauses a service, which keeps its identity. `hydo explore --new-faults` draws such pauses. `hydo guide strategies` says to outlast the failure detector's timeout and to run production's replica counts and features. | built; a user's onboarding of a distributed query engine, where a pause of one worker for 8 s left queries running forever (M330) |
| Does an acknowledged write end as acknowledged, or fail or vanish later by itself? | The claim `acked_outcome` of a `type: messages` check. The check's `final:` reads back each acknowledged send's final state and the system's error. Where it can, it also reads whether the effect is there. A failure with an error that the project names as the client's keeps the claim. A send with an unknown outcome may end in any state. The reference is the section "Read back the final states" of `hydo guide messages`. | built; `meilisearch` on the Mac engine (M346; open item 174), and `garage`'s writes of an object store, each judged by whether the key holds its value (M349; open item 177). The `kvrocks` example judges writes of strings, hashes, lists, sets, sorted sets, transactions and scripts (M351; open item 179). |
| Does a listing or a scan show what a read of each key shows, after a replica comes back? | A `final_` check lists the keys, then reads each one, and names every key that differs. A kill, a pause or a power loss of one node, while the workload deletes, makes a stale replica. | built; `garage` finding 2 on the Mac engine, a listing of deleted keys and of aborted uploads (M349; open item 177) |
| Does every accepted operation end, and does a completed result stay readable? | An `eventually_` check for each type of outcome that the client records as unknown, with `within=` longer than the system's own timeout. A second check reads each completed result after the heal (`hydo guide strategies`, "How each unknown outcome ends"). | built; the same onboarding, where the two checks found both bugs (M330) |
| Which checks and faults should a new project start with? | `hydo guide strategies` maps a type of system to the techniques that found bugs. Each technique has its evidence and its cost. | built; M293, M330 |
| Has the exploration seen enough, or should it run longer? | `hydo stats` prints the curve of new behaviour: the distinct verdicts, property outcomes, error kinds, and error paths after each run in seed order. It says whether the last quarter of the runs showed anything new. It also lists the fault kinds that `faults:` allows and no schedule drew; since M360 a reaction counts as the fault that its action makes. | built; the Redis quickstart on the Mac engine (M336), and on hvx and KVM, also as two shards merged (M341) |
| Do the books still match the client's operations? | The generic rule and domain rules, proposed by `hydo rules scan` and validated by `hydo rules check`. | built; `blnk`, `lago`, `formance-ledger` |
| Does the workload call every operation that writes? | `hydo rules scan --kind writes` lists the API's write routes and RPC methods, each with its handler, grouped by resource and ranked from durable data to logins and metrics. The onboarding skill gives each route of class `data` or `admin` a workload command. | partly built; on six examples' sources every known write of the RESULTS pages was listed, and 109 of 114 sampled routes were writes (M344; open item 175). On Garage it listed none (M349); since M350 it lists 42, every write of Garage's workload among them. Since M355 it lists the 142 write commands of Kvrocks's command tables |
| Where did a wrong value come from? | Value history and value properties on the Mac engine. | partly built; `blnk` (M285, M291) |
| Did the faults reach the error handling at all? | `error_paths: true` records which `if err != nil` bodies, `Err` returns, `?` branches, panics and retry functions ran in each run. `hydo report --paths` lists what only faults reached, against `hydo explore --no-faults`, and what no run reached. | partly built; `blnk` on the Mac engine (M306; open item 155) |
| What is the smallest schedule after which a service never starts again? | `hydo explore` shrinks a service that does not start again (`ERROR <svc> restart failed`) and a workload or a check that does not end (`TIMEOUT workload`, the Mac engine's `exit budget exhausted`) by the error's signature, as it shrinks a violation. A system's detection of the damage that hydo made is shrunk only with `--shrink-detected-damage`. | built; M339; the Redis quickstart on the Mac engine, hvx and KVM, the scripted engine; TIMEOUT shrinks on hvx and exit-budget shrinks on KVM (M341) |
| Does the cluster upgrade in place? | `U=` switches a member's version during a run, and `hydo explore --upgrades` draws it. | built; `etcd-upgrade` (M238) |
| Can hydo run the team's images from a private registry? | `hydo compose` and fetch-images.sh log in with the credential that `docker login` saved: an `auths` entry, or the helper that `credHelpers` or `credsStore` names. `hydo compose --from-docker` saves an image from the local Docker when hydo cannot log in, and pins its digest. | built; tests with a registry in the test process and Docker's `registry:2` (M320; open item 165). No real private registry ran. |

Ideas that would answer more:

- [Configuration as an explored dimension](#configuration-as-an-explored-dimension);
- [Soak tests with exact resource counts](#soak-tests-with-exact-resource-counts);
- [Value properties over two values and over call order](#value-properties-over-two-values-and-over-call-order);
- [Clocks per service](#clocks-per-service);
- [Repeated fault windows and membership changes](#repeated-fault-windows-and-membership-changes);
- [Resource limits per service](#resource-limits-per-service);
- [Faults of the disk's blocks on hvx and on added disks](#faults-of-the-disks-blocks-on-hvx-and-on-added-disks); and
- [Transactions across messages](#transactions-across-messages).

### A team with a web app

| The question | What hydo answers today | Status |
|---|---|---|
| Why is one of the team's Playwright tests flaky? | `browser:` runs the tests in the VM, and an `H=` delay of the API makes the flaky test fail on every run. | built; `web-shop` (M253) |
| Does a page show an uncaught error, a spinner that stays, or a failed request it never shows? | Five invariants watch every page that a test or a walk opens. A spinner ends with its document, so a page that the walk leaves is not a stuck spinner. | built; `web-shop` (M253, M301) |
| Can hydo test the pages without the team's tests? | `browser.explore` and the token `I=` walk through the pages from a seed, and each run leaves the walk as a Playwright test. Every run keeps a screenshot of every action, and a failing run the Playwright trace of every step, also across a power cut; `hydo report --html` shows them as the run's timeline. | partly built; `memos`, `vikunja` (M258, M273, M301, M334); open items 124, 131, and 167 |
| Does the page still show what the server said it saved? | `browser.read_back` opens the pages at the end of the run and after a power cut, and looks for every object whose id the server returned in an acknowledged write. | built; `vikunja`, `web-shop` (M334); both on KVM (M341); open item 168 |
| Is a saved object still saved after a crash? | A rule over the database, with power cuts of the whole guest. | built; `vikunja` finding 1 (M273) |
| Did a page change visually? | `browser.screens: compare` compares each page with a recorded screenshot. | partly built; M254; the Mac engine ran no comparison (open item 145) |
| Does the frontend behave when a payment API fails? | Fakes, emulators, and `hydo explore --protocol-cover`. | built; `web-shop`, `openapi-payouts` (M257) |

Ideas that would answer more:

- [A walk that follows the page](#a-walk-that-follows-the-page);
- [Faults inside a step](#faults-inside-a-step); and
- [Stateful emulators](#stateful-emulators).

### A library author in Rust

| The question | What hydo answers today | Status |
|---|---|---|
| Does a function keep its contract for every valid input? | `hydo fuzz` generates inputs from `requires` and `input` lines, judges each call by `ensures`, and shrinks a failure. Since M307 also enumerations, trait methods on a named type, and types that a constructor builds, such as bytes' `Bytes`. Since M318 also const generic parameters, `&'static` parameters, closures, and `Self::Owned` results. Since M326 also bit operators, two lists of one length, `&'static mut` parameters, and closures of two arguments and of sorts, and a test module that does not build names its cause. A fourth evaluation (M327) fuzzed 71 of 88 contracted functions of six new crates. Since M333 hydo also takes a type parameter's `A::Item`, an error type with private fields, and generic structures with public fields. A constructor may take no argument or return a `Result`, so all 88 build without a helper. A constructor with builder methods still needs one. | partly built; M275, M282, M289, M307, M308, M318, M326, M327, M333; open items 139 and 163 |
| Does a function that writes files keep its promise under a power cut? | `hydo fuzz --vm` with a `// hydo: durable` check, which runs again in the recovery boot. | partly built; `fuzz-vm` on hvx (M292) and on the Mac engine (M296, M297) |
| Does a function that measures a timeout or waits for a thread keep its contract? | `hydo fuzz --vm` steps the clock, pauses the call or its thread, or kills it, a few milliseconds after the call's start (`E=` and `A=fuzz.call+<ms>:<action>`). | partly built; `fuzz-vm` and rs-snowflake on hvx and the Mac engine (M296) |
| Does a contract hold inside the running system? | `contracts:` in the manifest checks every call at hardware breakpoints, without a change of the run. | partly built; the Mac engine and KVM (M241, M255) |
| Is a flaky test a real race? | `workload.kind: test` with a pause of one thread (`P=workload/<thread>@...`). Since M338 (the fingerprint epoch 5) the pause leaves out a thread that ended before its move and finds the workload behind busybox's `timeout`. | built; `qdrant-flaky-10200` (M226, M338) |

Ideas that would answer more:

- [Active property tests from a snapshot](#active-property-tests-from-a-snapshot); and
- [Function fuzzing in more languages](#function-fuzzing-in-more-languages).

### An embedded or device team

| The question | What hydo answers today | Status |
|---|---|---|
| Does a power cut during an update leave a bootable slot of an allowed version? | Reactions at the update's console events (`A=...:cut`), `C=`, and `cf=`. | built; `swupdate`, `mender`, `rauc` (M259, M272) |
| Does the update hold with real reboots and separate storage chips? | `guest.reboot: true` and `guest.disks`, each disk with its own write cache. | partly built; added disks on the Mac engine and KVM only (M263; open item 128) |
| Where exactly in a commit does a cut break the device? | One run per delay of a reaction, written as a shell loop. | partly built; `mender` (M276; open item 136) |

Ideas that would answer more:

- [Added disks on hvx](#added-disks-on-hvx);
- [A built-in search of a reaction's delay](#a-built-in-search-of-a-reactions-delay); and
- [A device's boot details](#a-devices-boot-details).

### A team that adopts hydo in CI

| The question | What hydo answers today | Status |
|---|---|---|
| Did this pull request add a failure? | `hydo ci run` decides each finding against the base by replay: `new`, `pre-existing`, `fixed`, and more. | built; M197; a GitHub trial in a private repository (open item 67) |
| Can a known bug stay without failing every pull request? | `hydo ci accept` writes the finding into `hydo/known-findings.yaml`. | built; M223 |
| Can the run fit a time budget? | `budget:`, `--shard I/N` with `hydo ci merge`, and `seeds_from: history`. `hydo stats --budget 2h --workers N --shards N` estimates the runs that fit from an exploration's own measures (M336). | built; M223, M286, M336; ran on the Mac only (open item 67) |
| What is the history of each finding? | `hydo ci record` keeps a store, and `hydo ci page` writes it as one web page. Its headline counts the open findings of every branch, and each run shows when it started. | built; M286, M301; not run on GitHub |
| Can the pull request show only what it changed? | `hydo ci run` writes `comment.md`, which the GitHub action posts and edits. | built; M286 |
| Which commit introduced a finding? | `hydo versions --git A..B` replays it per commit, by hand. | partly built; M221 |
| Does an exploration spend VMs on runs it already ran? | A first run or a shrink candidate whose schedule the exploration already ran takes the earlier result without a VM. `exploration.json` counts `repeats` and `repeats_saved_s`. | built; M339; the Redis quickstart on the Mac engine, and on hvx and KVM (M341) |
| Can a shrink candidate skip the part it shares with an earlier candidate? | `hydo explore --shrink-from-snapshots`: the reruns and the candidates snapshot where a step range or a composed round starts, and a later candidate on the same worker continues from the deepest one it reaches. `--check-continued` compares each with its run from S1. | built on KVM, on by default (four GitHub runners, the Redis example); M340; exact on the Mac engine too, but slower there, so off (open item 172) |
| Which problems did all the rounds of exploration find? | `hydo report A B C` and `hydo report --html A B C` read several explorations of one image, made with different options. They list each problem once by its class and signature, and name every exploration and seed that showed it. | built; M345 (the Meilisearch example's aimed and blind rounds on the Mac engine) |
| Can a long exploration run on many machines at once? | `hydo explore --shard I/N` explores one part of the seeds, and `hydo explore --merge` makes one exploration of the parts. hydo's workflow `explore.yml` runs up to 20 parts on GitHub's KVM runners. | built; M304 (Redis, Redpanda, and Dragonfly on GitHub's runners), M310 |
| Can more workers of one exploration fit on one Mac? | The Mac engine's workers map guest RAM from the snapshots' shared RAM bases, and each holds only the pages its runs write. The explorer counts the bases once and lets as many workers boot at once as fit. | built on the Mac engine; Redis and Meilisearch (M335); hvx and KVM do not share between workers (open item 36) |

Ideas that would answer more:

- [CI stage 3](#ci-stage-3-bisection-the-object-store-and-one-shrink-across-shards); and
- [Cost per operation in CI](#cost-per-operation-in-ci).

### A maintainer who receives a bug report

| The question | What hydo answers today | Status |
|---|---|---|
| Can the maintainer see it fail? | `hydo run <image> '<schedule>'` repeats the run bit for bit, and `hydo repro` replays a run directory. | built; every example |
| Why did it fail? | `hydo report --html`, `hydo inspect` at any stop of the run, `hydo diff`, and `hydo explain`. The report names a step by the schedule's number. A service that its environment stopped in a recovery boot is not said to have lost data (M316). `hydo inspect diagnose <explore dir> --problem N` diagnoses an exploration's problem, and the report prints the `hydo run ... --out` that makes its run directory (M324). A root too big for guest RAM gets a verdict that names the sizes (M328). A run that used up the engine's exit budget names the check that ran, or gives the check's violation (M328). `hydo inspect stop --all` frees the guests of forgotten sessions (M328). On hvx a boot whose agent lines pass 16 MiB ends with a verdict that names the error-log scan (M348). | built; M165, M178, M220, M228, M301, M324, M328, M348 |
| Is it a bug of the system or of the setup? | Each failure class has a label and advice (`hydo_explore::advice`). Since M328 a failure without a power cut has a class named for its fault families (`process`, `network+clock`). A script under Docker in the example's `native/` folder confirms the cause outside hydo. | built; M228, M328; `redpanda/native`, `saleor/native` |
| Which versions have it? | `hydo versions` bisects over versions, assuming one change point. | built; `formance-ledger` (M221) |
| Is the fix complete? | The same schedule on the fixed build, and the saved regression in CI. | built; `blnk`, `qdrant-flaky-10200` |

Ideas that would answer more:

- [Deeper explanations of where two runs differ](#deeper-explanations-of-where-two-runs-differ);
- [An MCP server for hydo inspect](#an-mcp-server-for-hydo-inspect); and
- [A preemption record on hvx](#a-preemption-record-on-hvx).

### Someone who evaluates an upgrade or a dependency bump

| The question | What hydo answers today | Status |
|---|---|---|
| Does the new version pass every saved regression? | `hydo repro --regressions` on an image of the new version, or `hydo ci run`. | built; M197 |
| Is a known finding still there in the new version? | `hydo versions` with `--param`, `--git`, or `--images`. | built; M221 |
| Does a rolling upgrade, or a rollback, keep the data? | `U=<target>@<at>:<version>`, with `+<back>` for a rollback. | built; `etcd-upgrade`, `surrealdb/upgrade` (M238, M277) |
| Does the new version answer as the old one did? | `hydo compare-versions` runs the same schedules on both versions and names the first difference. | built; Milestone 298 |
| Does each operation cost more after the bump? | `perf:` and `hydo perf compare`, exact on hvx. | partly built; M237 |
| Did the bump change a default setting that matters? | Not answered today. | idea; [Configuration as an explored dimension](#configuration-as-an-explored-dimension) |

### Someone who keeps hydo and its skill current

| The question | What hydo answers today | Status |
|---|---|---|
| Is a newer hydo or a newer onboarding skill out? | `hydo update --check` names each part with its version and the newest release's (exit status 3 when one is newer). `hydo doctor` prints one line about a newer hydo, at most once a day. With a GitHub token (`GH_TOKEN`, `GITHUB_TOKEN` or `gh auth login`), GitHub's limit for requests without a login does not apply. At that limit the commands say so with the reset time, and they exit with status 4. | built; M315 (a fake release in tests; the real `--check` against GitHub), M322 (the limit and the token against the fake release only) |
| Can one command update hydo and every copy of the skill? | `hydo update --yes` updates hydo the way it was installed (Homebrew, cargo, the release archive, the install cache). It updates the skill in the known folders, in every Claude Code configuration folder, and in every folder where hydo wrote it, and Claude Code's plugin `hydo@hydo`. Since M332 the skill has hydo's version and comes from the same release, and a skill of the old line 1.0.0 to 2.4.0 counts as older. | partly built; M315, M332; only fake brew, cargo and claude programs ran it (open item 161) |
| Which skill goes with the installed hydo? | The skill of the same version: from 0.5.0 on, each hydo release `v<version>` carries `hydo-skill-<version>.tar.gz`, and the skill's step 0 asks for the hydo of its version (`metadata.hydo-min`). | built; M332 (a fake release in tests; no release carries the skill yet) |

## The ideas

Each idea says what it is, why it matters, what it would need, and where it came from. An idea
that a milestone builds moves into the tables of this page with its new status.

### Configuration as an explored dimension

- **What it is.** The explorer draws a system's settings as it draws faults, and the shrinker
  removes the settings that a failure does not need.
- **Why it matters.** Many findings depend on one setting: Redis's `appendfsync`, libSQL's
  durability, Dragonfly's `takeover`. Today each setting is a separate image, chosen by hand.
- **What it needs.** Settings that change without a new image, or one image per drawn setting,
  and a schedule token that names them.
- **Where it came from.** The user's chart of testing stages; being built as Milestone 299
  (tentative number), not in `main` on 2026-11-17.

### Soak tests with exact resource counts

- **What it is.** A long run that counts memory, open files, disk use, and work per operation
  at every step, and fails when a count grows without a bound.
- **Why it matters.** A leak shows only after hours. In a deterministic run, every count is
  exact, so a growth of one page per operation is visible.
- **What it needs.** The guest-side counters of `docs/design/cost-tracking.md` section 6, read at
  every step, and runs longer than the 65,536 messages of one run (`hydo_engine_proto::MAX_SENDS`).
- **Where it came from.** A conversation with the user; the cost experiment (M198), `hydo run
  --memory` (M239), and `perf:` (M237).

### A check that an error path is reached under faults

- **What it is.** A declaration that a panic, an error branch, or a recovery path must run in at
  least one run of an exploration. hydo reports the paths that no run reached.
- **Why it matters.** Static architecture rules, such as those of the cargo-pup lint tool, say
  where errors may go. Only a run shows that the error handling runs at all under real faults.
- **What is built.** Milestone 306 records which error paths ran, on the Mac engine, and lists
  the paths never reached (`hydo guide error-paths`). The declaration that a path must run, which
  fails an exploration, is not built (open item 155).
- **What it needs.** A property that an exploration breaks, not one run, and the error paths
  on KVM and hvx.
- **Where it came from.** A conversation with the user about static architecture rules, and the
  user's direction to observe the code that runs under faults.

### Value properties over two values and over call order

- **What it is.** A `keep` line that compares two recorded names, a value with a constant, or the
  order in which functions ran.
- **Why it matters.** Blnk's overdraft limit had two copies, one in the transaction and one in its
  metadata. A property over both names states the bug directly.
- **What it needs.** A wider language of `// hydo: keep`, the property table of `result.json`
  with the `keep` lines, and `hydo rules check` that runs them.
- **Where it came from.** Open item 146; the section "Next steps" of `docs/design/value-history.md`.

### Function fuzzing in more languages

- **What it is.** `hydo fuzz` for Go and other languages, beside Rust.
- **Why it matters.** Many of the onboarded systems are Go. Build it only if an evaluation shows
  that the language holds users back: the user's direction is Rust and the experience first.
- **What it needs.** A harness writer and a value reader per language. Before that, a fresh
  user's evaluation of the kind that M280 and M288 made must show the need.
- **Where it came from.** Open item 139; the user's direction on function fuzzing.

### Active property tests from a snapshot

- **What it is.** From a snapshot at a function's entry inside a running system, hydo calls the
  function with inputs from its `input` domains, in branches that leave the run as it was.
- **Why it matters.** It tests a function in the real state of the system, not in a copy of the
  crate.
- **What it needs.** Branches from a mid-run snapshot on the Mac engine or KVM, and a call
  injected at a breakpoint.
- **Where it came from.** The section "Next steps" of `docs/design/contracts.md`.

### Contracts for interpreted languages

- **What it is.** Contracts and value history for interpreted and JIT-compiled programs.
- **Why it matters.** Lago, Saleor, and Ghost run on Ruby, Python, and Node. Today contracts read
  only compiled Rust and Go through their debug information.
- **What it needs.** A stop in each interpreter's call path, and reads of the interpreter's own
  structures.
- **Where it came from.** The section "Next steps" of `docs/design/contracts.md`.

### A guest counter for loops without exits on the Mac engine

- **What it is.** A counter of the guest's branches on the Mac, so that the engine can stop a
  loop that makes no VM exits.
- **Why it matters.** k3s's kine loops without exits, so k3s does not start on the Mac engine.
  KVM has `guest.branch_tick` for this (M274).
- **What it needs.** An M2 has no such counter. The guest PMU that macOS 15 emulates with guest
  EL2 on M3 and later chips was not measured.
- **Where it came from.** Open items 13 and 119; `docs/design/exit-free-loops.md`.

### Breakpoints for contracts and value history on hvx

- **What it is.** Hardware breakpoints in the hvx module, so that contracts and value history run
  on hvx as on the Mac engine and KVM.
- **Why it matters.** hvx is the reference engine and runs the most examples. uprobes changed the
  fingerprint (M241).
- **What it needs.** The side module of the section "The hvx design" of `docs/design/contracts.md`, the full
  suite on it, and then a reload of the main module.
- **Where it came from.** Open items 113 and 146.

### CI stage 3: bisection, the object store, and one shrink across shards

- **What it is.** The commit that introduced each new finding (`hydo bisect`), `hydo ci
  reproduce` on a laptop, an object-store backend for the history, and one shrink per signature
  across shards.
- **Why it matters.** A team with many shards and a long history needs the commit that caused a
  finding, and a store that is not a directory.
- **What it needs.** About four to six milestones, mostly independent of each other
  (`docs/design/ci.md` section 9).
- **Where it came from.** `docs/design/ci.md`; open item 67.

### Cost per operation in CI

- **What it is.** A pull request comment that names exact changes of cost, such as flushes per
  acknowledged write, base against head.
- **Why it matters.** A timing benchmark in CI is noisy. A count from a replayable run is exact.
- **What it needs.** A `cost` object in `result.json` from the host-side counters, and the
  guest-side counters for marked schedules.
- **Where it came from.** `docs/design/cost-tracking.md` section 6 (M198).

### Exact profiles

- **What it is.** The work of each function of a service, counted exactly in a run under faults.
- **Why it matters.** A profile that repeats bit for bit shows a change of a few branches between
  two builds.
- **What it needs.** Samples at exact branch counts on hvx, or breakpoints at chosen functions.
- **Where it came from.** A conversation with the user on observing whole systems: value history,
  the code that runs under faults, and exact profiles.

### An MCP server for hydo inspect

- **What it is.** The commands of `hydo inspect` as a Model Context Protocol (MCP) server, the
  interface that AI coding tools use to call programs.
- **Why it matters.** The first users of `hydo inspect` are AI agents (`docs/design/inspect.md`).
  Today they drive it through a shell and files.
- **What it needs.** A server over the existing session, with bounded answers.
- **Where it came from.** Open item 56; the section "Left for later" of `docs/design/inspect.md`.

### Environments for AI agents

- **What it is.** An environment that an agent can create from an image, branch, and drop, each
  branch exact and replayable.
- **Why it matters.** Training and evaluation of agents need environments that repeat. The spike
  of M199 showed branches with `hydo inspect exec --branch`.
- **What it needs.** A network through recorded outside services, branches that can be dropped,
  an incremental state hash, and several vCPUs under lock-step control.
- **Where it came from.** Open item 68; `docs/design/agent-environments.md`.

### Deeper explanations of where two runs differ

- **What it is.** `hydo diff` and `hydo explain` that also compare the client's replies, the disk
  requests, and guest memory at each stop.
- **Why it matters.** For a durability bug, the disk requests name the writes that were not
  flushed. Today the diff stops at the controller's records.
- **What it needs.** An engine log of disk requests and packets outside the fingerprint, and a
  hash of RAM and disk at a chosen stop.
- **Where it came from.** Open item 49; the section "What deeper levels need later" of `docs/design/explain.md`.

### A preemption record on hvx

- **What it is.** A record of each tick that a timing schedule forced on hvx, with the guest's
  instruction pointer and page table.
- **Why it matters.** Without it, a verdict that flips between two hvx timing schedules cannot be
  traced to the tick that made the difference. KVM and the Mac engine have the record (M208).
- **What it needs.** A small log in the module and a new ioctl to read it.
- **Where it came from.** Open item 77.

### Resource limits per service

- **What it is.** Memory and CPU limits per service, as Compose's `mem_limit` and `cpus` give a
  container.
- **Why it matters.** A program that sizes itself from `memory.max` or `cpu.max` uses the whole
  guest today, which hides its behaviour at its real size.
- **What it needs.** A cgroup per service under the cgroup2 mount of M283. Since M338 an image
  service sees that mount's files.
- **Where it came from.** Open item 144.

### Clocks per service

- **What it is.** A clock step for one service while the others keep the guest's clock.
- **Why it matters.** Real clusters have clocks that differ between nodes. Today `T=` steps the
  whole guest's clock.
- **What it needs.** A time-namespace variant of the guest kernel, or a time offset per process.
- **Where it came from.** Item 9 of the section "Ranked plan" of `docs/design/jepsen-gaps.md`.
  The Prometheus example (M342) needed it too: its agent and its server share the guest's clock,
  so a server whose clock alone steps ahead of its senders cannot be tested.

### Repeated fault windows and membership changes

- **What it is.** One token that repeats a fault (`K=n1@3+2/7+2`), and faults that add or remove
  a cluster's members during a run.
- **Why it matters.** Some bugs need a fault that comes back, or a member that joins during a
  failure.
- **What it needs.** New schedule syntax, an explorer option, and the system's own commands for
  membership.
- **Where it came from.** Items 6 and 10 of the section "Ranked plan" of
  `docs/design/jepsen-gaps.md`.

### Added disks on hvx

- **What it is.** `guest.disks` on hvx. The Mac engine and KVM have it since M263.
- **Why it matters.** A device model with separate storage chips runs only on the Mac engine and
  KVM today, and hvx refuses it before any VM.
- **What it needs.** A second block device in the module with its own PCI function, queue, cache,
  snapshot, and fingerprint part; first on a side module.
- **Where it came from.** Open item 128.

### Faults of the disk's blocks on hvx and on added disks

- **What it is.** `torn=`, `mis=`, `lse=`, `hang=` and `G=...:bad|rot` on hvx, and on the disks
  of `guest.disks` on every engine. The Mac engine and KVM have them on the data disk since M313.
- **Why it matters.** A storage engine on hvx, or a device model whose storage chips are added
  disks, meets no torn write, misdirected write, bad sector, or stuck request.
- **What it needs.** On hvx, the faults in the module's virtio block device, with their part of
  the snapshot and the fingerprint, first on a side module (`docs/design/block-faults.md`, "What
  hvx would need"). On added disks, the same state per disk in the engines, and a disk name in
  the tokens.
- **Where it came from.** Open item 160.

### A built-in search of a reaction's delay

- **What it is.** An explorer option that runs one reaction at every delay of a range, to map the
  window where a fault breaks a promise.
- **Why it matters.** The explorer draws a reaction's delay from six fixed values. Mender's
  window on hvx was 69 to 87 ms after the event and needed a shell loop.
- **What it needs.** A range in the reaction's token and an explorer mode that walks it.
- **Where it came from.** Open item 136.

### A device's boot details

- **What it is.** The device's kernel command line, squashfs, and the device mapper in the Mac
  engine's guest kernel.
- **Why it matters.** RAUC reads the booted slot from the kernel command line, and its `verity`
  bundles need the device mapper.
- **What it needs.** A kernel variant for devices and a manifest key for the command line.
- **Where it came from.** Open item 125.

### Faults inside a step

- **What it is.** A kill, a pause, or a network fault in the middle of a workload step, timed by
  the guest's clock.
- **Why it matters.** A page walk's step takes one step, so a fault starts only between steps.
  Memos's finding 2 needs a refused write in the middle of a step. Since Milestone 296 a fuzzed
  call gets its faults inside it from a reaction at the call's start.
- **What it needs.** A timed form of each guest fault that the explorer draws for every manifest.
- **Where it came from.** Open items 131 and 139.

### A walk that follows the page

- **What it is.** A page walk that chooses its actions by what the page shows, such as a memo by
  its text, with breadth that depends on the page alone.
- **Why it matters.** A fault changes the page, and so the walk. Then a shrink candidate takes
  other actions, and the walk loses its failure.
- **What it needs.** A choice of action keyed by the control's role and name, not by its place.
- **Where it came from.** Open items 129 and 131.

### Leaving a fault kind or a known bug out of an exploration

- **What it is.** An option that leaves one fault kind out, and a way to keep a known finding
  from coming back in every schedule.
- **Why it matters.** The SurrealDB and Dragonfly examples needed copies of the manifest for this.
  In SurrealDB, half of the drawn clock steps reached a known bug.
- **What it needs.** An explorer option, and `hydo explore` that reads `faults:` with the build's
  parameters.
- **Where it came from.** Open items 140 and 141.

### Upgrades of image services and several upgrades per schedule

- **What it is.** `U=` for a service that runs a Docker image, and more than one `U=` in a drawn
  schedule.
- **Why it matters.** Many teams run their services from images, and an upgrade often follows
  another.
- **What it needs.** Versions as images in the manifest, and an explorer that draws several `U=`.
- **Where it came from.** Open item 109.

### Kubernetes beyond one pod

- **What it is.** `hydo k8s` with the replicas of a Deployment, a Service's port mapping,
  `localhost` shared by a pod's containers, and the Kubernetes API.
- **Why it matters.** A team whose system depends on leases or operators cannot test them through
  the importer today.
- **What it needs.** A proxy per Service, a network namespace per pod, and k3s in the guest on
  every engine (open item 119).
- **Where it came from.** Open items 114 and 119.

### Stateful emulators

- **What it is.** Emulators that keep state and send webhooks, such as a Stripe emulator that
  remembers payments.
- **Why it matters.** stripe-mock keeps no state and sends no webhooks, so a webhook's bugs stay
  out of reach.
- **What it needs.** An image with Node.js 24 that hydo builds and pins, for the emulate project
  of vercel-labs.
- **Where it came from.** Open item 116.

### A rules scan that reads syntax

- **What it is.** `hydo rules scan` and the clock audit that parse the code instead of matching
  words.
- **Why it matters.** The scan read none of Saleor's 25 `models.py` files, and the clock audit
  found no place in Redpanda's C++.
- **What it needs.** A parser per language, and a reading of calls through interfaces and
  channels.
- **Where it came from.** Open items 110, 117, 122, and 173.

### Transactions across messages

- **What it is.** A claim of the messages check over consume-transform-produce transactions, with
  a cycle search over them.
- **Why it matters.** Redpanda's half-committed transaction shows as a loss today, without the
  transaction it belongs to. The Pulsar example (Milestone 356) needed a hand-written final check
  over a subscription of its own to judge each transaction all or nothing. The claims judged the
  acknowledgements inside a transaction as plain ones.
- **What it needs.** A `txn` operation with send and commit parts in the history format.
- **Where it came from.** The section "Left open" of `docs/design/messaging.md`, and open item
  181.

### A fleet that hydo runs

- **What it is.** Machines that build images, cache them, and explore a project without a stop, with a time budget per project.
- **Why it matters.** A team needs KVM or bare metal today, and GitHub's runners stop a job at 6 hours with 2 to 4 CPUs.
- **What it needs.** A queue, workers on bare-metal hosts with KVM, and an image cache by input key. A KVM fingerprint holds within one CPU vendor, and on Intel within one model (open item 102), so the fleet needs one CPU model or fingerprints per model.
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### An app for GitHub instead of a workflow file

- **What it is.** An app that a team installs on its repositories instead of a workflow file. hydo gets the pushes and pull requests and answers with checks and comments.
- **Why it matters.** The action needs a workflow file and its inputs in the team's repository, and its examples never ran with a pull request from a fork (open item 67).
- **What it needs.** The app's webhooks, the check runs API, and the comment of `hydo ci run` (M286).
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### Onboarding as a pull request

- **What it is.** The onboarding skill runs on hydo's side against a team's repository and opens a pull request with a `hydo/` folder whose clean runs pass.
- **Why it matters.** An onboarding of a real project takes an expert hours (`docs/ONBOARDING-PLAYBOOK.md`), and a team stops at it.
- **What it needs.** The skill in a sandbox with the fleet's engines, and a check that the plan's clean runs pass before the pull request opens.
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### A web app of findings for a team

- **What it is.** Accounts, teams, each finding across branches, triage (accept, assign, link to an issue), notifications in Slack or email, and a link to a finding that anyone on the team opens.
- **Why it matters.** `hydo ci page` (M286) is one static page of a directory store that one person reads.
- **What it needs.** A hosted store of `hydo ci record`'s runs, the page's views as a web app, and the accept file written back as a pull request.
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### A replay in one click

- **What it is.** A button on a finding that replays it on the fleet and opens an `hydo inspect` session in the browser, or downloads the image and the schedule for a local replay.
- **Why it matters.** A developer who did not run the exploration needs the image and the engine to see the failure.
- **What it needs.** `hydo inspect` sessions on the fleet and a browser terminal, or [An MCP server for hydo inspect](#an-mcp-server-for-hydo-inspect).
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### From a finding to a proven fix

- **What it is.** A coding agent gets a finding's deterministic replay and writes a fix. hydo proves it with the replay, the saved regressions, and a new exploration before the pull request opens.
- **Why it matters.** A deterministic replay is the input that an agent needs to fix a bug, and the proof that the fix holds.
- **What it needs.** The parts exist: `hydo ci` decides `fixed` by replay, `hydo versions` names the version that brought a finding, and `hydo inspect` explains it. A loop joins them.
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### Billing by deterministic work

- **What it is.** Metering of a project's runs by a unit that is exact and the same on every run.
- **Why it matters.** A hosted service needs a unit that a team can predict and check.
- **What it needs.** The work counts of M198 (`docs/design/cost-tracking.md`) or the guest's retired branches of `perf:` (M237), and quotas per organization.
- **Where it came from.** The user, 2026-11-18: what hydo lacks to be a hosted service end to end.

### Shrinks that skip what they already ran

- **What it is.** A shrink candidate continues from a snapshot of an earlier candidate where
  their commands part. Since Milestone 339 the explorer takes the earlier result for a schedule
  it already ran, without a snapshot. Since Milestone 340 a candidate continues from a snapshot
  at a step range or a composed round of an earlier run on its worker, on by default with KVM. What is left is
  a snapshot that costs less than the part it skips on the Mac, snapshots that every worker
  loads, and points inside a round.
- **Why it matters.** Shrinks took 27 % to 83 % of the seconds of the three measured
  explorations that shrank. 10 % to 21 % of the shrinks' seconds repeated an earlier schedule
  exactly, and 10 % to 43 % of their guest clock was already run by an earlier candidate. The
  first runs of an exploration share almost nothing: 0.07 % of 12,590 s.
- **What it needs.** A cheaper Mac snapshot, and snapshots that every worker loads. Every
  compared run matched on the Mac engine and on KVM (M340).
- **Where it came from.** Milestone 337 (tentative number), `hydo/explore/RESEARCH-shared-prefix.md`;
  open item 167.

## How to keep this page current

Every milestone that builds or changes a capability updates its rows here: the status, the
command or key, and the evidence. A milestone that builds an idea moves it from
[The ideas](#the-ideas) into the tables. A new idea from an open item, a design page, or a
conversation gets its entry here in the same commit. `AGENTS.md` "How to record new work" step 4
gives the rule.

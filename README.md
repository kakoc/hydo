<p align="center">
  <img src="docs/img/hero.svg" width="860" alt="An animated terminal in a repository with the Redis example. hydo onboard finds its hydo/ folder and installs the Claude Code skill, whose command is /hydo:onboard. hydo explore runs Redis 30 times with different faults: 13 runs fail, in 3 different ways, and each failing run fails again the same way when hydo replays it. hydo report lists the 3 problems, each with a label, how many runs show it, and the command that shows it again: Redis does not start again after disk errors, Redis loses confirmed data after a power cut with appendfsync set to no, and Redis loses confirmed data with a faulty disk.">
</p>

<h1 align="center">hydo</h1>

> What if the bug that shows up once in a thousand runs of your tests showed up every time?<br>
> What if the bugs that nobody wrote a test for turned up on their own?<br>
> What if "never charge a customer twice" were checked while your database crashes?

hydo runs your whole system in one virtual machine: your services, their databases, and a
client that uses them. Then it breaks things. It kills processes, pauses them, cuts the network,
pulls the power, makes the disk fail, and moves the clock. After each run it checks the rules
that your system promises, for example that every confirmed payment is still there.

Nothing inside that machine is left to chance, so a run repeats bit for bit. When a rule breaks,
hydo takes away faults until only the ones the failure needs are left. You get one command that
makes the same failure happen again, tomorrow or on a colleague's laptop.

- **It looks for bugs on its own.** `hydo explore` chooses the faults itself, runs them, and
  checks your rules after each run. It found the etcd and NATS bugs below without being told what
  to look for.
- **Every failure comes back.** hydo writes the faults of each run as one line of text. `hydo run`
  with that line repeats the run, and the failure with it.
- **The rules can be your business rules.** A rule is a command you write, such as a SQL query
  over your tables. Two such queries found the two Blnk bugs below.
- **Your software runs unmodified:** Linux programs, plain files, or Docker images. A Docker
  Compose file imports with one command.

## Watch it break a system

https://github.com/user-attachments/assets/ab903228-0ee6-454f-bf14-c29224d613ce

hydo runs [Blnk](hydo/onboard/examples/blnk/), a ledger, again and again, each time with other
crashes, pauses, network cuts, and clock jumps. Each tile is one function of Blnk's code. It lights
up the first time a run reaches it: green in a normal run, orange only when something broke. In the
first run hydo finds a known Blnk bug, an all-or-nothing batch of transfers that kept half of
itself ([#438](https://github.com/blnkfinance/blnk/issues/438)), and shows the one command that
replays it.

## Install

On a Mac, with Homebrew:

```sh
brew install kakoc/tap/hydo
```

On a Mac or on Linux, with cargo:

```sh
cargo install hydo --locked
hydo setup
```

`hydo setup` downloads the kernel and the tools of hydo's virtual machine, and `hydo doctor`
checks your machine. hydo needs an Apple Silicon Mac, or an x86_64 Linux machine with KVM. `hydo
update` updates hydo and its onboarding skill. [docs/INSTALL.md](docs/INSTALL.md) has the release
archives, their checksums, and hydo's own kernel module for Intel machines.

## What hydo found in real projects

Each project's folder in [`hydo/onboard/examples/`](hydo/onboard/examples/) has hydo's setup for it, so you can see
each bug happen yourself.

**New bugs that hydo found and reported to each project:**

- **[ClickHouse](hydo/onboard/examples/clickhouse/)**: after a power cut, a change to a table's columns gets stuck for
  good, and every later change to that table waits behind it ([#134373](https://github.com/ClickHouse/ClickHouse/issues/134373)).
- **[RabbitMQ](hydo/onboard/examples/rabbitmq/)**: a message that RabbitMQ confirmed is lost for good when one server
  loses power while another one is paused ([#18515](https://github.com/rabbitmq/rabbitmq-server/issues/18515)). After the
  clock is set back, a stream has no server to write to for that long
  ([#18516](https://github.com/rabbitmq/rabbitmq-server/issues/18516)).
- **[Microsoft Garnet](hydo/onboard/examples/garnet/)**: when data moves to another server and that server's backup
  takes over right after, part of the data has no server that answers for it, for good
  ([#2440](https://github.com/microsoft/garnet/issues/2440)). A backup server stops copying its main server after that one
  restarts, and still reports that it is in sync ([#2441](https://github.com/microsoft/garnet/issues/2441)).
- **[Redpanda](hydo/onboard/examples/redpanda/)**: a transaction can be saved in one place and dropped in another, and
  a confirmed write can be lost. Redpanda confirmed both and fixed them
  ([#395](https://github.com/redpanda-data/community/issues/395), [#396](https://github.com/redpanda-data/community/issues/396)).
- **[Apache Pulsar](hydo/onboard/examples/pulsar/)**: after a server restart, the unfinished transaction of a client
  that crashed holds back every message after it ([#27749](https://github.com/apache/pulsar/issues/27749)).
- **[Apache Kvrocks](hydo/onboard/examples/kvrocks/)**: a backup server that has just copied all the data from its main
  server does not start again after a power cut ([#3827](https://github.com/apache/kvrocks/issues/3827)).
- **[VictoriaMetrics](hydo/onboard/examples/victoriametrics/)**: after the clock is set back, every server deletes the
  current month's data ([#11973](https://github.com/VictoriaMetrics/VictoriaMetrics/issues/11973)).
- **[Typesense](hydo/onboard/examples/typesense/)**: a server that restarts repeats a delete in the wrong order and
  loses documents that it had saved ([#3202](https://github.com/typesense/typesense/issues/3202)). After the clock is set
  back, the cluster does not replace a failed leader for that long
  ([#3203](https://github.com/typesense/typesense/issues/3203)).
- **[Meilisearch](hydo/onboard/examples/meilisearch/)**: after a crash, a swap of two indexes runs a second time and
  undoes itself ([#6743](https://github.com/meilisearch/meilisearch/issues/6743)). A power cut empties an upload that
  Meilisearch had accepted ([#6749](https://github.com/meilisearch/meilisearch/issues/6749)).
- **[Prometheus](hydo/onboard/examples/prometheus/)**: a backup snapshot leaves out data that arrived late
  ([#20547](https://github.com/prometheus/prometheus/issues/20547)).
- **[Dragonfly](hydo/onboard/examples/dragonfly/)**: after the clock is set back, a restart loads an older copy of the
  data ([#8826](https://github.com/dragonflydb/dragonfly/issues/8826)).
- **[Temporal](hydo/onboard/examples/temporal/)**: after the clock is set back, one database crash leaves the server
  without its database for that long ([#13099](https://github.com/temporalio/temporal/issues/13099)).
- **[Valkey](hydo/onboard/examples/valkey/)**: after the clock is set back, copying data to a backup server waits for
  that long ([#5406](https://github.com/valkey-io/valkey/issues/5406)).
- **[Restate](hydo/onboard/examples/restate/)**: after the clock is set back and a restart, accepted work waits for
  that long ([#5951](https://github.com/restatedev/restate/issues/5951)).
- **[NSQ](hydo/onboard/examples/nsq/)**: a crash in the first seconds of a new queue loses the messages it had
  accepted ([#1542](https://github.com/nsqio/nsq/issues/1542)).
- **[SurrealKV](hydo/onboard/examples/surrealdb/)**: one damaged byte on the disk makes it lose later data that it had
  saved ([#536](https://github.com/surrealdb/surrealkv/issues/536)).
- **[Saleor](hydo/onboard/examples/saleor/)**: a refund can be lost, and a gift card can be charged for an order that
  stays unpaid ([#20022](https://github.com/saleor/saleor/issues/20022), [#20023](https://github.com/saleor/saleor/issues/20023)).
- **[Blnk](hydo/onboard/examples/blnk/)**: payments can go past their overdraft limit ([#437](https://github.com/blnkfinance/blnk/issues/437)), and
  a batch that must be all or nothing can be half done ([#438](https://github.com/blnkfinance/blnk/issues/438)).
- **[Lago](hydo/onboard/examples/lago/)**: one month can be billed twice ([#7164](https://github.com/getlago/lago-api/issues/7164)).
- **[Hatchet](hydo/onboard/examples/hatchet/)**: after a crash, work that Hatchet had accepted never runs
  ([#5527](https://github.com/hatchet-dev/hatchet/issues/5527)).
- **[Formance Ledger](hydo/onboard/examples/formance-ledger/)**: two payments that use a new account at the same moment
  can make one of them fail ([#2542](https://github.com/formancehq/ledger/issues/2542)).

**Known bugs that showed up rarely, and with hydo show up on every run:**

- **[etcd](hydo/onboard/examples/etcd/)**: a server that crashes at one moment comes back out of step with the others
  ([#14685](https://github.com/etcd-io/etcd/issues/14685)). hydo found it without being told where to look.
- **[NATS](hydo/onboard/examples/nats-jetstream/)**: a power cut deletes a whole stream ([#6888](https://github.com/nats-io/nats-server/issues/6888)).
  hydo found it without being told where to look.
- **[TigerBeetle](hydo/onboard/examples/tigerbeetle/)**: a server stops for good when two payments expire in the same
  nanosecond ([#3960](https://github.com/tigerbeetle/tigerbeetle/issues/3960)).
- **[Qdrant](hydo/onboard/examples/qdrant-flaky-10200/)**: a test that passed 2,604 times in a row fails on every run
  when hydo pauses one of its threads at the right moment
  ([#10200](https://github.com/qdrant/qdrant/issues/10200#issuecomment-6803314544)).

## Your own project

In your repository, run:

```sh
hydo onboard
```

Then:

1. `hydo onboard` starts a `hydo/` folder from your Docker Compose file, or writes a skeleton
   whose placeholders fail every run until you fill them in.
2. It installs the onboarding skill for the AI coding tools it finds, such as Claude Code. The
   skill teaches the tool to set up hydo.
3. In Claude Code, type `/hydo:onboard`. The skill asks about your system and writes the client
   and the rules with you.
4. `hydo explore` looks for failures, and `hydo report` lists each one with the command that
   replays it.

To install the skill as a Claude Code plugin without `hydo onboard`, type these in Claude Code:

```text
/plugin marketplace add kakoc/hydo
/plugin install hydo@hydo
/hydo:onboard
```

[docs/ONBOARD.md](docs/ONBOARD.md) describes the onboarding, and
[docs/YOUR-PROJECT.md](docs/YOUR-PROJECT.md) shows how to write the `hydo/` folder by hand.

## Learn more

- [The Linux quickstart](hydo/dist/linux/QUICKSTART-linux.md) and
  [the Mac quickstart](hydo/dist/mac/QUICKSTART-mac.md) take you from the install to a first
  exploration of a Redis example.
- `hydo guide`, the manual built into the command. Start with `hydo guide workflow`.
- [The example projects](hydo/onboard/examples/) in this repository.
- [What hydo does not do](docs/LIMITS.md).
- [Every use case that hydo serves](docs/USE-CASES.md), each with its status, and the ideas for
  more.
- [How hydo is built](docs/ARCHITECTURE.md) and [how to contribute](CONTRIBUTING.md).

The current release is hydo 0.7.0. It is experimental: formats and commands can change between
versions.

## License

Apache-2.0 ([LICENSE](LICENSE)). The kernel module [hv/hvx/hvx.c](hv/hvx/hvx.c) is GPL-2.0-only;
[hv/hvx/LICENSE.md](hv/hvx/LICENSE.md) has the details.

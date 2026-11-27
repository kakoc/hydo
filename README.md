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

Each project's folder in [`hydo/onboard/examples/`](hydo/onboard/examples/) holds its hydo setup
and the recorded runs.

**New bugs, reported upstream:**

- **[ClickHouse](hydo/onboard/examples/clickhouse/):** after a power loss, a `MODIFY COLUMN` of a dropped column
  fails on every retry and blocks every later mutation ([#134373](https://github.com/ClickHouse/ClickHouse/issues/134373)).
- **[RabbitMQ](hydo/onboard/examples/rabbitmq/):** a stream loses a confirmed message, because its coordinator
  elects a writer without the node that holds it ([#18515](https://github.com/rabbitmq/rabbitmq-server/issues/18515)). After a clock step back, a
  stream has no writer for the size of the step ([#18516](https://github.com/rabbitmq/rabbitmq-server/issues/18516)).
- **[Microsoft Garnet](hydo/onboard/examples/garnet/):** a failover right after a slot migration leaves the slot
  without an owner for good ([#2440](https://github.com/microsoft/garnet/issues/2440)). A replica reports its link up but never
  replicates again after its primary restarts ([#2441](https://github.com/microsoft/garnet/issues/2441)).
- **[Redpanda](hydo/onboard/examples/redpanda/):** a transaction commits on one partition and aborts on the other,
  and a produce is answered with an earlier transaction's offset. Both confirmed and fixed
  ([#395](https://github.com/redpanda-data/community/issues/395), [#396](https://github.com/redpanda-data/community/issues/396)).
- **[Apache Pulsar](hydo/onboard/examples/pulsar/):** after a broker restart, a crashed client's open transaction
  blocks its partitions until a transactional client connects ([#27749](https://github.com/apache/pulsar/issues/27749)).
- **[Apache Kvrocks](hydo/onboard/examples/kvrocks/):** a replica that has just caught up by a full sync does not
  start again after a power loss ([#3827](https://github.com/apache/kvrocks/issues/3827)).
- **[VictoriaMetrics](hydo/onboard/examples/victoriametrics/):** a clock step back deletes the current month's data
  on every storage node, and new rows wait in memory for the size of the step. Both fixed ([#11973](https://github.com/VictoriaMetrics/VictoriaMetrics/issues/11973), [#11974](https://github.com/VictoriaMetrics/VictoriaMetrics/issues/11974)).
- **[Prometheus](hydo/onboard/examples/prometheus/):** a snapshot leaves out the head's out-of-order samples
  ([#20547](https://github.com/prometheus/prometheus/issues/20547)).
- **[Typesense](hydo/onboard/examples/typesense/):** a restarted node replays a delete out of order and loses
  acknowledged documents ([#3202](https://github.com/typesense/typesense/issues/3202)). After a clock step back, a failed leader is not
  replaced for the size of the step ([#3203](https://github.com/typesense/typesense/issues/3203)).
- **[Meilisearch](hydo/onboard/examples/meilisearch/):** an index swap runs twice after a crash ([#6743](https://github.com/meilisearch/meilisearch/issues/6743)),
  and a power loss empties an enqueued addition and a finished dump ([#6749](https://github.com/meilisearch/meilisearch/issues/6749)).
- **Clock steps back.** A step back of the clock delays a [Valkey](hydo/onboard/examples/valkey/) replica's full
  sync ([#5406](https://github.com/valkey-io/valkey/issues/5406)). It leaves [Temporal](hydo/onboard/examples/temporal/) without
  its database after one PostgreSQL crash ([#13099](https://github.com/temporalio/temporal/issues/13099)). It
  makes [Dragonfly](hydo/onboard/examples/dragonfly/) load an older snapshot
  ([#8826](https://github.com/dragonflydb/dragonfly/issues/8826)), and it holds [Restate](hydo/onboard/examples/restate/)'s
  invocations after a restart ([#5951](https://github.com/restatedev/restate/issues/5951)).
- **Lost writes.** [NSQ](hydo/onboard/examples/nsq/) loses a new queue's messages at a crash before its first sync
  ([#1542](https://github.com/nsqio/nsq/issues/1542)). [SurrealKV](hydo/onboard/examples/surrealdb/) drops later synced commits
  after one damaged value ([#536](https://github.com/surrealdb/surrealkv/issues/536)).
- **Business rules.** [Saleor](hydo/onboard/examples/saleor/) loses a refund, and leaves a gift card charged for an
  unpaid order ([#20022](https://github.com/saleor/saleor/issues/20022),
  [#20023](https://github.com/saleor/saleor/issues/20023)). [Hatchet](hydo/onboard/examples/hatchet/) never runs acknowledged runs
  ([#5527](https://github.com/hatchet-dev/hatchet/issues/5527)). [Formance Ledger](hydo/onboard/examples/formance-ledger/) fails a
  payment with `500` in a race ([#2542](https://github.com/formancehq/ledger/issues/2542)). [Blnk](hydo/onboard/examples/blnk/) goes
  past an overdraft limit and keeps half of an all-or-nothing batch
  ([#437](https://github.com/blnkfinance/blnk/issues/437), [#438](https://github.com/blnkfinance/blnk/issues/438)).
  [Lago](hydo/onboard/examples/lago/) invoices one month twice ([#7164](https://github.com/getlago/lago-api/issues/7164)).

**Known bugs that hydo makes happen on every run:**

- **[etcd 3.5.5](hydo/onboard/examples/etcd/):** a member that crashes after a defragmentation comes back on
  another revision ([#14685](https://github.com/etcd-io/etcd/issues/14685)). hydo found it without being told what to look for.
- **[NATS 2.10.22](hydo/onboard/examples/nats-jetstream/):** a power cut, or a few crashes and pauses, delete a
  JetStream stream for good ([#6888](https://github.com/nats-io/nats-server/issues/6888)). Found the same way.
- **[TigerBeetle](hydo/onboard/examples/tigerbeetle/):** a server stops for good when two pending transfers expire
  in the same nanosecond ([#3960](https://github.com/tigerbeetle/tigerbeetle/issues/3960)).
- **[Qdrant](hydo/onboard/examples/qdrant-flaky-10200/):** a flaky test that never failed in 2,604 ordinary runs
  fails on every run when hydo pauses one thread at one moment
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

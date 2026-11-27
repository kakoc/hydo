# Apache RocketMQ under hydo: a replica group in controller mode

Apache RocketMQ (github.com/apache/rocketmq) is a message broker in Java. This example runs
RocketMQ 5.5.0, the newest release with an official image, in controller mode: a NameServer with
the controller embedded, and one replica group `broker-a` of three brokers, in one guest of hvx
on the bare-metal machine. It uses the settings that RocketMQ's documentation names for data
safety: `allAckInSyncStateSet`, which acknowledges a message only after every replica of the
SyncStateSet has it, and `SYNC_FLUSH`. It looks for bugs that nobody reported.

## The short answers

FINDINGS

## The version

- RocketMQ 5.5.0 (`rocketmq-all-5.5.0`, released 2026-04-10). Its official image
  `apache/rocketmq:5.5.0` (published 2026-05-04) is the newest: 5.5.1 (2026-08-20) has no image.
  The image is
  `docker.io/apache/rocketmq@sha256:7e8f6c9dbd9df742ed26ba69c00d4ad69e2f86a56f3ca7782ff8144dd0798132`,
  linux/amd64 `sha256:a613f1341f3404b96900cf11e5c35f2d049c103504a1dd60242a04cebf5be467`, with
  Eclipse Temurin's Java 8u482.
- The client: rocketmq-client-go v2.1.2, the newest release of RocketMQ's Go client for the
  remoting protocol, with hydo's Go SDK v0.6.0 (`hydohist`), and a small remoting client of the
  example's own (`client/remoting.go`) for what the Go client does not offer.
- hydo of `main` at `bdb403f4` (0.7.0 and Milestones 368 to 374), with the guest files of the
  published 0.6.0 Linux release, on hvx.

## The promises

These are the promises under test, in the words of RocketMQ's documentation:

- Controller mode (`docs/en/controller/deploy.md`): with `allAckInSyncStateSet`, "a message
  needs to be replicated to every replica in SyncStateSet before it is returned to the client as
  successful, and this parameter can ensure that the message is not lost". The controller elects
  a master only from the SyncStateSet while `enableElectUncleanMaster` is `false` (the default):
  electing from outside "may select a replica with lagging data as Master and lose messages".
- `SYNC_FLUSH`: the master answers a send after the message is on its disk.
- Transactional messages: a half message is delivered only after its commit, never after a
  rollback, and the broker asks the producer group about a transaction whose producer sent
  neither.
- Ordered messages: the messages of one queue are delivered in the order they were stored.
- Delayed messages and retries: a message with a delay level is delivered after its delay; a
  message that a consumer gives back is delivered again from the retry topic, and after the
  maximum reconsume times it goes to the dead-letter topic.
- Consumer offsets: the broker keeps a group's committed offset per queue, and a consumer goes on
  from it. The promise that an offset holds across a failover is the task's, unconfirmed by the
  owner: the documentation promises at-least-once delivery.

## The project

`docker-compose.yaml` runs the NameServer `namesrv` with the controller embedded
(`config/namesrv.conf`, the documentation's "Embed NameServer deployment" with one controller)
and three brokers `broker0` to `broker2` of the replica group `broker-a` (`config/broker.conf`,
started by `config/start-broker.sh`). Every broker starts as a slave with `brokerId -1`, and the
controller chooses the master. Each node's store is in a volume of its own. The JVMs have small
heaps (128 MiB for the NameServer, 256 MiB for a broker), and the commit log's files are 64 MiB.
`hydo compose` imported it, and `hydo/hydo.yaml` changes the import in these ways:

- each service keeps the address of the Compose file: the importer moved every address by one
  (`ipv4_address 10.89.0.10 ignored; the service is at 10.89.0.11`), and the brokers'
  configuration names the NameServer's and the controller's address;
- each node's store (`/home/rocketmq/store`) is a file system of its own (`storage:`), the target
  of `L=` and `G=`, which `setup:` gives to the image's user (uid 3000), as `hydo build --check`
  advised;
- a broker's port is 10911 and its readiness check a connect to it; the importer took 9876 from
  the image's `EXPOSE` for each broker;
- the logs and the JVM's GC log go to `/dev/shm`, because the image's root is read-only in the
  guest;
- the brokers' settings `allAckInSyncStateSet`, `flushDiskType`, `minInSyncReplicas` and
  `transactionCheckInterval` are build parameters; the first two are `configs:` entries;
- the three brokers form the group `brokers`, with `primary: /bin/rmq leader` (the master that
  the NameServer names), `links: all` and `restart: together` (Milestone 374).

Deviations from RocketMQ's defaults, each for the test: `transactionCheckInterval` is 10 s
instead of 60 s and `transactionTimeOut` 6 s, so that a check comes within a run;
`fileReservedTime` is 100 years, so that a forward clock step deletes no file.

## The workload

The test client `rmq` (`client/`, `build.sh`) waits in `provision:` until the NameServer names a
master and every broker, and creates the topics `j` and `o` (4 queues each) and the group `w`'s
retry and dead-letter topics `%RETRY%w` and `%DLQ%w` (1 queue each, readable) on every broker.
The test composer runs 30 rounds of 1 s with up to 2 of these commands each:

- `parallel_driver_pub`: one `SendSync` to `j`, to the queue of the command's number. One in five
  messages is a poison message (`x<id>`), and one in five is delayed by level 2, 5 s (`d<id>`).
  Only `SEND_OK` counts as acknowledged; another status or an error is an unknown outcome.
- `parallel_driver_tx`: one transactional message to `j`. `t<id>` commits, `r<id>` rolls back,
  and `u<id>` answers "unknown", so that the broker's check commits it. The decision is written
  to a synced file before the half message is sent, and every producer of the group `txg`
  answers checks from it.
- `parallel_driver_opub`: one `SendSync` to `o`, to the queue of the message's key (`k0` to
  `k3`).
- `parallel_driver_consume` and `parallel_driver_oconsume`: one consumer of the group `w` (on a
  queue of `j` or on `%RETRY%w`) or `og` (on a queue of `o`). It asks the master for the group's
  committed offset, pulls up to 4 messages from it, gives each poison message back with
  `CONSUMER_SEND_MSG_BACK` (delay level 1, at most 2 reconsumes), and commits the next offset
  with a synchronous `UPDATE_CONSUMER_OFFSET`. A lock per queue and group lets one consumer at a
  time take a queue, as RocketMQ's rebalancing does. rocketmq-client-go sends offsets one-way, so
  the example's own remoting client sends them.

The drain (`rmq drain`) answers the broker's transaction checks and takes every message left of
`j`, `o`, `%RETRY%w` and `%DLQ%w` until a quiet window of 45 s.

## The rules

Two `type: messages` checks, one history each (`hydo guide messages`):

- `jobs` (history `q`): `delivery: at-least-once`, `after_ack: no-redelivery` (a committed offset
  holds), `dead_letter: {after: 3, to: dlq}` and `delay: per-message`. A rolled-back transaction
  is a failed send, so its delivery is an aborted delivery (`no-phantom`).
- `order` (history `o`): `delivery: at-least-once`, `order: {kind: fifo, scope: key}` and
  `after_ack: no-redelivery`.

The drain is the `final:` of `jobs`; `order` has the placeholder `final: "true"`.

RESULTS_REST

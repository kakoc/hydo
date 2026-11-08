# Upstream findings: triage and draft reports

hydo's onboardings of Medusa (M174, M180), libSQL's server sqld (M173, M182), TigerBeetle (M176,
M183) and PostgreSQL (M182) reported behaviour of those projects. This document checks each one
against the project's issue tracker, release notes and documentation. It reproduces the new ones
without hydo, says where a report should go, and gives a draft report for each finding that should
be reported. **Nothing has been filed.** The drafts are for the user to review and send.

The checks were made on 2026-11-08. The links to `github.com/kakoc/hydo/tree/main/...` in the
drafts work only after this branch is merged and pushed.

## Summary

| finding | status | what to do |
|---|---|---|
| Medusa: two concurrent refunds of one order duplicate the order's items | The cause is known: [medusajs/medusa#16823](https://github.com/medusajs/medusa/issues/16823) (closed by the stale bot, not fixed), [#16824](https://github.com/medusajs/medusa/pull/16824) (a fix, closed by the stale bot, not merged) and the open [#17369](https://github.com/medusajs/medusa/issues/17369) (the fulfillment workflows). The refund path is not reported. Not fixed in 2.22.4, the newest release. Reproduced without hydo on 2.18.0 and 2.22.4. | Open a public issue (draft 1), linked to #17369. |
| sqld: the default `durability_mode` loses acknowledged transactions at a power cut | Known: [tursodatabase/libsql#2297](https://github.com/tursodatabase/libsql/issues/2297) (open, 2026-10-22). It is also SQLite's documented behaviour for `synchronous=NORMAL` in WAL mode. | No new report. |
| sqld: a setting made through the admin API is lost on every restart | Apparently new. No issue or pull request mentions it, and `main` has the same code. Reproduced without hydo on 0.24.33. | Open a public issue (draft 2). |
| TigerBeetle: failures only on a corrupted or lying disk of one replica | Not an upstream bug. TigerBeetle documents that it halts when a record is corrupted on every replica, and one replica is every replica. | Nothing to report. |
| PostgreSQL: acknowledged writes lost on a disk that drops acknowledged writes | Not an upstream bug. PostgreSQL documents that it depends on the disk keeping what `fsync` acknowledged. | Nothing to report. |

## 1. Medusa: two concurrent refunds of one order duplicate the order's items

### What hydo saw

`hydo/onboard/examples/medusa/RESULTS.md`, "What was found", item 1 (docs/HISTORY.md M174 and
M180). Two back-office refunds of the same order ran at the same time. Each refund created an
order change of type `credit_line` with the same version number. Confirming both changes wrote the
order's line item twice at that version, so the order reads two units instead of one, and the
shop's stock no longer adds up. Explorations found it blind twice. The minimal schedule was a kill
of Medusa plus a pause of Redis; the faults only move the two refund requests in time.

### Known or new

- **The cause is known.** Medusa's order module guards "one active order change per order" with an
  unlocked read followed by a create (`createOrderChange_` in
  `packages/modules/order/src/services/order-module-service.ts`). Two concurrent calls can both pass
  the guard, and each then sets `version: order.version + 1`.
  - [#16823](https://github.com/medusajs/medusa/issues/16823) (2026-09-15) describes this guard
    and shows two concurrent `createReturn` calls writing two `order_item` rows at version 3. The
    stale bot closed it on 2026-10-04 as "not planned".
  - [#16824](https://github.com/medusajs/medusa/pull/16824), a fix that locks the order row before
    the guard, was closed by the stale bot on 2026-10-07 without being merged.
  - [#17369](https://github.com/medusajs/medusa/issues/17369) (2026-10-23, open) shows the same kind
    of corruption for the fulfillment workflows (two `order_change` rows with one version,
    duplicate `SHIP_ITEM`) and names the guard as one cause.
  - [#16012](https://github.com/medusajs/medusa/issues/16012) was a different race: concurrent
    captures and refunds could exceed the authorized or captured amount. It was fixed by
    [#16097](https://github.com/medusajs/medusa/pull/16097) (merged 2026-07-22), which serializes
    the payment module's amount checks. That fix does not cover the order change that a refund
    creates.
- **The refund path is not reported.** No issue or pull request describes concurrent refunds
  through `POST /admin/payments/:id/refund` (searches for "refund race", "concurrent refund",
  "refund order change", "order change version", "active order change", `refundPaymentWorkflow`
  and `createOrderChange_`).
- **Not fixed in the newest release.** In v2.22.4 (2026-11-06), `refundPaymentWorkflow`
  (`packages/core/core-flows/src/payment/workflows/refund-payment.ts`) takes no lock on the order,
  and `createOrderChange_` is unchanged. The reproduction below corrupts orders on 2.22.4.

### Reproduction without hydo

`hydo/onboard/examples/medusa/upstream/` (its README has the full output): Docker Compose with
PostgreSQL 15, Redis 7 and a Medusa backend built from `medusa-starter-default`, using the built-in
system payment provider. `race.mjs` refunds one control order twice in sequence, then runs 20
trials. Each trial places an order of one unit, captures its payment, and sends two refunds of 10
together.

Run on 2026-11-08, Docker Engine 28.4.0 (an Ubuntu 24.04 arm64 VM) on an Apple Silicon Mac (arm64 images), Node.js
22.23.4, PostgreSQL 15.19:

| Medusa | both refunds accepted | two order changes at one version | line item written twice |
|---|---|---|---|
| 2.18.0 (the version hydo tested) | 5 of 20 | 5 orders | 5 orders |
| 2.18.0, a first run | 7 of 20 | 7 orders | 7 orders |
| 2.22.4 (newest release, 2026-11-06) | 10 of 20 | 10 orders | 9 orders |

In every other trial one refund was refused with "already has an existing active order change",
which is Medusa's guard working.

The control order (two refunds one after the other) reaches version 3 and keeps one item. In a
corrupted trial both refunds are accepted, the order stays at version 2, PostgreSQL holds two
`order_change` rows of type `credit_line` for that version, and two `order_item` rows for the same
item at that version. `GET /admin/orders/:id` then lists the item twice.

### Where to report

Medusa's [SECURITY.md](https://github.com/medusajs/medusa/blob/develop/SECURITY.md) asks for
vulnerabilities by email to security@medusajs.com. This finding is better as a **public issue**:

- Only an authenticated admin user can send the refunds. A customer cannot trigger it.
- No money moves twice: the payment module serializes refunds since #16097, and both refunds were
  within the captured amount. What is corrupted is the order's record (its items, and the stock
  and fulfilment that read them).
- The cause is already public in #16823, #16824 and #17369, so a private report would not keep
  anything private.

Use the "Bug report for v2.0" issue template (`.github/ISSUE_TEMPLATE/bug_report_v2.yml`). An
alternative is a comment on #17369, which already proposes a per-order lock for the fulfillment
workflows; a separate issue is clearer because the workflow and the endpoint differ.

### Draft 1 (issue in medusajs/medusa)

**Title:** [Bug]: Two concurrent refunds of one order create two order changes with the same version and duplicate the order's line items

**Package.json file**

```json
{
  "name": "medusa-starter-default",
  "version": "0.0.1",
  "dependencies": {
    "@medusajs/admin-sdk": "2.22.4",
    "@medusajs/cli": "2.22.4",
    "@medusajs/framework": "2.22.4",
    "@medusajs/medusa": "2.22.4"
  }
}
```

(medusa-starter-default at commit `9565d9d`, with the `@medusajs/*` versions set to 2.22.4. Also
seen on 2.18.0.)

**Node.js version:** v22.23.4

**Database and its version:** PostgreSQL 15.19 (`postgres:15-alpine`), Redis 7 for the event bus,
the workflow engine and locking

**Operating system name and version:** Linux containers (`node:22-alpine`) on Docker Engine 28.4.0 in an Ubuntu 24.04 arm64 VM on an Apple Silicon Mac

**Browser name:** n/a (admin API)

**What happened?**

Two `POST /admin/payments/:id/refund` requests for the payment of one order, sent at the same
time (two operators, or one double click in the admin), are sometimes both accepted. Each one
creates an order change of type `credit_line` with the same `version`, and confirming both writes
the order's line items twice at that version. From then on the order lists each item twice:
`GET /admin/orders/:id?fields=items.*` returns the same `ordli_…` id twice, so an order of one unit
reads as two. Fulfilment, returns and totals of the order then work from the duplicated version.

When the two requests interleave differently, one of them is refused with `Order (…) already has
an existing active order change`, which is the intended outcome. The refusal comes from
`createOrderChange_`, which lists the active changes and then creates one, without a lock. Two
requests can both pass that check. This is the guard described in #16823 (closed by the stale bot)
and named in #17369; the refund path is another way to reach it. #16097 serialized the payment
module's amount checks, but `refundPaymentWorkflow` still creates the order's credit line change
with no lock on the order.

**Expected behavior**

Two refunds of one order are serialized or one is refused, as when they are sent one after the
other: both refunds recorded, the order at version 3 with one line item (one unit).

**Actual behavior**

In 20 trials of two concurrent refunds of 10 each (one fresh order of one unit per trial):

- 10 trials: one refund refused with `Order (…) already has an existing active order change`; the
  order has one item.
- 10 trials: both refunds accepted; the order has two `order_change` rows of type `credit_line` at
  version 2. In 9 of them the line item was also written twice at version 2, and the order lists
  it twice.

On 2.18.0 the same script corrupted 5 of 20 orders (7 of 20 in a first run).

Through the admin API (a control order refunded twice in sequence, then a corrupted trial):

```
control (sequential): refunds=[ok | ok] version=3 items=[["ordli_01M6WZH0WHWDQ81586HMDK4MRS",1]] units=1
trial 1:              refunds=[ok | ok] version=2 items=[["ordli_01M6WZH4807E33CNZG04BV5EYY",1],["ordli_01M6WZH4807E33CNZG04BV5EYY",1]] units=2
```

In PostgreSQL:

```
select order_id, version, count(*), string_agg(change_type, ',') from order_change group by 1, 2 having count(*) > 1;
 order_01M6WZH4803FECCP772WG448VV |       2 |       2 | credit_line,credit_line

select order_id, item_id, version, count(*) from order_item where deleted_at is null group by 1, 2, 3 having count(*) > 1;
 order_01M6WZH4803FECCP772WG448VV | ordli_01M6WZH4807E33CNZG04BV5EYY |       2 |    2
```

A per-order lock around the credit-line order change in `refundPaymentWorkflow`, or the row lock
on order-change creation proposed in #16824, would prevent it.

**Link to reproduction repo**

https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/medusa/upstream (`MEDUSA_VERSION=2.22.4 sh run.sh 20`: Docker Compose with PostgreSQL, Redis and a Medusa backend built from the starter with the system payment provider; `race.mjs` sends the two refunds with `Promise.all`).

Found with hydo, a deterministic fault-testing tool (https://github.com/kakoc/hydo); the example is https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/medusa.

## 2a. sqld: the default durability loses acknowledged transactions at a power cut

### What hydo saw

`hydo/onboard/examples/libsql-replica/RESULTS.md`, "What was found", item 1 (M173, M182). sqld's
default `durability_mode` is `relaxed`, which runs connections with `PRAGMA synchronous=NORMAL`. A
transaction acknowledged before a power cut was missing after it.

### Known or new

**Known.** [tursodatabase/libsql#2297](https://github.com/tursodatabase/libsql/issues/2297) (opened
2026-10-22, open, no answer from the maintainers yet) reports exactly this on 0.24.33 and `main`,
with a reproduction on a device-mapper volume. It also explains why the `wallog`, which sqld does
sync on every commit, does not save the data: after an unclean shutdown sqld truncates it and
rebuilds it from the database file. The behaviour of `synchronous=NORMAL` itself is documented by
SQLite ([PRAGMA synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous): in WAL mode a
transaction committed with NORMAL "might roll back following a power loss or system crash").

No new report is needed. #2297 suggests `durability_mode: strong` through the admin API as the way
to avoid the loss; finding 2b shows that this setting does not survive a restart. Draft 2 links the
two.

## 2b. sqld: a setting made through the admin API is lost on every restart

### What hydo saw

`hydo/onboard/examples/libsql-replica/RESULTS.md`, "What was found", item 3. With
`durability_mode` set to `strong` once through the admin API, the explorer still found acknowledged
transactions lost at a power cut, always after a kill and restart of the primary. After the restart
the admin API reported `"durability_mode":"relaxed"` again. The same was then seen in Docker on the
build machine after `kill -9` and after a clean stop.

### Known or new

**Apparently new.** No issue or pull request in tursodatabase/libsql mentions a configuration that
is reset on restart (searches for "durability_mode", "namespace config", "block_writes", "config
overwritten", "reset on restart", "default namespace config"). The admin API is documented only in
code; `durability_mode` appears in no document of the repository.

**The cause** (read in `libsql-server-v0.24.33`; `main` on 2026-11-07 has the same code): when
namespaces are disabled, the default, every start of a primary runs

```rust
namespace_store.create(NamespaceName::default(), namespace::RestoreOption::Latest, Default::default())
```

(`libsql-server/src/lib.rs`), and `NamespaceStore::create` stores the configuration it receives
in the meta store before it loads the namespace (`libsql-server/src/namespace/store.rs`:
`handle.store(db_config)`). The meta store has the configuration the admin API saved (it is
written to the `metastore` database and restored at start), but this call replaces it with
`DatabaseConfig::default()`. Every field of the default namespace's configuration is reset, not
only `durability_mode`: `block_reads`, `block_writes`, `max_db_size`, `jwt_key`, `allow_attach`,
`txn_timeout`, `heartbeat_url`. There is no command-line flag for `durability_mode`, so with
namespaces disabled the admin API is the only way to set it.

### Reproduction without hydo

`hydo/onboard/examples/libsql-replica/upstream/` (its README has the full output): one sqld
primary from `ghcr.io/tursodatabase/libsql-server:v0.24.33` with `SQLD_ADMIN_LISTEN_ADDR`, and
`run.sh`, which sets the configuration, restarts sqld and reads the configuration back with the
admin API and `PRAGMA synchronous`. Run on 2026-11-08, Docker Engine 28.4.0 (an Ubuntu 24.04 arm64 VM) on an Apple Silicon
Mac (arm64 image, `sqld 0.24.33 (40a151bd 2025-12-19)`):

| step | `durability_mode` | `PRAGMA synchronous` |
|---|---|---|
| fresh start | relaxed | 1 (NORMAL) |
| `POST .../config` with `"durability_mode":"strong"` | strong | 2 (FULL) |
| 5 s later | strong | 2 |
| `SIGKILL`, start | relaxed | 1 |
| set strong again, clean stop, start | relaxed | 1 |
| set strong again, `docker compose restart` | relaxed | 1 |
| `"block_writes":true` | (writes refused: `BLOCKED`) | |
| clean stop, start | `block_writes` false, relaxed | 1, and an `INSERT` succeeds |

### Where to report

tursodatabase/libsql has no `SECURITY.md`, private vulnerability reporting is off, and there are
no issue templates; its CONTRIBUTING.md asks for bugs as issues. A **public issue** fits. The reset
of `jwt_key` and `block_reads`/`block_writes` could matter for access control, but only the
`durability_mode` and `block_writes` resets were reproduced, and the `jwt_key` case was not tried.
If the user wants to be careful about that part, it can be left out of the public issue and
mentioned to the maintainers separately (for example on their Discord, which CONTRIBUTING.md
names).

### Draft 2 (issue in tursodatabase/libsql)

**Title:** sqld: the default namespace's config set through the admin API is reset to the defaults on every restart

**Versions:** sqld 0.24.33 (`ghcr.io/tursodatabase/libsql-server:v0.24.33`, `sqld 0.24.33 (40a151bd 2025-12-19)`), arm64 and amd64 images; namespaces disabled (the default), `SQLD_NODE=primary`, `SQLD_ADMIN_LISTEN_ADDR=0.0.0.0:9090`. The code below is the same on `main`.

**What happens**

A configuration set with `POST /v1/namespaces/default/config` is applied and reads back correctly
until sqld restarts. After any restart (`SIGKILL`, a clean stop, `docker restart`), `GET
/v1/namespaces/default/config` returns the defaults again, and the server behaves accordingly:
`durability_mode` goes back to `relaxed` (`PRAGMA synchronous` 1 instead of 2), and `block_writes`
goes back to `false` (writes that were refused are accepted).

This matters most for `durability_mode`. There is no command-line flag for it, so the admin API is
the only way to make a server without namespaces sync every commit, and #2297 suggests `strong`
to avoid losing acknowledged writes at a power loss. After the next restart the server is back on
`relaxed` without any message.

**Steps to reproduce**

```sh
docker run -d --name sqld -e SQLD_NODE=primary -e SQLD_ADMIN_LISTEN_ADDR=0.0.0.0:9090 \
  -p 127.0.0.1:18080:8080 -p 127.0.0.1:19090:9090 ghcr.io/tursodatabase/libsql-server:v0.24.33
C=http://127.0.0.1:19090/v1/namespaces/default/config
pragma_sync() { curl -s http://127.0.0.1:18080/v2/pipeline -H 'content-type: application/json' \
  -d '{"requests":[{"type":"execute","stmt":{"sql":"PRAGMA synchronous"}},{"type":"close"}]}' | grep -o '"value":"[0-9]"'; }
curl -s -X POST $C -H 'content-type: application/json' \
  -d '{"block_reads":false,"block_writes":false,"durability_mode":"strong"}'
curl -s $C; pragma_sync                   # "durability_mode":"strong", "value":"2"
docker restart sqld; sleep 3         # or docker kill + docker start
curl -s $C; pragma_sync                   # "durability_mode":"relaxed", "value":"1"
```

A script that also covers `SIGKILL`, a clean stop and `block_writes`:
https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/libsql-replica/upstream

**Expected**

The configuration stored through the admin API survives a restart: the meta store saves it, and it
reads back the same after the server starts again. (If resetting it is intended for the default
namespace, the admin API could refuse the change or say that it lasts until the next restart.)

**Actual**

```
after POST strong             "block_writes":false,"txn_timeout_s":null,"durability_mode":"strong"   PRAGMA synchronous: 2
after SIGKILL and start       "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
after clean stop and start    "block_writes":false,"txn_timeout_s":5,"durability_mode":"relaxed"     PRAGMA synchronous: 1
after POST block_writes       "block_writes":true,"block_reason":"maintenance","durability_mode":"strong"   INSERT: Operation was blocked: maintenance
after clean stop and start    "block_writes":false,"block_reason":null,"durability_mode":"relaxed"   INSERT: ok
```

**Cause, as far as we can tell**

With namespaces disabled, `lib.rs` runs on every start of a primary:

```rust
if self.disable_namespaces {
    namespace_store
        .create(NamespaceName::default(), namespace::RestoreOption::Latest, Default::default())
        .await?;
}
```

and `NamespaceStore::create` stores the `DatabaseConfig` it is given (`handle.store(db_config)`)
before loading the namespace. So the configuration the meta store restored from disk is replaced
by `DatabaseConfig::default()` each time. Keeping the stored configuration when the default
namespace already exists in the meta store would fix it.

Found with hydo, a deterministic fault-testing tool (https://github.com/kakoc/hydo); the example is https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/libsql-replica.

## 3. TigerBeetle: no upstream bug

`hydo/onboard/examples/tigerbeetle/RESULTS.md` (M176, M179, M183). No acknowledged transfer was lost
and no balance went wrong on an honest disk, on one replica (0.17.9, 0.16.45, 0.16.29) or on three
replicas (0.17.9 in 8 GiB). Every failure needed a storage fault of the replica's own disk or file:
a lying or erroring disk, or (M183) zeroed, truncated or bit-flipped bytes of the replica's data
file. In those runs a single replica refused to start (`panic: superblock not found`, `panic: data
file inode size was truncated or corrupted`) instead of serving a damaged ledger.

That is TigerBeetle's documented behaviour, not a bug. Its
[safety documentation](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/concepts/safety.md),
"Storage Fault Tolerance", says that a record is lost only if it is corrupted on every replica of a
cluster, "and even in that case **the system would safely halt**". With one replica, that replica is
every replica. One hydo run zeroed the superblock under a running replica and the run passed: the
replica does not read its superblock again while it runs, so the damage shows only at the next
start. That is also within the documented model. The earlier "every acknowledged transfer lost"
failure of M176 was a fault in hydo's check, not in TigerBeetle. Nothing to report.

## 4. PostgreSQL: no upstream bug

`hydo/onboard/examples/postgres-sql/RESULTS.md` (M182). PostgreSQL lost acknowledged writes only
on `disk=lying`, a disk that acknowledges a flush and then drops the data. PostgreSQL documents
that it relies on the storage keeping what `fsync` reported as written
([WAL reliability](https://www.postgresql.org/docs/15/wal-reliability.html)), so no database can
keep acknowledged data on such a disk. The other failures were I/O-error disks under which
PostgreSQL does not start again, and one failed set-up of hydo's own network fault (`tc`). With an
honest disk, power cuts lost nothing, also with `synchronous_commit=off` at the cut points tried
(that setting may lose the most recent commits by design:
[asynchronous commit](https://www.postgresql.org/docs/15/wal-async-commit.html)). Nothing to
report.

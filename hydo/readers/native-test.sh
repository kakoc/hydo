#!/usr/bin/env bash
# Run hydo-read / hydo-check against REAL stores, natively (no VM), on the build box:
#   hydo/readers/native-test.sh            # from the repository root
# Starts MySQL 8.4.10, PostgreSQL 15.6, Kafka 3.5.0 (KRaft) and Redis 7.2.4 from the same
# pinned store paths the examples stage into their guests, and etcd 3.5.6 from the pinned
# release tarball (examples/etcd/build.sh), each on its own port in a fresh temp dir; runs the
# unit tests and the store tests (`go test -v`); stops everything and removes the temp dir.
# Ports (override with HYDO_TEST_PORTBASE, default 37000): +306 mysql, +432 postgres,
# +92/+93 kafka, +379 redis, +2379/+2380 etcd, +999 a port nothing listens on.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../.." && pwd)"
B="${HYDO_TEST_PORTBASE:-37000}"
MYSQL="${HYDO_MYSQL:-/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10}"
PG="${HYDO_PG:-/nix/store/6mr1dkqqgkm6lhnkvir9i1jim2dnrl8j-postgresql-15.6}"
JDK="${HYDO_KAFKA_JDK:-/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7}"
KP="${HYDO_KAFKA:-/nix/store/prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0}"
REDIS="${HYDO_REDIS_OUT:-/nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4}"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
ETCDV=3.5.6
ETCDSHA=4db32e3bc06dd0999e2171f76a87c1cffed8369475ec7aa7abee9023635670fb
ETCDTAR="${HYDO_ETCD_DL:-$HOME/hydo-etcd-dl/v$ETCDV}/etcd-v$ETCDV-linux-amd64.tar.gz"

T="$(mktemp -d /tmp/hydo-read-test.XXXXXX)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  sleep 2
  for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$T"
}
trap cleanup EXIT
wait_for() {   # <what> <seconds> <command...>
  local what=$1 n=$2; shift 2
  for _ in $(seq "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
  echo "native-test: $what did not come up; log tail:" >&2; tail -20 "$T/$what.log" >&2 || true; exit 1
}

# MySQL
mkdir -p "$T/mysql"
cat > "$T/my.cnf" <<EOF
[mysqld]
datadir=$T/mysql
socket=$T/mysql.sock
pid-file=$T/mysqld.pid
bind-address=127.0.0.1
port=$((B + 306))
mysqlx=OFF
skip-log-bin
performance_schema=OFF
innodb_buffer_pool_size=32M
innodb_redo_log_capacity=8M
EOF
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" --initialize-insecure --user="$(id -un)" > "$T/mysql.log" 2>&1
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" --user="$(id -un)" >> "$T/mysql.log" 2>&1 & PIDS+=($!)
wait_for mysql 120 "$MYSQL/bin/mysql" --no-defaults --protocol=TCP -h127.0.0.1 -P$((B + 306)) -uroot -e "SELECT 1"

# PostgreSQL
"$PG/bin/initdb" -D "$T/pg" -U postgres --auth=trust > "$T/postgres.log" 2>&1
"$PG/bin/postgres" -D "$T/pg" -c listen_addresses=127.0.0.1 -c port=$((B + 432)) -c unix_socket_directories="$T" >> "$T/postgres.log" 2>&1 & PIDS+=($!)
wait_for postgres 60 "$PG/bin/pg_isready" -h 127.0.0.1 -p $((B + 432)) -U postgres

# Kafka (KRaft, one node)
cat > "$T/kafka.properties" <<EOF
process.roles=broker,controller
node.id=1
controller.quorum.voters=1@127.0.0.1:$((B + 93))
listeners=PLAINTEXT://127.0.0.1:$((B + 92)),CONTROLLER://127.0.0.1:$((B + 93))
inter.broker.listener.name=PLAINTEXT
controller.listener.names=CONTROLLER
listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT
advertised.listeners=PLAINTEXT://127.0.0.1:$((B + 92))
log.dirs=$T/kafka-logs
num.partitions=1
offsets.topic.replication.factor=1
transaction.state.log.replication.factor=1
transaction.state.log.min.isr=1
auto.create.topics.enable=true
group.initial.rebalance.delay.ms=0
EOF
J=("$JDK/bin/java" -Xms256m -Xmx256m "-Dlog4j.configuration=file:$KP/config/log4j.properties" "-Dkafka.logs.dir=$T/kafka-applogs" -cp "$KP/libs/*")
"${J[@]}" kafka.tools.StorageTool format -t P0hH8j92QAKKXdQ2D7Aw7A -c "$T/kafka.properties" > "$T/kafka.log" 2>&1
"${J[@]}" kafka.Kafka "$T/kafka.properties" >> "$T/kafka.log" 2>&1 & PIDS+=($!)
wait_for kafka 120 grep -q "Kafka Server started" "$T/kafka.log"

# Redis
"$REDIS/bin/redis-server" --port $((B + 379)) --bind 127.0.0.1 --save "" --appendonly no --dir "$T" > "$T/redis.log" 2>&1 & PIDS+=($!)
wait_for redis 30 "$REDIS/bin/redis-cli" -p $((B + 379)) ping

# etcd
[ -f "$ETCDTAR" ] || { mkdir -p "$(dirname "$ETCDTAR")"; curl -sSfL -o "$ETCDTAR" "https://github.com/etcd-io/etcd/releases/download/v$ETCDV/etcd-v$ETCDV-linux-amd64.tar.gz"; }
echo "$ETCDSHA  $ETCDTAR" | sha256sum -c - > /dev/null
tar -xzf "$ETCDTAR" -C "$T"
"$T/etcd-v$ETCDV-linux-amd64/etcd" --data-dir "$T/etcd" --listen-client-urls http://127.0.0.1:$((B + 2379)) \
  --advertise-client-urls http://127.0.0.1:$((B + 2379)) --listen-peer-urls http://127.0.0.1:$((B + 2380)) \
  --initial-advertise-peer-urls http://127.0.0.1:$((B + 2380)) --initial-cluster default=http://127.0.0.1:$((B + 2380)) \
  > "$T/etcd.log" 2>&1 & PIDS+=($!)
wait_for etcd 30 curl -sf http://127.0.0.1:$((B + 2379))/health

export HYDO_TEST_MYSQL="root@tcp(127.0.0.1:$((B + 306)))/"
export HYDO_TEST_POSTGRES="postgres://postgres@127.0.0.1:$((B + 432))/postgres?sslmode=disable"
export HYDO_TEST_KAFKA="127.0.0.1:$((B + 92))"
export HYDO_TEST_REDIS="127.0.0.1:$((B + 379))"
export HYDO_TEST_ETCD="http://127.0.0.1:$((B + 2379))"
export HYDO_TEST_DEADPORT="127.0.0.1:$((B + 999))"
cd "$TOP/hydo/readers"
GOMODCACHE="$TOP/build/readers-gomod" GOCACHE="$TOP/build/readers-gocache" GOPATH="$TOP/build/readers-gopath" \
  GOTOOLCHAIN=local GOFLAGS=-mod=readonly CGO_ENABLED=0 "$GO" test -count=1 -v . 2>&1 | grep -E '^(=== RUN|--- |PASS|FAIL|ok|\s+\S+_test.go)'
echo "native-test: done"

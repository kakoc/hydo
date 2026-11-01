#!/usr/bin/env bash
# Run hydo-read's store tests (tests/stores.rs) against REAL stores, natively (no VM):
#   hydo/crates/hydo-read/native-test.sh                 cargo test --release --locked --test stores
#   hydo/crates/hydo-read/native-test.sh <command...>    anything else (a cross-built test binary,
#                                                        a comparison script), with the stores up
# Starts MySQL 8.4.10, PostgreSQL 15.6, Kafka 3.5.0 (KRaft, the pinned JDK) and Redis 7.2.4 from
# the pinned store paths the examples stage into their guests, and etcd 3.5.6 from its pinned
# release tarball, each on its own port in a fresh temp dir; exports HYDO_TEST_* for them; runs
# the command; stops everything and removes the temp dir. On a machine with these /nix/store
# paths (the build machine) it runs as it is; on the bare-metal machine, bare-metal.sh runs it in a chroot
# whose /nix/store holds the closures taken out of the recorded example images.
# Ports: HYDO_TEST_PORTBASE (default 37000) +306 mysql, +432 postgres, +92/+93 kafka, +379 redis,
# +2379/+2380 etcd, +999 a port nothing listens on. Paths: HYDO_MYSQL, HYDO_PG, HYDO_KAFKA_JDK,
# HYDO_KAFKA_LIBS, HYDO_REDIS_SERVER, HYDO_REDIS_CLI, HYDO_ETCD (the etcd binary).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
B="${HYDO_TEST_PORTBASE:-37000}"
MYSQL="${HYDO_MYSQL:-/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10}"
PG="${HYDO_PG:-/nix/store/6mr1dkqqgkm6lhnkvir9i1jim2dnrl8j-postgresql-15.6}"
JDK="${HYDO_KAFKA_JDK:-/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7}"
KLIBS="${HYDO_KAFKA_LIBS:-/nix/store/prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0/libs}"
RS="${HYDO_REDIS_SERVER:-/nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4/bin/redis-server}"
RC="${HYDO_REDIS_CLI:-$(dirname "$RS")/redis-cli}"
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
trap 'echo "native-test: failed at line $LINENO: $BASH_COMMAND" >&2' ERR
wait_for() {   # <what> <seconds> <command...>
  local what=$1 n=$2; shift 2
  for _ in $(seq "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
  echo "native-test: $what did not come up; log tail:" >&2; tail -20 "$T/$what.log" >&2 || true; exit 1
}

# MySQL (also mysql_native_password, for the plugin tests)
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
default-time-zone=+00:00
mysql_native_password=ON
EOF
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" --initialize-insecure --user="$(id -un)" > "$T/mysql.log" 2>&1
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" --user="$(id -un)" >> "$T/mysql.log" 2>&1 & PIDS+=($!)
wait_for mysql 120 "$MYSQL/bin/mysql" --no-defaults --protocol=TCP -h127.0.0.1 -P$((B + 306)) -uroot -e "SELECT 1"

# PostgreSQL
LANG=C LC_ALL=C "$PG/bin/initdb" -D "$T/pg" -U postgres --auth=trust > "$T/postgres.log" 2>&1
LANG=C LC_ALL=C "$PG/bin/postgres" -D "$T/pg" -c listen_addresses=127.0.0.1 -c port=$((B + 432)) -c unix_socket_directories="$T" >> "$T/postgres.log" 2>&1 & PIDS+=($!)
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
cat > "$T/log4j.properties" <<EOF
log4j.rootLogger=WARN, stdout
log4j.appender.stdout=org.apache.log4j.ConsoleAppender
log4j.appender.stdout.layout=org.apache.log4j.PatternLayout
log4j.appender.stdout.layout.ConversionPattern=%p %c %m%n
log4j.logger.kafka.server.KafkaRaftServer=INFO
EOF
J=("$JDK/bin/java" -Xms256m -Xmx256m "-Dlog4j.configuration=file:$T/log4j.properties" -Djava.io.tmpdir=/tmp -cp "$KLIBS/*")
"${J[@]}" kafka.tools.StorageTool format -t P0hH8j92QAKKXdQ2D7Aw7A -c "$T/kafka.properties" > "$T/kafka.log" 2>&1
"${J[@]}" kafka.Kafka "$T/kafka.properties" >> "$T/kafka.log" 2>&1 & PIDS+=($!)
wait_for kafka 120 grep -q "Kafka Server started" "$T/kafka.log"

# Redis
"$RS" --port $((B + 379)) --bind 127.0.0.1 --save "" --appendonly no --dir "$T" > "$T/redis.log" 2>&1 & PIDS+=($!)
wait_for redis 30 "$RC" -p $((B + 379)) ping

# etcd
ETCD="${HYDO_ETCD:-}"
if [ -z "$ETCD" ]; then
  [ -f "$ETCDTAR" ] || { mkdir -p "$(dirname "$ETCDTAR")"; curl -sSfL --max-time 300 -o "$ETCDTAR" "https://github.com/etcd-io/etcd/releases/download/v$ETCDV/etcd-v$ETCDV-linux-amd64.tar.gz"; }
  echo "$ETCDSHA  $ETCDTAR" | sha256sum -c - > /dev/null
  tar -xzf "$ETCDTAR" -C "$T"
  ETCD="$T/etcd-v$ETCDV-linux-amd64/etcd"
fi
"$ETCD" --data-dir "$T/etcd" --listen-client-urls http://127.0.0.1:$((B + 2379)) \
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
# for a comparison script that loads its own data: the store paths and the temp dir
export HYDO_CC_MYSQL="$MYSQL" HYDO_CC_PG="$PG" HYDO_CC_JAVA="$JDK/bin/java" HYDO_CC_KAFKA_LIBS="$KLIBS" HYDO_CC_TMP="$T"
echo "native-test: stores up"
if [ $# -gt 0 ]; then
  "$@"
else
  ( cd "$HERE" && cargo +1.90.0 test --release --locked --test stores -- --test-threads=1 )
fi
echo "native-test: done"

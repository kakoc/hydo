#!/bin/bash
# Inside the chroot: start MySQL 8.4.10, PostgreSQL 15.6, Kafka 3.5.0 (KRaft), Redis 7.2.4 and
# etcd 3.5.6 on ports 47000+ in a fresh temp dir (as hydo/readers/native-test.sh did), export
# HYDO_TEST_* for them, run "$@", stop everything.
set -euo pipefail
B=47000
MYSQL=/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10
PG=/nix/store/6mr1dkqqgkm6lhnkvir9i1jim2dnrl8j-postgresql-15.6
JAVA=/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7/bin/java
T="$(mktemp -d /tmp/hydo-read-cc.XXXXXX)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  sleep 2
  for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$T"
}
trap cleanup EXIT
trap 'echo "inside: failed at line $LINENO: $BASH_COMMAND" >&2; tail -5 "$T"/*.log >&2 2>/dev/null' ERR
wait_for() {
  local what=$1 n=$2; shift 2
  for _ in $(seq "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
  echo "inside: $what did not come up; log tail:" >&2; tail -20 "$T/$what.log" >&2 || true; exit 1
}
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
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" --initialize-insecure > "$T/mysql.log" 2>&1
"$MYSQL/bin/mysqld" --defaults-file="$T/my.cnf" >> "$T/mysql.log" 2>&1 & PIDS+=($!)
wait_for mysql 120 "$MYSQL/bin/mysql" --no-defaults --protocol=TCP -h127.0.0.1 -P$((B + 306)) -uroot -e "SELECT 1"
"$PG/bin/initdb" -D "$T/pg" -U postgres --auth=trust > "$T/postgres.log" 2>&1
"$PG/bin/postgres" -D "$T/pg" -c listen_addresses=127.0.0.1 -c port=$((B + 432)) -c unix_socket_directories="$T" >> "$T/postgres.log" 2>&1 & PIDS+=($!)
wait_for postgres 60 "$PG/bin/pg_isready" -h 127.0.0.1 -p $((B + 432)) -U postgres
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
J=("$JAVA" -Xms256m -Xmx256m "-Dlog4j.configuration=file:$T/log4j.properties" -Djava.io.tmpdir=/tmp -cp "/opt/kafka/libs/*")
"${J[@]}" kafka.tools.StorageTool format -t P0hH8j92QAKKXdQ2D7Aw7A -c "$T/kafka.properties" > "$T/kafka.log" 2>&1
"${J[@]}" kafka.Kafka "$T/kafka.properties" >> "$T/kafka.log" 2>&1 & PIDS+=($!)
wait_for kafka 120 grep -q "Kafka Server started" "$T/kafka.log"
/opt/redis/redis-server --port $((B + 379)) --bind 127.0.0.1 --save "" --appendonly no --dir "$T" > "$T/redis.log" 2>&1 & PIDS+=($!)
wait_for redis 30 /opt/redis/redis-cli -p $((B + 379)) ping
/opt/etcd/etcd --data-dir "$T/etcd" --listen-client-urls http://127.0.0.1:$((B + 2379)) \
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
export HYDO_CC_MYSQL="$MYSQL" HYDO_CC_PG="$PG" HYDO_CC_JAVA="$JAVA" HYDO_CC_TMP="$T"
echo "inside: stores up"
"$@"

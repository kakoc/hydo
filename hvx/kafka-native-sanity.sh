#!/usr/bin/env bash
# Native (on-the-remote-HOST) sanity for the M33 Kafka recipe: format a single-node
# KRaft store with a FIXED cluster UUID, start the broker, run KafkaClient (produce
# acks=all + consume + oracle), assert HYDO-ORACLE: OK. This validates the config +
# client FAST before the slow hvx L2 boot. NOT the deterministic path -- just a recipe
# check. Scoped kill: only the broker PID we start.
set -uo pipefail
JDK=/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7
KP=/nix/store/prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0
JAVA=$JDK/bin/java
JAVAC=$JDK/bin/javac
CID="hYd0KafkaC1uM33AAAAAAA"     # placeholder; replaced below with a valid UUID
W=$(mktemp -d /tmp/kafka-sanity.XXXXXX)
echo ">> workdir $W"
cd "$W"

# a VALID Kafka cluster UUID (base64url of 16 bytes). Generate once; we'll bake a fixed
# one into build-linux.sh, but for the native check just use the tool.
CID=$("$JAVA" -cp "$KP/libs/*" kafka.tools.StorageTool random-uuid 2>/dev/null | tail -1)
echo ">> cluster.id = $CID"

mkdir -p "$W/data/kafka-logs"
cat > "$W/server.properties" <<EOF
process.roles=broker,controller
node.id=1
controller.quorum.voters=1@127.0.0.1:9093
listeners=PLAINTEXT://127.0.0.1:9092,CONTROLLER://127.0.0.1:9093
inter.broker.listener.name=PLAINTEXT
controller.listener.names=CONTROLLER
listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT
advertised.listeners=PLAINTEXT://127.0.0.1:9092
log.dirs=$W/data/kafka-logs
num.partitions=1
default.replication.factor=1
offsets.topic.replication.factor=1
offsets.topic.num.partitions=1
transaction.state.log.replication.factor=1
transaction.state.log.min.isr=1
auto.create.topics.enable=true
num.network.threads=2
num.io.threads=2
log.index.size.max.bytes=1048576
log.segment.bytes=16777216
metadata.log.segment.bytes=8388608
group.initial.rebalance.delay.ms=0
EOF

echo ">> compile KafkaClient.java"
cp ~/hydo/hvx/KafkaClient.java .
"$JAVAC" -cp "$KP/libs/*" -d . KafkaClient.java || { echo "COMPILE FAIL"; exit 1; }

echo ">> format storage (fixed UUID)"
"$JAVA" -cp "$KP/libs/*" kafka.tools.StorageTool format -t "$CID" -c "$W/server.properties" --ignore-formatted 2>&1 | tail -3

echo ">> start broker"
"$JAVA" -Xmx320m -XX:+UseSerialGC -cp "$KP/libs/*" kafka.Kafka "$W/server.properties" > "$W/broker.log" 2>&1 &
BPID=$!
echo "broker pid=$BPID"

# wait for "Kafka Server started"
ok=0
for i in $(seq 1 60); do
  if grep -q "Kafka Server started" "$W/broker.log" 2>/dev/null; then ok=1; break; fi
  if ! kill -0 $BPID 2>/dev/null; then echo "BROKER DIED early"; break; fi
  sleep 1
done
echo ">> broker started=$ok after ${i}s"
grep -E "Kafka Server started|ERROR|Exception" "$W/broker.log" | head -8

if [ $ok -eq 1 ]; then
  echo ">> run client (produce acks=all + consume + oracle)"
  "$JAVA" -Xmx160m -XX:+UseSerialGC -cp ".:$KP/libs/*" KafkaClient 127.0.0.1:9092 hydotopic 30 2>&1 | grep -aE "HYDO-KAFKA|HYDO-ORACLE|hydo-kafka-client-done|Exception" | head -12
fi

echo ">> scoped kill broker $BPID"
kill -9 $BPID 2>/dev/null
sleep 1
rm -rf "$W"
echo ">> done"

#!/bin/sh
# start-broker.sh: start a broker of broker-a from /etc/rocketmq-hydo/broker.conf, with the
# values of the environment put in (the build parameters of hydo.yaml, or Compose's environment).
set -e
sed -e "s/@ADDR@/${BROKER_ADDR:?}/" \
    -e "s/@ALLACK@/${RMQ_ALLACK:-true}/" \
    -e "s/@FLUSH@/${RMQ_FLUSH:-SYNC_FLUSH}/" \
    -e "s/@MININSYNC@/${RMQ_MININSYNC:-1}/" \
    -e "s/@TXCHECK@/${RMQ_TXCHECK:-10000}/" \
    /etc/rocketmq-hydo/broker.conf > /tmp/broker.conf
cd "$ROCKETMQ_HOME/bin"
exec ./mqbroker -c /tmp/broker.conf

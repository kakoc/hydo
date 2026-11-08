# Sourced by the large-transaction test's commands and checks.
# q SQL: one statement through ysqlsh on yb-tserver-1, the value(s) printed bare.
q() { timeout 600 hydo-oci yb_tserver_1 /home/yugabyte/postgres/bin/ysqlsh -h yb-tserver-1 -U yugabyte -X -q -At -v ON_ERROR_STOP=1 -c "$1"; }
# admin ARGS: yb-admin against the three masters.
admin() { timeout 600 hydo-oci yb_tserver_1 /home/yugabyte/bin/yb-admin -master_addresses yb-master-1:7100,yb-master-2:7100,yb-master-3:7100 "$@"; }
# wait_ysql SECONDS: until YSQL answers (guest time).
wait_ysql() { i=0; while [ "$i" -lt "$1" ]; do q "SELECT 1" >/dev/null 2>&1 && return 0; sleep 2; i=$((i + 2)); done; return 1; }

#!/bin/sh
# Creates Temporal's two PostgreSQL databases and their schemas with the admin-tools image's
# temporal-sql-tool, as temporalio/docker-compose's auto-setup image did up to 1.29 (that image
# is not published for later versions). Safe to run again: an existing database or schema is
# left as it is, and update-schema applies only the versions that are missing.
set -eu
SQL="temporal-sql-tool --plugin postgres12 --ep ${POSTGRES_SEEDS:-postgresql} -p ${DB_PORT:-5432} -u ${POSTGRES_USER:-temporal} -pw ${POSTGRES_PWD:-temporal}"
S=/etc/temporal/schema/postgresql/v12
for db in temporal:temporal temporal_visibility:visibility; do
  name=${db%%:*} dir=${db#*:}
  $SQL --db "$name" create-database 2>/dev/null || true
  $SQL --db "$name" setup-schema -v 0.0 2>/dev/null || true
  $SQL --db "$name" update-schema -d "$S/$dir/versioned"
done
echo "temporal-setup: schemas ready"

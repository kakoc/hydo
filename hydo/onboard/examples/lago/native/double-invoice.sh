#!/bin/sh
# Finding 8: a subscription whose cancellation is not committed yet at the hourly billing run is
# invoiced twice for one period. Starts Lago v1.57.0 with the holding proxy (compose.yaml and
# compose.hold.yaml) as the Docker Compose project hydo-native-lago-dbl, and removes it at the end
# with its volumes and the proxy's image.
#   sh double-invoice.sh <time zone> <start HH:MM:SS UTC>
#   sh double-invoice.sh Atlantic/Azores 00:56:00       # the run of 2026-11-14 (UTC-1 in November)
# The start must be 4 minutes before midnight in the time zone, and at least 5 minutes from now.
# The script makes five customers in that time zone with monthly anniversary subscriptions whose
# period ends at that midnight. held1, held2 and held3 are cancelled at the start with the
# database's answers held for 16 minutes, past the billing run at minute 10. ctrl1 is cancelled
# 60 s later without a hold, and plain1 is not cancelled. It prints every customer's invoices.
# Takes about 25 minutes after the start. KEEP=1 leaves the project running.
# Needs Docker with Compose v2, curl and python3. The API listens on 127.0.0.1:13000.
set -u
cd "$(dirname "$0")"
TZNAME=${1:?usage: sh double-invoice.sh <time zone> <start HH:MM:SS UTC>}
START=${2:?usage: sh double-invoice.sh <time zone> <start HH:MM:SS UTC>}
export COMPOSE_PROJECT=hydo-native-lago-dbl
P="docker compose --progress quiet -p $COMPOSE_PROJECT -f compose.yaml -f compose.hold.yaml"
L="python3 -I lago.py"

# The seconds until the start, and the subscription date: noon in the time zone, one month
# before the local date that begins 4 minutes after the start.
eval "$(python3 -I - "$TZNAME" "$START" <<'EOF'
import datetime as dt, sys, zoneinfo
tz = zoneinfo.ZoneInfo(sys.argv[1])
now = dt.datetime.now(dt.timezone.utc)
h, m, s = map(int, sys.argv[2].split(":"))
at = now.replace(hour=h, minute=m, second=s, microsecond=0)
if at < now:
    at += dt.timedelta(days=1)
day = (at + dt.timedelta(minutes=4)).astimezone(tz)
if (day.hour, day.minute) != (0, 0):
    sys.exit(f"double-invoice.sh: {sys.argv[2]} UTC is {at.astimezone(tz):%H:%M} in {sys.argv[1]}, not 23:56")
year, month = (day.year, day.month - 1) if day.month > 1 else (day.year - 1, 12)
noon = dt.datetime(year, month, day.day, 12, tzinfo=tz).astimezone(dt.timezone.utc)
print(f"WAIT={int((at - now).total_seconds())}; SUB_AT={noon:%Y-%m-%dT%H:%M:%SZ}")
EOF
)" || exit 1
[ "$WAIT" -ge 300 ] || { echo "double-invoice.sh: the start is $WAIT s away; give one at least 5 minutes away" >&2; exit 1; }
echo "start in $WAIT s; subscriptions from $SUB_AT, time zone $TZNAME"

cleanup() { [ "${KEEP:-0}" = 1 ] || $P down -v --rmi local --remove-orphans >/dev/null 2>&1; }
trap cleanup EXIT INT TERM
$P up -d --build >/dev/null || exit 1
i=0
until curl -sf http://127.0.0.1:13000/health >/dev/null 2>&1; do
  i=$((i + 1)); [ $i -gt 150 ] && { echo "double-invoice.sh: the API did not start" >&2; exit 1; }; sleep 2
done
CUSTOMERS="held1 held2 held3 ctrl1 plain1"
$L seed dbl anniversary "$SUB_AT" $CUSTOMERS || exit 1
# The API accepts a customer's time zone only with a license, so the script sets it in the
# database.
$P exec -T db psql -U lago -d lago -c \
  "UPDATE customers SET timezone = '$TZNAME' WHERE external_id IN ('held1','held2','held3','ctrl1','plain1')" || exit 1
python3 -I experiment.py "$START" 960 held1 held2 held3 -- ctrl1
sleep 120
$L invoices $CUSTOMERS

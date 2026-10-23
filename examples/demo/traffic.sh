#!/usr/bin/env bash
# Random traffic for the demo stack: picks a service, sends one request through its sds
# proxy port and waits a little. Hot services appear several times in the pool, so they
# stay awake while the cold ones keep falling asleep and waking up.
set -euo pipefail

# name:proxy-port, repeated entries raise the hit rate
pool=(
  api:18201 api:18201 api:18201 api:18201
  web:18202 web:18202 web:18202
  shop:18203 shop:18203
  blog:18204 blog:18204
  docs:18205
  wiki:18206
  metrics:18207 metrics:18207
  mail:18208
  chat:18209
  cdn:18210
)

echo "sending random requests through the sds proxies (Ctrl-C to stop)"
while true; do
  pick=${pool[RANDOM % ${#pool[@]}]}
  name=${pick%%:*}
  port=${pick##*:}
  # curl measures the round trip itself; a wake shows up as a request taking a second or more
  result=$(curl -s -o /dev/null -w '%{http_code} %{time_total}s' --max-time 120 "http://127.0.0.1:${port}/" || echo "000 -")
  printf '%s  %-8s %s\n' "$(date +%H:%M:%S)" "$name" "$result"
  sleep "$((RANDOM % 4)).$((RANDOM % 10))"
done

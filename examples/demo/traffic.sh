#!/usr/bin/env bash
# Random traffic for the demo stack: picks a service, sends one request through the path
# sds watches for it and waits a little. Hot services appear several times in the pool, so
# they stay awake while the cold ones keep falling asleep and waking up.
#
#   http  native sds HTTP proxy (1820x)
#   caddy Caddy with forward-auth to sds (1830x)
#   tcp   native sds TCP proxy in front of Redis (18211), a PING via bash /dev/tcp
set -euo pipefail

# name:kind:port, repeated entries raise the hit rate
pool=(
  api:http:18201 api:http:18201 api:http:18201 api:http:18201
  web:http:18202 web:http:18202 web:http:18202
  shop:http:18203 shop:http:18203
  blog:http:18204 blog:http:18204
  docs:caddy:18305 docs:caddy:18305
  wiki:caddy:18306
  metrics:caddy:18307 metrics:caddy:18307
  mail:http:18208
  chat:http:18209
  cdn:http:18210
  cache:tcp:18211 cache:tcp:18211
)

# One Redis PING through the TCP proxy. sds holds the connection while Redis wakes, so
# the reply can take a second or more; prints "PONG <seconds>s" or "ERR -".
redis_ping() {
  local port=$1 start reply
  start=$(date +%s)
  if { exec 3<>"/dev/tcp/127.0.0.1/${port}"; } 2>/dev/null; then
    printf 'PING\r\n' >&3
    IFS= read -r -t 120 reply <&3 2>/dev/null || reply=""
    exec 3<&- 3>&-
  fi
  reply=${reply:-}
  reply=${reply%$'\r'}
  if [[ $reply == "+PONG" ]]; then
    echo "PONG $(($(date +%s) - start))s"
  else
    echo "ERR -"
  fi
}

echo "sending random requests through sds and Caddy (Ctrl-C to stop)"
while true; do
  pick=${pool[RANDOM % ${#pool[@]}]}
  name=${pick%%:*}
  rest=${pick#*:}
  kind=${rest%%:*}
  port=${rest##*:}
  if [[ $kind == tcp ]]; then
    result=$(redis_ping "$port")
  else
    # curl measures the round trip itself; a wake shows up as a request taking a second or more
    result=$(curl -s -o /dev/null -w '%{http_code} %{time_total}s' --max-time 120 "http://127.0.0.1:${port}/" || echo "000 -")
  fi
  printf '%s  %-8s %-5s %s\n' "$(date +%H:%M:%S)" "$name" "$kind" "$result"
  sleep "$((RANDOM % 4)).$((RANDOM % 10))"
done

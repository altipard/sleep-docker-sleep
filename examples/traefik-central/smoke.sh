#!/usr/bin/env bash
# Real Traefik forward-auth contract test: token injection, sleep/wake and token stripping.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"

SDS=target/debug/sds
CFG=target/traefik-e2e-sds.toml
NETWORK=sds-traefik-e2e
APP=sds-traefik-app
PROXY=sds-traefik-proxy
PID=''

cleanup() {
  kill "$PID" 2>/dev/null || true
  docker rm -f "$APP" "$PROXY" >/dev/null 2>&1 || true
  docker network rm "$NETWORK" >/dev/null 2>&1 || true
}
trap 'status=$?; if [ $status -ne 0 ]; then docker logs "$PROXY" 2>&1 || true; fi; cleanup; exit $status' EXIT
cleanup 2>/dev/null || true
PID=''

printf '[admin]\nbind = "0.0.0.0:9902"\ntoken = "e2e-secret"\n' > "$CFG"
"$SDS" integrate traefik e2e \
  --host e2e.example.com \
  --upstream http://app:80 \
  --sds-url http://host.docker.internal:9902 \
  --config "$CFG" > target/traefik-e2e-full.txt
awk '/# --- or as container labels/{exit} {print}' target/traefik-e2e-full.txt > target/traefik-e2e.yml

docker network create "$NETWORK" >/dev/null
docker run -d --name "$APP" --network "$NETWORK" --network-alias app \
  -p 127.0.0.1:18082:80 \
  -l sds.enable=true -l sds.group=e2e -l sds.start_grace=0s \
  -l sds.health=http://127.0.0.1:18082/ \
  traefik/whoami:v1.10@sha256:1699d99cb4b9acc17f74ca670b3d8d0b7ba27c948b3445f0593b58ebece92f04 >/dev/null

"$SDS" run --headless --config "$CFG" > target/traefik-e2e-sds.log 2>&1 &
PID=$!
sleep 2

docker run -d --name "$PROXY" --network "$NETWORK" \
  -p 127.0.0.1:18083:80 \
  --add-host host.docker.internal:host-gateway \
  -v "$PWD/target/traefik-e2e.yml:/etc/traefik/dynamic.yml:ro" \
  traefik:v3.3@sha256:2cd5cc75530c8d07ae0587c743d23eb30cae2436d07017a5ff78498b1a43d09f \
  --providers.file.filename=/etc/traefik/dynamic.yml \
  --entrypoints.web.address=:80 >/dev/null
sleep 3

curl -fsS --max-time 20 -H 'Host: e2e.example.com' http://127.0.0.1:18083/ > target/traefik-e2e-body.txt
! grep -qi 'X-SDS-Token' target/traefik-e2e-body.txt

"$SDS" sleep e2e --config "$CFG" >/dev/null
sleep 2
test "$(docker inspect -f '{{.State.Status}}' "$APP")" = exited
curl -fsS --max-time 30 -H 'Host: e2e.example.com' http://127.0.0.1:18083/ > target/traefik-e2e-wake-body.txt
! grep -qi 'X-SDS-Token' target/traefik-e2e-wake-body.txt
test "$(docker inspect -f '{{.State.Status}}' "$APP")" = running

printf 'Traefik E2E: authenticated request PASS\n'
printf 'Traefik E2E: wake after sleep PASS\n'
printf 'Traefik E2E: token stripped before upstream PASS\n'

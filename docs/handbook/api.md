# HTTP API

Base: `http://<admin.bind>` (default `http://127.0.0.1:9900`). With `admin.token` set,
`/api/*` and `/_sds/check/*` require the header `X-SDS-Token: <token>` or
`Authorization: Bearer <token>`; otherwise `401`.

## Overview

| Method | Path | Response |
|---|---|---|
| GET | `/healthz` | `ok` (text) |
| GET | `/` | web overview (HTML) |
| GET | `/_sds/check/<group>` | forward-auth, see [Reverse proxy](reverse-proxy.md) |
| GET | `/_sds/wait/<group>` | waiting page (HTML), `503` while not `running`, `404` for an unknown group |
| GET | `/api/v1/status` | overall state (JSON) |
| GET | `/api/v1/groups/<group>` | one group (JSON), `404` if unknown |
| POST | `/api/v1/groups/<group>/wake` | `202` immediately; with `?wait=true` blocking: `200` ready, `502` start failed, `504` timeout, `503` overloaded |
| POST | `/api/v1/groups/<group>/sleep` | `202` |
| GET | `/api/v1/events` | server-sent events: `status` (every 2 s and on every event) and `event` |

## `GET /api/v1/status`

```json
{
  "version": "0.1.0",
  "now_ms": 1792694670391,
  "uptime_ms": 47923,
  "docker": "unix:///var/run/docker.sock",
  "last_discovery_ms": 1792694652487,
  "warnings": [],
  "groups": [
    {
      "name": "shop",
      "state": "running",
      "state_since_ms": 1792694670314,
      "last_activity_ms": 1792694670323,
      "inflight": 0,
      "requests_total": 5,
      "wakes_total": 2,
      "sleeps_total": 1,
      "members": [
        { "name": "shop-app-1", "id": "73b46efff4f4", "running": true, "health": "healthy" }
      ],
      "last_error": null,
      "warnings": [],
      "source": "labels",
      "settings": {
        "idle_timeout": "15m",
        "start_timeout": "1m 30s",
        "stop_timeout": "20s",
        "start_grace": "5s",
        "health": "docker",
        "wake": "hold",
        "hold_timeout": "1m",
        "max_waiters": 200
      }
    }
  ],
  "events": [
    { "ts_ms": 1792694670314, "group": "shop", "kind": "waking", "message": "starting 1 container(s)" }
  ]
}
```

Times are Unix milliseconds. The remaining time until sleep is
`last_activity_ms + idle_timeout - now_ms` (only while `state = running`).

`state`: `stopped`, `starting`, `running`, `stopping`, `failed`.
A member's `health`: `none`, `starting`, `healthy`, `unhealthy`.
`source`: `labels` or `config`.
An event's `kind`: `discovered`, `removed`, `waking`, `running`, `start_failed`,
`sleeping`, `stopped`, `stop_failed`, `external`, `warning`.

## Examples

```bash
# all sleeping groups
curl -s http://127.0.0.1:9900/api/v1/status | jq -r '.groups[] | select(.state=="stopped") | .name'

# wake a group and wait
curl -s -X POST 'http://127.0.0.1:9900/api/v1/groups/shop/wake?wait=true' | jq .result

# live events
curl -N http://127.0.0.1:9900/api/v1/events
```

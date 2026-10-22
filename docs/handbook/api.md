# HTTP-API

Basis: `http://<admin.bind>` (Standard `http://127.0.0.1:9900`). Mit gesetztem
`admin.token` verlangen `/api/*` und `/_sds/check/*` den Header `X-SDS-Token: <token>`
oder `Authorization: Bearer <token>`; sonst `401`.

## Übersicht

| Methode | Pfad | Antwort |
|---|---|---|
| GET | `/healthz` | `ok` (Text) |
| GET | `/` | Web-Übersicht (HTML) |
| GET | `/_sds/check/<gruppe>` | Forward-Auth, siehe [Reverse-Proxy](reverse-proxy.md) |
| GET | `/_sds/wait/<gruppe>` | Warteseite (HTML), `503` solange nicht `running`, `404` bei unbekannter Gruppe |
| GET | `/api/v1/status` | Gesamtzustand (JSON) |
| GET | `/api/v1/groups/<gruppe>` | Eine Gruppe (JSON), `404` wenn unbekannt |
| POST | `/api/v1/groups/<gruppe>/wake` | `202` sofort; mit `?wait=true` blockierend: `200` bereit, `502` Start fehlgeschlagen, `504` Timeout, `503` überlastet |
| POST | `/api/v1/groups/<gruppe>/sleep` | `202` |
| GET | `/api/v1/events` | Server-Sent Events: `status` (alle 2 s und bei jedem Ereignis) und `event` |

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

Zeiten sind Unix-Millisekunden. Die verbleibende Zeit bis zum Einschlafen ergibt sich
aus `last_activity_ms + idle_timeout - now_ms` (nur bei `state = running`).

`state`: `stopped`, `starting`, `running`, `stopping`, `failed`.
`health` eines Mitglieds: `none`, `starting`, `healthy`, `unhealthy`.
`source`: `labels` oder `config`.
`kind` eines Ereignisses: `discovered`, `removed`, `waking`, `running`, `start_failed`,
`sleeping`, `stopped`, `stop_failed`, `external`, `warning`.

## Beispiele

```bash
# alle schlafenden Gruppen
curl -s http://127.0.0.1:9900/api/v1/status | jq -r '.groups[] | select(.state=="stopped") | .name'

# Gruppe wecken und warten
curl -s -X POST 'http://127.0.0.1:9900/api/v1/groups/shop/wake?wait=true' | jq .result

# Ereignisse live
curl -N http://127.0.0.1:9900/api/v1/events
```

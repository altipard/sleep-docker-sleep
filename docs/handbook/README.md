# sleep-docker-sleep (`sds`) – Handbook

`sds` stops Docker containers that have not received requests for a while and starts them
again as soon as the next request arrives. The first caller waits briefly or sees a
waiting page; after that everything works as usual. The goal: free resources on hosts
with many rarely used stacks without anyone having to start things by hand.

This handbook is written for administrators who set up and operate `sds`. Developers will
find a chapter on the [architecture](architecture.md) at the end.

## How it works

```text
                       1. request                     3. answer "200, go ahead"
  Browser ──────────▶ reverse proxy ──── 2. forward-auth ────▶ sds ─────┐
                      (nginx/Caddy/      GET /_sds/check/shop            │ starts the
                       Traefik)                                          │ containers
                          │ 4. request to the app                        ▼ when needed
                          └────────────────────────────────▶ container group "shop"
                                                             (app, db, …)
```

1. Before every request the reverse proxy asks `sds` (forward-auth, a request that takes
   microseconds while the group is running).
2. If the group is running, `sds` answers `200` immediately and records the activity.
3. If the group is asleep, `sds` starts the containers and either holds the request until
   the group is healthy (`wake = hold`) or returns a waiting page right away that reloads
   itself (`wake = page`).
4. If no request arrives for `idle_timeout`, `sds` stops the group.

`sds` is therefore a **control plane**: the actual traffic keeps flowing through the
existing reverse proxy. If `sds` goes down, traffic through nginx continues unchanged
(fail-open); only putting groups to sleep pauses. Caddy and Traefik have no bypass for
forward-auth; there a supervised service with automatic restart bridges the gap (see
[Limitations](limitations.md)).

For stacks without any reverse proxy and for plain TCP services there is also a
[native proxy](native-proxy.md), where `sds` itself owns the public port.

## Terms

| Term | Meaning |
|---|---|
| **Group** | A set of containers that sleep and wake together. Default: one Compose project. |
| **Member** | A container in a group (`sds.enable=true`). |
| **keep_awake** | A container in the same project that `sds` never stops (e.g. the project's own Caddy). |
| **Forward-auth** | The mechanism by which the reverse proxy asks `sds` before every request. |
| **hold / page** | Behaviour while waking up: hold the request, or serve a waiting page. |
| **Health** | The criterion for when a started group receives traffic (Docker HEALTHCHECK, HTTP, TCP or a grace period). |

## What `sds` is not

- Not a replacement for a reverse proxy in production. The native proxy is for special cases.
- Not an orchestrator: `sds` starts and stops existing containers, it does not create any.
- Not an autoscaler: there is only "on" and "off", no replicas.

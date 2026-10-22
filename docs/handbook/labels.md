# Labels and groups

Containers describe themselves how `sds` should treat them. Project teams only have to put
labels into their Compose file; no central list is needed.

## Forming groups

A group is the unit that sleeps and wakes together. Its name comes from:

1. `sds.group`, if set,
2. otherwise the Compose project (`com.docker.compose.project`, usually the directory name),
3. otherwise the container name.

All containers with `sds.enable=true` and the same group name form one group, even across
Compose projects. The name must be URL-safe: no whitespace, no `/`.

Typical patterns:

| Situation | Labels |
|---|---|
| Host nginx in front of the project, everything may sleep | `sds.enable=true` on all services |
| Project brings its own Caddy/Traefik | `sds.enable=true` on app and db, `sds.keep_awake=true` on the proxy |
| Two projects share a database | `sds.group=shared-db` on all participants |
| Only the app should sleep, the db keeps running | `sds.enable=true` only on the app |

When waking, `sds` starts all members in alphabetical order and then waits for the health
check. Compose dependencies (`depends_on`) are not evaluated; applications that cannot
cope with a database that is not ready yet need a restart policy
(`restart: unless-stopped`) or a HEALTHCHECK that `sds` waits for.

## Reference

| Label | Values | Description |
|---|---|---|
| `sds.enable` | `true`/`false` | The container becomes a member of its group. Without this label `sds` ignores the container. |
| `sds.keep_awake` | `true` | The container is never stopped by `sds`. Documents intent, e.g. on the project's own proxy. |
| `sds.group` | name | Explicit group instead of the Compose project. |
| `sds.idle_timeout` | duration | See [Configuration](configuration.md#defaults-and-group-settings). |
| `sds.start_timeout` | duration | |
| `sds.stop_timeout` | duration | |
| `sds.start_grace` | duration | |
| `sds.health` | `docker`, `grace`, `http://…`, `tcp://host:port` | |
| `sds.wake` | `hold`, `page` | |
| `sds.hold_timeout` | duration | |
| `sds.max_waiters` | number | |
| `sds.proxy.listen` | address | Native proxy, see [its chapter](native-proxy.md). |
| `sds.proxy.upstream` | `host:port` | |
| `sds.proxy.mode` | `http`, `tcp` | |

Unknown `sds.*` labels are errors: the container is ignored and `sds check` names the
label. This prevents a typo like `sds.idle_timout` from silently activating the default.

Group-wide settings (`idle_timeout`, `health`, …) sensibly live on one container. If they
are on several and contradict each other, the alphabetically first container name wins;
the web overview shows a warning.

## Choosing a health check

| Goal | Recommendation |
|---|---|
| Image has a usable HEALTHCHECK | `sds.health=docker` (default). `sds` waits until all members with a HEALTHCHECK report `healthy`. |
| No HEALTHCHECK, HTTP endpoint available | `sds.health=http://127.0.0.1:18080/healthz` (address as seen from the `sds` process). |
| TCP service (database, SMTP) | `sds.health=tcp://127.0.0.1:5432` |
| None of the above | `sds.health=grace` and a generous `sds.start_grace`. |

If a member crashes while starting (`exited`), `sds` aborts immediately and reports the
group as `failed` instead of waiting until `start_timeout`.

## Example: Compose project with its own Caddy

```yaml
services:
  caddy:
    image: caddy:2-alpine
    ports: ["80:80", "443:443"]
    extra_hosts: ["host.docker.internal:host-gateway"]
    labels:
      sds.keep_awake: "true"
  app:
    image: registry.example.com/shop:1.4.2
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.idle_timeout: "20m"
      sds.health: "docker"
  db:
    image: postgres:16
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.stop_timeout: "60s"
```

After `docker compose up -d` the group `shop` appears in `sds status`. Caddy keeps
running and asks `sds` via `host.docker.internal:9900`, see
[Reverse proxy integration](reverse-proxy.md).

## Interaction with restart policies

A `docker stop` by `sds` does not restart a container with `restart: always` or
`restart: unless-stopped`; Docker remembers the manual stop. Two peculiarities:

- After a restart of the Docker daemon (or the host), `restart: always` brings stopped
  containers back. `sds` detects that and lists the group as `running`; it goes back to
  sleep after `idle_timeout`.
- If a running application crashes and Docker restarts it via the restart policy, `sds`
  briefly sees `stopped`, then `running` again. This is intended and visible in the event
  log.

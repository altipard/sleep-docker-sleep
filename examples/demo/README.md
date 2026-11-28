# Demo: three ways sds sees traffic, short timeouts, random traffic

A playground to watch `sds` work: eleven groups with idle timeouts between 20 and 60
seconds and a script that sends random requests. Within a minute the dashboard shows
groups going to sleep, waking up on a request and serving again. The groups cover the
three ways `sds` sees traffic:

| Groups | How `sds` sees the request | Ports on 127.0.0.1 |
|---|---|---|
| `api` `web` `shop` `blog` `mail` `chat` `cdn` | native HTTP proxy (`sds.proxy.mode=http`) in front of a [whoami](https://github.com/traefik/whoami) server | proxy 1820x, app 1810x |
| `docs` `wiki` `metrics` | forward-auth: a Caddy container asks `sds` via `/_sds/check/<group>` before every request, the production setup | Caddy 18305-18307 |
| `cache` | native TCP proxy (`sds.proxy.mode=tcp`) in front of Redis | proxy 18211, Redis 18111 |

```bash
cd examples/demo
docker compose up -d        # eleven services plus Caddy, all ports on loopback
sds run -c sds.toml         # in a second terminal: dashboard with the eleven groups
./traffic.sh                # in a third terminal: random requests, Ctrl-C to stop
```

`sds run -c sds.toml` uses the demo config next to this file and leaves your own
`sds.toml` alone. It puts the admin API on `127.0.0.1:19900` instead of `9900`, so it
does not collide with a daemon you already run; query it with `sds -c sds.toml status`.
Add `--headless` to log to stderr instead of showing the dashboard.

`traffic.sh` sends HTTP requests through the sds proxy ports and through Caddy, and a
Redis `PING` through the TCP proxy using bash's `/dev/tcp`, so it needs only bash and
curl. Services that appear several times in its pool (`api`, `web`) rarely sleep; `cdn`
is hit so rarely that it is asleep most of the time. `blog` and `wiki` use
`sds.wake=page`: the first request after a nap gets the waiting page (HTTP 503) instead
of being held, for `wiki` served through Caddy. The other groups hold the request until
the container is ready; a `PING` to a sleeping Redis returns `PONG` after the wake.

## Forward-auth through Caddy

[`Caddyfile`](Caddyfile) holds three sites generated with `sds integrate caddy`, one per
group, for example:

```bash
sds -c sds.toml integrate caddy docs --host :8080 --upstream http://docs:80 \
    --sds-url http://host.docker.internal:19900
```

Caddy runs in the compose stack and reaches the daemon on the host as
`host.docker.internal:19900`. `docs`, `wiki` and `metrics` publish no port of their own,
so Caddy is the only way in; they use `sds.health=grace` because `sds` cannot probe them
from the host.

**Caddy fails closed.** As long as `sds` is not running, Caddy answers `502` for these
three groups (the native proxy ports are not open either). Start the stack and `sds` in
either order; Caddy needs no restart once `sds` is up. See
[Reverse proxy integration](../../docs/handbook/reverse-proxy.md#caddy).

### The demo token

`sds.toml` sets `admin.token = "demo-only-not-a-secret"` and the Caddyfile sends it as
`X-SDS-Token`. It is a public placeholder for this demo only, so the same files work on
macOS and Linux. If you reuse these files for anything else, replace it in both files
with a real secret.

### macOS (Docker Desktop) and Linux

- **Docker Desktop** forwards `host.docker.internal` from containers to the Mac's
  `127.0.0.1`, so the admin API stays on loopback: `sds run -c sds.toml`.
- **Linux**: `host.docker.internal` comes from `extra_hosts: host-gateway` in
  `compose.yaml` and points at the `docker0` bridge (`172.17.0.1` by default), so `sds`
  has to listen there:

  ```bash
  sds run -c sds.toml --bind 172.17.0.1:19900
  export SDS_URL=http://172.17.0.1:19900   # or --url on each command
  sds -c sds.toml status
  ```

  All client commands (`status`, `wake`, `sleep`, `hold`, `release`, `top`) derive the
  daemon address from `admin.bind` in `sds.toml`, which still says loopback, so they need
  `SDS_URL` or `--url` on Linux. `docker0` only exists while Docker is running, so start
  Docker before `sds`. `sds` allows the non-loopback bind because a token is set.
  On this bind every container on the host can reach the admin API, and the token is
  public, so run the demo only on a machine whose containers you trust. A host firewall
  (ufw, firewalld) may block containers from reaching `172.17.0.1:19900`; if Caddy
  answers `502` while `sds` runs, allow that port from the Docker bridge. Check the bridge address with
  `ip -4 addr show docker0` if it differs.

## TCP proxy and Redis

The TCP proxy accepts the connection, wakes `cache` and connects to Redis only once it
is ready. Redis uses a Docker `HEALTHCHECK` (`redis-cli ping`) with `sds.health=docker`
instead of a `tcp://` probe: Docker's port forwarder accepts connections on 18111 before
Redis listens, so a TCP probe would report the group ready too early and the first
client would be disconnected.

A wake takes one to two seconds here: `whoami` starts instantly, `sds.start_grace` is
1 s and Redis is healthy within a second or two. Real applications take longer; raise
`sds.start_timeout` accordingly.

Clean up with `docker compose down` and stop `sds run` with Ctrl-C.

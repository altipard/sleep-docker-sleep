<p align="center">
  <img src="assets/logo/sds-logo.svg" alt="sleep-docker-sleep logo: the Docker whale wearing a nightcap" width="180">
</p>

<h1 align="center">sleep-docker-sleep</h1>

<p align="center">
  Scale idle Docker containers to zero and wake them on the first request.<br>
  One Rust binary, no changes to your applications, works with the reverse proxy you already run.
</p>

---

`sds` watches the containers on a host. When a group of containers (a Compose project by
default) has not seen a request for a while, `sds` stops it. The next request starts it
again; the caller either waits a moment or sees a small "waking up" page. Useful for hosts
that run many rarely used stacks: staging environments, customer demos, internal tools,
side projects.

```text
browser ──▶ reverse proxy ──(forward-auth: /_sds/check/shop)──▶ sds ──▶ docker start / stop
                 │                                                            │
                 └──────────────── request ────────────────▶ container group "shop" ◀┘
```

## Highlights

- **Control plane only.** nginx, Caddy or Traefik ask `sds` before each request via their
  built-in forward-auth. `sds` never carries application traffic, so it cannot become a
  bottleneck, and with nginx a crash of `sds` fails open.
- **Label driven.** Containers opt in with `sds.enable=true`; a Compose project forms a
  group automatically. No central inventory to maintain, one daemon per host.
- **Graceful wake-up.** Requests are held until the group is healthy (Docker HEALTHCHECK,
  HTTP or TCP probe), or a self-refreshing waiting page is served. Bounded waiters, one
  start per group, no thundering herd.
- **Native proxy as a fallback.** For stacks without a reverse proxy and for TCP services
  such as databases, `sds` can own the public port itself (streaming, WebSocket aware).
- **Operator tooling included.** CLI, live terminal dashboard (`sds top` attaches to the
  running service), web overview with event log, JSON API, generated proxy snippets,
  systemd/launchd units.

## Quick start

1. Install the binary (see [Installation](docs/handbook/installation.md)):

   ```bash
   cargo install --git https://github.com/altipard/sleep-docker-sleep   # or grab a release binary
   sds check                                                        # verifies Docker access
   ```

2. Label a project and publish its port on loopback only:

   ```yaml
   services:
     app:
       image: ghcr.io/example/app:1.2.3
       ports: ["127.0.0.1:18080:8080"]
       labels:
         sds.enable: "true"
         sds.idle_timeout: "15m"
         sds.health: "http://127.0.0.1:18080/healthz"
   ```

3. Run the daemon and wire up the proxy:

   ```bash
   sds run                                       # dashboard; `sds service install` for production
   sds integrate nginx myproject --host app.example.com --upstream http://127.0.0.1:18080
   ```

   The command prints a ready-to-use `server` block (Caddyfile block or Traefik middleware
   for the others). Reload the proxy, done. `sds sleep myproject` and a browser reload show
   the whole cycle.

Full documentation lives in the [handbook](docs/handbook/README.md): installation,
configuration reference, labels, per-proxy integration notes, native proxy, operations,
troubleshooting, HTTP API, limitations, architecture.

## How it compares

| | sleep-docker-sleep | Sablier | Lazytainer |
|---|---|---|---|
| Integration | forward-auth of nginx/Caddy/Traefik, or native proxy | proxy plugins (Traefik, Caddy, nginx, …) | own proxy / iptables |
| Discovery | container labels, Compose project = group | proxy configuration per route | labels |
| Wake behaviour | hold or waiting page, bounded waiters | waiting page themes | hold |
| Health gating | Docker HEALTHCHECK, HTTP, TCP, grace | container running | TCP |
| Operator UI | CLI, TUI, web overview, API | none | none |
| Language | Rust | Go | Go |

The design goal was production stability on hosts with a mix of host-level and
per-project proxies, which is why `sds` stays out of the data path by default. See
[Limitations](docs/handbook/limitations.md) for the honest list of what it does not do.

## Status

Early release. The state machine, discovery, API and proxy snippets are covered by tests;
the end-to-end path (`just smoke`) runs against a real Docker daemon. Used on Linux hosts
with systemd and on macOS with Docker Desktop. Feedback and issues are welcome.

## Development

```bash
just            # lint, test, build
just smoke      # end-to-end check against real Docker
just docs-serve # handbook with live reload (mdBook)
```

Rust 1.85+ and [just](https://just.systems) are the only tools needed. Examples for host
nginx and Caddy, project and central Traefik, and the native proxy live under
[`examples/`](examples/README.md). Commits follow Conventional Commits; releases are cut
automatically from `main`.

## License

MIT, see [`LICENSE`](LICENSE).

The logo is a derivative of the Docker whale, which is a trademark of Docker, Inc., and is
used solely to indicate that this project works with Docker. This project is not
affiliated with or endorsed by Docker, Inc.

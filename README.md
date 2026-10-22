# sleep-docker-sleep (`sds`)

Stops Docker containers that have not received requests for a while and wakes them up on
the next request. A single Rust binary that hooks into the existing reverse proxy (nginx,
Caddy, Traefik) and does not have to carry any traffic itself.

```text
Browser ─▶ reverse proxy ─(forward-auth)─▶ sds ─▶ docker start/stop
                 └──────────────────────────────▶ container group
```

- Containers describe themselves with labels (`sds.enable=true`, `sds.idle_timeout=15m`).
- A group (a Compose project by default) sleeps and wakes as one unit.
- While waking up, the request is held or a waiting page is shown.
- Native TCP/HTTP proxy for stacks without a reverse proxy.
- CLI, terminal dashboard (`sds top` attaches to the running service), web overview and JSON API from one process.

## Quick start

```bash
sds init --path /etc/sds/sds.toml        # commented configuration
sds check                                # verify Docker access and groups
sds run                                  # daemon with dashboard
sds integrate nginx <group> --host app.example.com --upstream http://127.0.0.1:18080
```

Full guide: [Handbook](docs/handbook/README.md) (installation, configuration, proxy
integration, operations, troubleshooting, API).

## Development

```bash
task            # lint, test, build
task smoke      # end-to-end check against real Docker
task docs:serve # handbook with live reload (mdBook)
```

Examples for nginx, Caddy, Traefik and the native proxy live under `examples/`.
Commits follow Conventional Commits; releases are created automatically from `main`.

## License

MIT, see `LICENSE`.

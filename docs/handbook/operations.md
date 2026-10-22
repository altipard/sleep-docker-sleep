# Operations

## Commands at a glance

| Command | Purpose |
|---|---|
| `sds run [--headless] [--bind ADDR]` | Start the daemon. With a dashboard in a terminal; without a TTY or with `--headless` logs only. |
| `sds status [--json] [--events]` | All groups with state, containers, last request, time left until sleep. |
| `sds wake <group> [--wait]` | Start a group; with `--wait` the command blocks until ready (non-zero exit code on failure). |
| `sds sleep <group>` | Stop a group immediately. |
| `sds check [--json]` | Validate configuration, Docker access and groups without a daemon. Exit code 1 on warnings. |
| `sds init [--path] [--force] [--stdout]` | Write a commented `sds.toml`. |
| `sds integrate <nginx\|caddy\|traefik> <group> …` | Print a proxy snippet. |
| `sds service show\|install\|uninstall` | Show, install or print removal commands for the systemd/launchd unit. |
| `sds config [--path]` | Show the effective configuration or the path in use. |

`status`, `wake` and `sleep` talk to the running daemon. They read `admin.bind` and
`admin.token` from `sds.toml`; override with `--url` / `--token` or the environment
variables `SDS_URL` / `SDS_TOKEN`.

## Terminal dashboard

`sds run` in an interactive session shows groups, state, containers, requests and a log
pane. Keys: `↑`/`↓` select, `w` wake, `s` sleep, `q` quit. On quit, containers stay as
they are.

## Web overview

At `http://<admin.bind>/` (default `http://127.0.0.1:9900/`) there is a page with the same
information, live via server-sent events: groups, countdown until sleep, warnings, event
log, buttons for wake and sleep. Clicking a row shows the effective settings and the
forward-auth URL.

If a token is set, the page asks for it once and stores it in the browser. Only expose it
beyond the host behind a reverse proxy with TLS.

## Group states

| State | Meaning |
|---|---|
| `stopped` | All members stopped. The next request wakes the group. |
| `starting` | `docker start` is running or the health check is not green yet. Requests are held or see the waiting page. |
| `running` | All members running, traffic flows. `sleeps in` shows the remaining time. |
| `stopping` | `docker stop` is running. A request in this phase leads to a restart right afterwards. |
| `failed` | The last start failed (reason in `notes`/`last_error`). For 10 s requests are answered with 503 immediately, then the next request triggers a new attempt. |

External interventions are detected: `docker compose stop` leads to `stopped`,
`docker compose up -d` to `running` with a fresh idle timer. The event log records them as
`external`.

## Logs

- systemd: `journalctl -u sds -f`
- launchd: `/var/log/sds.log`
- container: `docker logs -f sds`

With `log.json = true` the output is JSON lines. Every relevant event (discovered, waking,
running, sleeping, stopped, start_failed, warning, external) appears with the group name.
Verbosity via `log.filter` or `SDS_LOG=debug`.

## Monitoring

- `GET /healthz` answers `ok` as long as the daemon runs (for uptime checks, load balancers).
- `sds status --json` or `GET /api/v1/status` return all groups machine-readable, e.g. for
  a Prometheus exporter or a dashboard across hosts.
- `sds check` as a deployment pipeline step fails on warnings (exit code 1).

## What happens when the daemon stops

`sds` neither stops nor starts containers on shutdown. Running groups keep running until
`sds` runs again and their `idle_timeout` elapses anew from the restart; sleeping groups
are not woken without `sds` (nginx forwards via fail-open to the stopped port and returns
502, Caddy/Traefik return 502/500). Therefore run the service with automatic restart; the
bundled units do that.

## Security

- The Docker socket equals root. Run `sds` only as a trusted user.
- Leave `admin.bind` on `127.0.0.1` wherever possible. Otherwise set `admin.token` and do
  not publish the port.
- The waiting page reveals only the group name and the state, no container ids.
- The check endpoint can wake groups. With a token it is usable only by the proxy.

## Backup

`sds` keeps no persistent state. Only `sds.toml` needs to be backed up.

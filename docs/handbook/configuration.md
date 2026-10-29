# Configuration (`sds.toml`)

`sds` works without a file (built-in defaults, admin API on `127.0.0.1:9900`). For
production a file is still recommended, at least for the token and logging.

## Search order

1. `--config <path>` (an error if the file is missing)
2. environment variable `SDS_CONFIG`
3. `./sds.toml` in the current directory
4. `~/.config/sds/sds.toml`
5. `/etc/sds/sds.toml`

`sds config --path` shows which file is used; `sds config` prints the effective
configuration with the admin token redacted. `sds init` writes a commented template.

Unknown keys are errors so that typos are not silently ignored. All durations use the
`humantime` notation: `30s`, `15m`, `2h`, `1h 30m`.

## Precedence of settings

For every group, from weakest to strongest:

1. built-in defaults
2. `[defaults]` in `sds.toml`
3. `[groups.<name>]` in `sds.toml`
4. container labels (`sds.*`)

If several containers of the same group set different values, the alphabetically first
container wins; `sds check` and the web overview show a warning.

## Reference

```toml
[admin]
bind = "127.0.0.1:9900"      # admin API, forward-auth endpoint and web overview
# token = "…"                 # required as soon as bind is not loopback (startup fails otherwise)
# public_url = "https://…"    # only if the waiting page is served from another origin
# unknown_group = "allow"     # what /_sds/check answers for unknown groups: allow | deny

[docker]
# host = "unix:///var/run/docker.sock"   # or tcp://…; default: DOCKER_HOST or the socket

[discovery]
interval = "30s"             # full re-listing of all containers; Docker events trigger it immediately as well

[log]
filter = "info"              # tracing filter, e.g. "info,sds=debug"
json = false                 # true: JSON lines for journald / log shippers

[defaults]                   # applies to every group, overridable by labels
idle_timeout = "15m"         # stop after this much time without a request
start_timeout = "90s"        # a wake-up counts as failed if not healthy by then
stop_timeout = "20s"         # grace period for docker stop before SIGKILL
start_grace = "5s"           # wait time for health = grace or without a HEALTHCHECK
health = "docker"            # docker | grace | "http://…" | "tcp://host:port"
wake = "hold"                # hold | page
hold_timeout = "60s"         # maximum time a request is held while waking up
max_waiters = 200            # more simultaneously held requests -> immediate 503

[groups.legacy]              # optional: groups without labels
containers = ["legacy-web-1", "legacy-db-1"]
idle_timeout = "30m"         # every field from [defaults] is allowed here

[groups.legacy.proxy]        # optional: native proxy, see its own chapter
listen = "0.0.0.0:8080"
upstream = "127.0.0.1:18080"
mode = "http"                # http | tcp
```

### `[admin]`

| Field | Default | Meaning |
|---|---|---|
| `bind` | `127.0.0.1:9900` | Address of the admin server. For proxies in containers `0.0.0.0:9900` plus a token; `run --insecure` explicitly allows a trusted private network without one. |
| `token` | empty | When set, `/api/*`, `/_sds/check/*` and `/_sds/wait/*` require the header `X-SDS-Token` or `Authorization: Bearer`. The snippets from `sds integrate` include it. The web overview asks for it on first use and keeps it only for the current browser tab. |
| `public_url` | empty | Reserved for setups where the waiting page lives under a different address. |

The admin server speaks plain HTTP. TLS belongs in a reverse proxy in front of it if the
web overview must be reachable from outside the host.

### `[docker]`

Without `host`, `DOCKER_HOST` applies, otherwise the local socket. Docker Desktop (macOS)
is reached through the same socket.

### `[discovery]`

`sds` listens for Docker events and reacts immediately to new, renamed or removed
containers. The interval re-listing catches missed events (e.g. after reconnecting to the
daemon).

### `[log]`

`filter` follows the `RUST_LOG` syntax; the environment variable `SDS_LOG` overrides the
value. With `json = true` the output suits Loki, Elastic or journald parsers.

### `[defaults]` and group settings

| Field | Meaning and notes |
|---|---|
| `idle_timeout` | Time since the last request after which the group is stopped. With the native proxy, open connections count as activity; with forward-auth only the requests themselves (plan long downloads with some margin). |
| `start_timeout` | Upper bound for start plus health check. After that the group is `failed`; requests during the next 10 s are answered immediately, the first one after that triggers a new attempt (`sds wake` does so at once). |
| `stop_timeout` | Passed to `docker stop`. Databases often need more than the Compose default of 10 s. |
| `start_grace` | Wait time when no other health check applies. |
| `health` | `docker`: the container's HEALTHCHECK (all members with a HEALTHCHECK must be `healthy`; if none has one, `start_grace` applies). `http://…`: GET until 2xx/3xx. `tcp://host:port`: connection attempt. `grace`: just wait. Addresses are as seen from the `sds` process. |
| `wake` | `hold`: the request is held until ready, the client only notices a delay. `page`: immediate waiting page with automatic reload (for browser applications with long start times). |
| `hold_timeout` | After this time a held request receives a 503 with the waiting page. Must be smaller than the reverse proxy's timeouts (the snippets set those accordingly). |
| `max_waiters` | Protection against load spikes while waking: more simultaneously waiting requests are answered with 503 immediately. |

`idle_timeout`, `start_timeout`, `hold_timeout` and `max_waiters` must be greater than
zero; `sds` refuses the file (or ignores the labelled container with a warning) otherwise.
`stop_timeout = "0s"` (immediate kill) and `start_grace = "0s"` are allowed.

### `[groups.<name>]`

For containers that cannot carry labels (third-party images, manually started
containers). `containers` holds names as shown by `docker ps`. If a container is missing,
the group still appears with a warning so the problem is visible. If a container also
carries labels, the labels win.

Group names must not contain whitespace or `/`, because they are part of the URL
`/_sds/check/<name>`.

## Applying changes

After changing `sds.toml`, restart the service (`systemctl restart sds`). A reload via
SIGHUP is not implemented yet; `sds` logs a note. Label changes on containers need no
restart: `docker compose up -d` creates new containers, which `sds` detects immediately.

Before restarting, run `sds check --config /etc/sds/sds.toml`: it validates the file,
checks Docker access and lists all groups with their effective values. Exit code 1 means
there are warnings.

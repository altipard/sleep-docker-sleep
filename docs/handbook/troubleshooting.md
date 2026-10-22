# Troubleshooting

First step, always: `sds check` (configuration and discovery) and `sds status --events`
(state and recent events) or the web overview.

| Symptom | Cause | Remedy |
|---|---|---|
| `sds check`: `docker daemon unreachable` | socket not readable or wrong `docker.host` | user in group `docker` or root; check `DOCKER_HOST` |
| group missing in `sds status` | no `sds.enable=true`, or a broken label (see warning in `sds check`) | check labels, run `docker compose up -d` again |
| group has a different name than expected | group name = Compose project (directory name) | set `sds.group` or `COMPOSE_PROJECT_NAME` |
| warning "sets different sds.* values" | members contradict each other | keep group-wide labels on one container |
| group never sleeps | requests keep coming (monitoring, the proxy's health checks, crawlers) | `sds status` shows `requests`; point the proxy's health checks at a path that bypasses forward-auth, or choose intervals larger than `idle_timeout` |
| group never sleeps (native proxy) | open keep-alive connection | `sds status` shows `N open`; limit the client |
| group does not wake, browser shows 502 | request bypasses forward-auth (location without `auth_request`, wrong path) | compare with the proxy snippet; `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9900/_sds/check/<group>` must return 200 and wake the group |
| state `failed`, "not ready after 90s" | health check does not apply (wrong address, container without HEALTHCHECK but `health=docker` and start takes longer than `start_grace`) | check the address as seen from the `sds` process; raise `start_grace`/`start_timeout`; test the health URL with `curl` |
| state `failed`, "container … is exited" | member crashes on start | `docker logs <container>`; often a missing dependency, set a restart policy |
| state `failed`, "docker start …: 404" | container was recreated (different id) | discovery catches up within seconds; otherwise `sds check` |
| waiting page appears but reloads forever | group becomes `running` but the proxy still does not forward | disable proxy caching for the check location; nginx: check the `error_page` lines |
| nginx returns 500 instead of the waiting page | `?deny=403` or `error_page 403` missing | regenerate the snippet |
| Traefik returns 500 | `sds` unreachable (no fail-open) | `systemctl status sds`; check the `host.docker.internal` address plus `extra_hosts` |
| 503 "Busy" | more than `max_waiters` requests waiting at once | raise the value or use `wake = page` |
| requests hang exactly `hold_timeout` and end in 503 | start time exceeds `hold_timeout` | `wake = page` for this group or raise `hold_timeout` (and the proxy timeout) |
| web overview "disconnected" | token set, SSE not possible without a token | reload the page, enter the token; the page then polls every 2 s |
| `proxy not started: bind … address in use` | port for the native proxy is taken | the container still publishes the port itself; move it to `127.0.0.1:<other port>` |
| container starts by itself after a host reboot | `restart: always` | intended; `sds` detects `running` and puts it to sleep again after `idle_timeout` |

## Debug logging

```bash
SDS_LOG=debug sds run --headless
```

shows every discovery, every health probe attempt and Docker events. Keep `info` for
production.

## Reproducing a state by hand

```bash
sds sleep shop                      # stops
curl -i http://127.0.0.1:9900/_sds/check/shop        # holds until ready, then 200
curl -i -H 'Accept: text/html' 'http://127.0.0.1:9900/_sds/check/shop?mode=page'   # 503 + HTML
sds wake shop --wait                # exit code 0 as soon as healthy
```

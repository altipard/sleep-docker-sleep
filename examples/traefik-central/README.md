# Central Traefik

One Traefik container routes to every project on the host. Projects attach to the shared
`proxy` network and configure their router through labels; nothing on the host changes
when a project is added.

```bash
docker network create proxy
docker compose -f traefik/compose.yaml up -d
docker compose -f shop/compose.yaml up -d
```

`sds` runs on the host (`sds run --bind 0.0.0.0:9900` or `admin.bind = "0.0.0.0:9900"`
plus a token, because Traefik reaches it via `host.docker.internal`). Alternatively run
`sds` as a container on the `proxy` network (see `../sds-as-container.compose.yaml`) and
use `http://sds:9900/_sds/check/<group>` in the labels; then no host port is needed.

Traefik has no fail-open for forwardAuth: while `sds` is down, requests fail with 500.
Keep `sds` under systemd/launchd with automatic restart.

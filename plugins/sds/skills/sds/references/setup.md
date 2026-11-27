# Setting up sds

Short form of the handbook (https://altipard.github.io/sleep-docker-sleep/). Chapters for
the details: "Labels and groups", "Configuration (sds.toml)", "Reverse proxy integration",
"Native proxy". Show every change to the user before making it (ground rule 4).

## 1. Group membership

A group is the unit that sleeps and wakes together. Its name is `sds.group` if set, else
the Compose project (usually the directory name), else the container name. The name must
not contain whitespace or `/`. Only containers with `sds.enable=true` are managed.

| Situation | Labels |
|---|---|
| Host nginx in front of the project, everything may sleep | `sds.enable=true` on all services |
| Project brings its own Caddy or Traefik | `sds.enable=true` on app and db, `sds.keep_awake=true` on the proxy |
| Two projects share a database | `sds.group=shared-db` on all participants |
| Only the app should sleep, the db keeps running | `sds.enable=true` only on the app |

Compose:

```yaml
services:
  app:
    image: registry.example.com/shop:1.4.2
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.idle_timeout: "20m"
  db:
    image: postgres:16
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.stop_timeout: "60s"
```

`docker run` (no Compose project, so set `sds.group`, otherwise every container is its own
group):

```bash
docker run -d --name shop-app --network shop --restart unless-stopped \
  --label sds.enable=true --label sds.group=shop --label sds.idle_timeout=20m \
  -p 127.0.0.1:18080:8080 registry.example.com/shop:1.4.2
```

Labels are fixed when a container is created. A label change means recreating the
container (`docker compose up -d`, or `docker rm` and `docker run`); that is the user's
decision, not yours (ground rule 4). sds picks up the new container without a restart.

All labels: `sds.enable`, `sds.keep_awake`, `sds.group`, `sds.idle_timeout`,
`sds.start_timeout`, `sds.stop_timeout`, `sds.start_grace`, `sds.health`, `sds.wake`,
`sds.hold_timeout`, `sds.max_waiters`, `sds.proxy.listen`, `sds.proxy.upstream`,
`sds.proxy.mode`. An unknown `sds.*` label or an invalid value (`sds.idle_timeout=0s`)
makes sds ignore the container; `sds check` names it. A container with `sds.*` labels but
without `sds.enable=true` is ignored with a warning; `sds.enable=false` silences it.
Members start in alphabetical order; Compose `depends_on` is not read.

Health check (`sds.health`): `docker` (default, the image's HEALTHCHECK), `http://…` or
`tcp://host:port` as seen from the sds process (on the host: the published port), or
`grace` with a generous `sds.start_grace`.

Containers that cannot carry labels (third-party images, containers started by hand) go
into `sds.toml`; `containers` holds names as shown by `docker ps`:

```toml
[groups.legacy]
containers = ["legacy-web-1", "legacy-db-1"]
idle_timeout = "30m"
```

`sds.toml` changes need a restart of sds, which is the user's step (ground rule 4).
Unknown keys are errors; check the file with `sds check --config <file>` before the
restart. `sds init --stdout` prints a commented template.

## 2. Wake path: reverse proxy (forward-auth)

The proxy asks sds before every request; sds wakes the group and holds the request or
serves a waiting page. `sds integrate <nginx|caddy|traefik> <group>` prints the snippet.

Where the proxy reaches sds (`--sds-url`):

| sds runs … | proxy runs … | `--sds-url` |
|---|---|---|
| on the host | on the host (nginx) | default, from `admin.bind` (`http://127.0.0.1:9900`) |
| on the host | in a container | `http://host.docker.internal:9900`; the proxy service needs `extra_hosts: ["host.docker.internal:host-gateway"]`, sds needs a non-loopback `admin.bind` and an `admin.token` |
| in a container | in a container, same network | `http://sds:9900` |

As soon as `admin.bind` is not loopback, `admin.token` is required, and **the snippet then
contains the token in clear text** (ground rule 5). Steps:

1. Preview only masked:

   ```bash
   sds integrate nginx shop --host shop.example.com --upstream http://127.0.0.1:18080 \
     | sed -E 's/(X-SDS-Token:? )"[^"]+"/\1"<redacted>"/'
   ```

2. Write the file without group or world read, after the user agreed:

   ```bash
   (umask 077; sds integrate nginx shop --host shop.example.com \
     --upstream http://127.0.0.1:18080 > ./sds-shop.conf)
   ```

   Into a root-owned directory the redirect must run inside the root shell:
   `sudo sh -c 'umask 077; sds integrate nginx shop > /etc/nginx/snippets/sds-shop.conf'`.
   The umask only applies to a new file: an existing file keeps its mode, so run
   `chmod 600 FILE` before overwriting it (a re-run, a rotated token).

3. If the proxy reads its config as a non-root user (Caddy usually runs as `caddy`), give
   only that group read access: `chgrp <proxy group> FILE; chmod 640 FILE`. nginx reads
   its config as root, so 0600 is enough.
4. Keep the file out of git, tell the user where it is, and leave reloading the proxy to
   the user.

Per proxy:

- **nginx**: fails open (an unreachable sds lets traffic through). Needs
  `map $http_upgrade $connection_upgrade { default upgrade; '' close; }` once in the
  `http {}` context; the snippet says so in a comment.
- **Caddy**: no fail-open; an unreachable sds means 502. From a Caddy container use
  `--sds-url http://host.docker.internal:9900` or `http://sds:9900`.
- **Traefik**: no fail-open; an unreachable sds means 500. Use the dynamic configuration
  (file provider) part, not the container-label variant at the end of the output: labels
  put the token into the compose file and `docker inspect`. This writes only the file
  provider part:

  ```bash
  (umask 077; sds integrate traefik shop --host shop.example.com \
    --upstream http://app:80 --sds-url http://host.docker.internal:9900 \
    | sed '/^# --- or as container labels/,$d' > ./sds-shop.yml)
  ```

  If labels are required, put them into a compose override file that is not committed.
  Traefik has no per-middleware forward-auth timeout; with `wake = hold` its global
  forwarding timeouts must exceed `hold_timeout`.

The nginx and Caddy snippets set their read timeout to `hold_timeout` + 5 s;
`--hold-timeout <seconds>` overrides the value taken from `sds.toml`.

Unknown group names in a snippet are let through with a warning by default; with
`admin.unknown_group = "deny"` they get an "unknown service" page instead.

## 3. Without a reverse proxy: native proxy

For stacks without a reverse proxy and for plain TCP (databases, SMTP), sds owns the public
port. The container publishes its port only locally:

```yaml
services:
  app:
    ports:
      - "127.0.0.1:18080:80"
    labels:
      sds.enable: "true"
      sds.group: "demo"
      sds.health: "http://127.0.0.1:18080/"
      sds.proxy.listen: "0.0.0.0:8080"
      sds.proxy.upstream: "127.0.0.1:18080"
      sds.proxy.mode: "http"
```

Open connections count as activity, so long downloads and WebSockets keep the group awake
(and permanent keep-alive clients keep it from sleeping). No TLS; `tcp` mode always holds
the connection until the group is ready. sds is then in the data path, so it must run as
a supervised service.

## 4. Verify

1. `sds check`: config, Docker access, discovered groups. Exit code 1 means warnings.
2. `sds status`: the group appears with its members.
3. An end-to-end test (`sds sleep <group>`, then a request through the proxy) stops the
   group, so only with the user's go (ground rule 7).

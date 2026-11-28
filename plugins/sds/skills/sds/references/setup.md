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

1. Find the config the daemon uses (`sds config --path` shows the CLI's choice; a service
   unit names it with `--config`). Binaries before the fix release (fixed from
   the next release on) silently fall back to built-in defaults in `sds integrate`,
   without token and with the default `admin.bind`, when they cannot read that file, even
   with `-c`; newer ones exit non-zero instead. So always pass it explicitly. The examples use
   `cfg=/etc/sds/sds.toml`.
2. Preview with every token line hidden:

   ```bash
   sds -c "$cfg" integrate nginx shop --host shop.example.com --upstream http://127.0.0.1:18080 \
     | sed -E '/[Xx]-[Ss][Dd][Ss]-[Tt][Oo][Kk][Ee][Nn]/s/^([[:space:]#]*).*/\1<line with X-SDS-Token hidden>/'
   ```

   Hidden lines mean a token is set; check that the sds address is the one the proxy can
   reach.
3. Write the file after the user agreed, always with this recipe, for any target. It
   writes a temporary file (`mktemp` creates it with mode 0600) in a subshell whose trap
   removes it on exit, Ctrl-C or kill, and installs it with a fixed mode, which also
   applies to an existing target (`umask` would only cover new files). The guard compares
   the snippet with the config: `sds config` shows a set token as `token = "<redacted>"`,
   and then the snippet must contain it. Without `admin.token` (loopback `admin.bind`, the
   usual host nginx setup) a snippet without token is expected. `sds config` also exits 1
   on an unreadable config, so it catches what older `sds integrate` binaries
   would silently replace with defaults.

   ```bash
   cfg=/etc/sds/sds.toml                    # the config the daemon runs with
   dest=/etc/nginx/snippets/sds-shop.conf
   (
     tmp=$(mktemp) || exit 1
     trap 'rm -f "$tmp"' EXIT
     trap 'exit 130' INT
     trap 'exit 143' TERM
     conf=$(sds -c "$cfg" config) || exit 1
     sds -c "$cfg" integrate nginx shop --host shop.example.com \
       --upstream http://127.0.0.1:18080 > "$tmp" || exit 1
     n=$(grep -c X-SDS-Token "$tmp")        # count only, never the lines
     case $conf in
       *'token = "<redacted>"'*)
         [ "$n" -gt 0 ] || { echo "error: admin.token is set but missing from the snippet" >&2; exit 1; } ;;
       *) echo "note: no admin.token in $cfg, so the snippet has none (expected)" ;;
     esac
     sudo install -m 600 "$tmp" "$dest"
   )
   ```

   Adjust only these parts:

   - **Mode and group** are the arguments of the last line. nginx reads its config as
     root: `-m 600`. A proxy that reads its config as a non-root user (Caddy usually runs
     as `caddy`) gets group read and nothing more: `sudo install -m 640 -g caddy "$tmp" "$dest"`.
   - **A target you own** (`dest=./sds-shop.conf`): the same line without `sudo`.
   - **A config only root can read**: put `sudo "$(command -v sds)"` in place of `sds` in
     both calls and keep the redirects in the user shell. Do not run the recipe through
     `sudo sh -c`: sudo drops `SDS_CONFIG`, `~` becomes root's and `sds` may not be on
     sudo's `PATH`.
   - **Traefik**: replace the `integrate` line as shown under "Per proxy".

   A kill that cannot be trapped (`kill -9`, a hard tool timeout) leaves the temporary
   file behind; it is still mode 0600, remove it.
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
  provider part; in the recipe of step 3 it replaces the `integrate` line (the
  variable keeps a failing `sds` from producing an empty file, and is never printed):

  ```bash
  snip=$(sds -c "$cfg" integrate traefik shop --host shop.example.com \
    --upstream http://app:80 --sds-url http://host.docker.internal:9900) || exit 1
  printf '%s\n' "$snip" | sed '/^# --- or as container labels/,$d' > "$tmp"
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

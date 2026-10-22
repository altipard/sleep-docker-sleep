# Installation

## Requirements

- Docker Engine 20.10 or newer (API 1.41+), Docker Compose v2 optional.
- Linux (systemd) or macOS (launchd). Windows is not supported.
- Access to the Docker socket (`/var/run/docker.sock`) for the user `sds` runs as. Note:
  whoever can read and write the socket effectively has root on the host. Run `sds` as
  `root` or as a dedicated user in the `docker` group, never as an application user.

## Option A: binary on the host (recommended)

Every [GitHub release](https://github.com/altipard/sleep-docker-sleep/releases) ships
binaries for Linux (x86_64, aarch64) and macOS (Apple Silicon, Intel):

```bash
curl -fsSLo sds https://github.com/altipard/sleep-docker-sleep/releases/latest/download/sds-linux-x86_64
sudo install -m 755 sds /usr/local/bin/sds
```

Or build from source (Rust 1.85+):

```bash
cargo install --git https://github.com/altipard/sleep-docker-sleep --locked
```

Put the configuration in `/etc/sds/sds.toml` (see [Configuration](configuration.md)), then:

```bash
sudo sds service install --config /etc/sds/sds.toml
```

`service install` writes the matching unit (`/etc/systemd/system/sds.service` or
`/Library/LaunchDaemons/com.zilicon.sds.plist`) and prints the commands to enable it.
`sds service show` only prints the unit without writing anything. `--user <name>` runs
the service as a different user.

systemd:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sds
journalctl -u sds -f
```

launchd:

```bash
sudo launchctl bootstrap system /Library/LaunchDaemons/com.zilicon.sds.plist
tail -f /var/log/sds.log
```

## Option B: as a container

Useful when the reverse proxy itself runs as a container (Traefik, a project's own Caddy)
and `sds` should be reachable on the same Docker network. Template:
`examples/sds-as-container.compose.yaml`.

```yaml
services:
  sds:
    image: ghcr.io/altipard/sleep-docker-sleep:<version>
    restart: unless-stopped
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ./sds.toml:/etc/sds/sds.toml:ro
    networks: [proxy]
    labels:
      sds.keep_awake: "true"
```

`sds.toml` must then contain `admin.bind = "0.0.0.0:9900"`, and the reverse proxy uses
`http://sds:9900/_sds/check/<group>`. Do **not** publish the port to the outside, or set an
`admin.token`.

Health checks with `http://…` or `tcp://…` are executed from inside the `sds` container.
Give addresses as reachable from there (`http://app:3000/` instead of `127.0.0.1:18080`).

## Multiple hosts

One `sds` per host. There is deliberately no central instance; each one manages only the
local Docker daemon. An overview across hosts can be built on the [HTTP API](api.md)
(`/api/v1/status`).

## Upgrading

Replace the binary and restart the service:

```bash
sudo install -m 755 sds /usr/local/bin/sds && sudo systemctl restart sds
```

Across the restart, running containers keep running and sleeping ones keep sleeping;
`sds` reads the current state from Docker at startup. Requests during the restart pass
through to the application via fail-open on nginx; on Caddy and Traefik there are 502/500
errors for the duration of the restart (under a second), see [Limitations](limitations.md).

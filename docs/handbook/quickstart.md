# Quick start

Five steps from an empty host to the first sleeping group. Details follow in the later
chapters.

## 1. Install the binary and check

```bash
sudo install -m 755 sds /usr/local/bin/sds
sds --version
sds check            # checks Docker access; "no groups" is fine at this point
```

## 2. Create the configuration

```bash
sudo mkdir -p /etc/sds
sds init --path /etc/sds/sds.toml
```

The file has a comment for every field. For a first test it can stay unchanged: admin API
on `127.0.0.1:9900`, `idle_timeout` 15 minutes.

## 3. Label the containers

In the project's Compose file:

```yaml
services:
  app:
    image: …
    ports:
      - "127.0.0.1:18080:80"     # local only; the reverse proxy talks to 18080
    labels:
      sds.enable: "true"
      sds.idle_timeout: "15m"
      sds.health: "http://127.0.0.1:18080/healthz"
  db:
    image: …
    labels:
      sds.enable: "true"         # same Compose project = same group
```

```bash
docker compose up -d
sds check                        # shows the group and its settings
```

The group name is the Compose project name (the directory name) unless `sds.group` is set.

## 4. Start the daemon

To try it out in the foreground with the dashboard:

```bash
sds run --config /etc/sds/sds.toml
```

Permanently as a service (one daemon per host covers all projects):

```bash
sudo sds service install --config /etc/sds/sds.toml
sudo systemctl enable --now sds          # macOS: see the output of service install
sds status                               # snapshot
sds top                                  # dashboard attached to the service, q detaches
```

## 5. Connect the reverse proxy

```bash
sds integrate nginx <group> --host app.example.com --upstream http://127.0.0.1:18080
```

The output is a complete `server` block (a Caddyfile block for Caddy, a middleware
definition for Traefik). Add it, reload the proxy, done.

Test: `sds sleep <group>`, then open the site in a browser. The waiting page appears
briefly, or the page loads after one or two seconds; afterwards the group is `running`
again in `sds status` and at `http://127.0.0.1:9900/`.

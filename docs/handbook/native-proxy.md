# Native proxy

For stacks without a reverse proxy and for plain TCP services, `sds` can own the public
port itself. This is the only mode in which `sds` sits in the data path; accordingly it is
meant for special cases, not as a replacement for nginx, Caddy or Traefik.

## Moving the port

The container must no longer publish its port publicly itself, otherwise the two collide.
Pattern:

```yaml
services:
  app:
    ports:
      - "127.0.0.1:18080:80"        # local only
    labels:
      sds.enable: "true"
      sds.group: "demo"
      sds.health: "http://127.0.0.1:18080/"
      sds.proxy.listen: "0.0.0.0:8080"
      sds.proxy.upstream: "127.0.0.1:18080"
      sds.proxy.mode: "http"
```

`sds` listens on `:8080`, forwards to `127.0.0.1:18080` and counts every connection as
activity. The same can be put into `sds.toml` under `[groups.<name>.proxy]`.

A port already in use is reported when the group starts (event "proxy not started"); the
group itself keeps working via forward-auth.

## Modes

| Mode | Behaviour |
|---|---|
| `http` | Understands requests. Sleeping group: hold or waiting page (503) depending on `wake`. WebSocket upgrades are passed through. Bodies are streamed, not buffered. Header read timeout 30 s, upstream response-header timeout 60 s (`504`), hop-by-hop headers are stripped. Sets `X-Forwarded-For` and `X-Forwarded-Proto`. |
| `tcp` | Opaque byte stream (databases, SMTP, SSH). The connection is accepted, the group woken, after readiness the upstream is connected and bytes are copied both ways. No bytes flow before readiness; the connection is always held (`wake = page` has no effect) and dropped after `hold_timeout`. |

Each listener accepts at most 1024 simultaneous connections; further clients wait in the
accept queue. Changing `upstream` or `mode` of a running proxy keeps the socket and only
restarts the accept loop; established connections finish with the old settings.

## Idle detection

Unlike forward-auth, **open connections** count as activity here. A group only sleeps
when no connection is open any more and `idle_timeout` has elapsed since the last one.
Long downloads and WebSockets therefore keep the group awake. The flip side: clients with
permanent keep-alive connections (monitoring, load balancers) prevent sleeping. Limit such
connections on the client side or use a reverse proxy with forward-auth.

## Limitations

- No TLS. For HTTPS put a reverse proxy in front, which can then use forward-auth anyway.
- HTTP/1.1 to the upstream; HTTP/2 is not offered to clients.
- No load balancing, one upstream per group.
- In this mode `sds` is a single point of failure for the group. Supervise the service.

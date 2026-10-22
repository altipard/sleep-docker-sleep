# Limitations and known behaviour

Deliberate decisions and open points of the current version. Anyone who needs one of
these should file an issue rather than work around it in proxy configuration.

## Fail-open only with nginx

nginx can bypass an unreachable `sds` via `error_page`. Caddy and Traefik have no clean
bypass for forward-auth: if `sds` is unreachable, clients see `502` (Caddy) or `500`
(Traefik). The bundled units restart `sds` within two seconds after a crash; `sds` itself
is ready in under a second and reads the current state from Docker.

## Idle detection with forward-auth counts requests, not connections

The proxy tells `sds` about the start of a request, not its end. A download that takes
longer than `idle_timeout` can be interrupted. In practice, `idle_timeout` values of
10 minutes and more are uncritical; where it matters, the native proxy helps, which counts
open connections.

## No config reload

Changes to `sds.toml` require a restart of the service. Label changes on containers take
effect immediately. SIGHUP is accepted and logged but not acted upon.

## Start order within a group

Members are started alphabetically; `depends_on` from Compose is not read. Applications
should retry connecting or have a restart policy. Stopping happens in parallel.

## Compose dependencies across groups

`sds` does not know that group A needs group B. Both are woken and put to sleep
independently. Shared dependencies belong in the same group (`sds.group`) or stay awake
(`sds.keep_awake`).

## No TLS on the admin server

Admin API and web overview speak plain HTTP. By default they listen on `127.0.0.1` only.
For access from outside, put a reverse proxy with TLS in front.

## Only one Docker daemon per instance

Swarm, Kubernetes and remote hosts via several sockets are not supported. One `sds` runs
per host.

## Health checks from the `sds` process's point of view

`http://` and `tcp://` are executed from where `sds` runs. On the host that is the
published port (`127.0.0.1:18080`), in a container the service name on the Docker
network. On macOS, container IPs are not reachable from the host; always use the
published port there or `health = docker`.

## The first call is slow

That is the principle: the first caller waits for the containers' start time. To avoid it
for certain clients (API consumers with short timeouts), use `wake = page` for browsers
and keep `idle_timeout` high for such groups.

## Unknown groups are let through

A typo in the proxy snippet does not cause an outage but a warning in the log and
`X-SDS-State: unknown-group`. Watch the logs after changing group names.

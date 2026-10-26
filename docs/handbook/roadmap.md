# Roadmap

Ideas that came up in use and are worth building, in rough order of priority. None of
them is scheduled; an issue or a pull request moves an item up. Items here are not
promises.

## Instance label to partition one Docker daemon

Today every `sds` process adopts every container with `sds.*` labels on its daemon, which
is why only one instance per daemon works (see
[Limitations](limitations.md#only-one-docker-daemon-per-instance-one-instance-per-daemon)).
A label such as `sds.instance = "team-a"` plus a matching `instance` setting in `sds.toml`
would let discovery filter containers and run several independent instances against one
daemon, for example one per team or per environment on a shared host. Containers without
the label would stay with the default instance so existing setups keep working.

## Consolidated overview across hosts

The web overview and `sds top` show the groups of the daemon they are attached to. With
`sds` on several hosts there is no single place to see all groups, their state and their
request counters. Two possible shapes:

- A read-only aggregator mode (`sds board`) that polls the admin API of a list of
  instances and renders one combined overview and TUI. Wake and sleep commands would be
  forwarded to the owning instance. Needs the admin API reachable across hosts, so TLS or
  a VPN in front, and the per-instance token.
- An export of the status as Prometheus metrics so an existing Grafana can do the
  consolidation. Cheaper to build, but no control actions.

The aggregator is the better fit for the operator use case; the metrics export may come
first because it is small.

## Config reload

`sds.toml` changes still need a restart; SIGHUP is accepted but ignored. See
[Limitations](limitations.md#no-config-reload).

## Fail-open for Caddy and Traefik

Only the nginx snippet can bypass an unreachable `sds`. A tiny health-aware fallback for
the forward-auth path would close the gap, see
[Limitations](limitations.md#fail-open-only-with-nginx).

## Hold that survives a restart of sds

`sds hold` is kept in memory: it survives `docker compose down` / `up` of the group, but a
restart of `sds` (update, crash, host reboot) forgets it, and the next request wakes a
group that is meant to stay down for maintenance. Writing the set of held groups to a
small state file (e.g. next to `sds.toml` or in the service's state directory) and loading
it at startup would close that. Needs a writable location in every install variant
(systemd, launchd, container), so the path should be configurable and a missing or
unwritable file only a warning. See
[Operations](operations.md#keeping-a-group-down).

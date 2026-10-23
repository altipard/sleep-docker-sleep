# Demo: ten services, short timeouts, random traffic

A playground to watch `sds` work: ten tiny [whoami](https://github.com/traefik/whoami)
web servers, each its own group with an idle timeout between 20 and 60 seconds, and a
script that sends random requests. Within a minute the dashboard shows groups going to
sleep, waking up on a request and serving again.

```bash
cd examples/demo
docker compose up -d        # ten containers, ports 18101-18110 on loopback
sds run                     # in a second terminal: dashboard with the ten groups
./traffic.sh                # in a third terminal: random requests, Ctrl-C to stop
```

`sds run` discovers the groups from the labels; a daemon that is already running picks
them up on its next discovery pass. The traffic goes through the native sds proxies on
ports 18201-18210, so `sds` sees every request. Services that appear several times in
the pool of `traffic.sh` (`api`, `web`) rarely sleep; `cdn` is hit so rarely that it is
asleep most of the time. `blog` and `wiki` use `sds.wake=page`, so the first request
after a nap gets the waiting page (HTTP 503) instead of being held.

A wake takes about a second here: `whoami` starts instantly and `sds.start_grace` is
1 s. Real applications take longer; raise `sds.start_timeout` accordingly.

Clean up with `docker compose down`.

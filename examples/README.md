# Examples

Each directory is a runnable setup. `sds` runs on the host unless stated otherwise;
labels in the Compose files form the groups.

| Directory | Setup |
|---|---|
| `nginx-host/` | nginx installed on the host, one project behind it. Forward-auth to `127.0.0.1:9900`. |
| `caddy-host/` | Caddy installed on the host, two projects with separate groups. Forward-auth to `127.0.0.1:9900`. |
| `caddy-project/` | A project that ships its own Caddy container; Caddy stays awake, app and db sleep. |
| `traefik-project/` | A project that ships its own Traefik container, configured through labels. |
| `traefik-central/` | One central Traefik container on a shared `proxy` network, projects attach to it. |
| `native-proxy/` | No reverse proxy; `sds` owns the public port itself. Also the pattern for TCP services. |
| `sds-as-container.compose.yaml` | Run `sds` itself as a container next to a containerised proxy. |

Snippets were generated with `sds integrate <proxy> <group> …` and are kept here so the
examples work without the binary at hand.

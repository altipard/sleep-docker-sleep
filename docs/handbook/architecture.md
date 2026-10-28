# Architecture (for developers)

`sds` is a single Rust binary (edition 2024, tokio) with a library part and a CLI part.
The modules follow the domain boundaries:

```text
src/
├── config/       sds.toml, label parsing, effective group settings
├── docker/       backend trait, bollard implementation, discovery (containers -> groups)
├── controller/   state machine per group, activity counters, registry, supervisor
├── admin/        axum: API, forward-auth, waiting page, SSE, embedded web overview
├── proxy/        native TCP/HTTP proxy (hyper)
├── integrate/    snippet generators for nginx, Caddy, Traefik
├── service/      systemd/launchd units
├── tui/          ratatui dashboard, log ring buffer
└── cli/          clap commands, API client
```

## Data flow

```text
 Docker events ──┐                             ┌── /_sds/check (axum)
 Docker list ────┴─▶ Supervisor ─▶ Registry ───┤── /api/v1/*   (axum)
                     (discover)   (1 actor     ├── native proxy (hyper)
                                   per group)  └── TUI / web overview
                                        │
                                        ▼
                                  Backend trait ─▶ bollard ─▶ Docker Engine API
```

- **Supervisor** lists containers periodically and on create/destroy/rename events, builds
  deterministic `GroupSpec`s with `discover()` and calls `Registry::apply`.
- **Registry** holds one actor (tokio task) per group, its `GroupHandle` and optionally a
  proxy listener. New groups are started, changed ones receive the new `GroupSpec` through
  a `watch` channel (latest value wins, nothing is ever dropped), vanished ones are
  cancelled. On shutdown the registry waits for running `docker stop`s.
- **Actor** owns the state machine (`stopped → starting → running → stopping`, `failed`).
  Long operations (`docker start` + health probing, `docker stop`) run in their own tasks
  so the actor keeps reacting to commands and events. The state is published as a
  `GroupSnapshot` over a `watch` channel.
- **GroupHandle** is the interface for request paths: `touch()` (two atomics),
  `ensure_ready()` (waits on the `watch` channel), `request_wake()` (sets a flag and pokes
  the actor through a `Notify`; idempotent, never lost). `force_wake()` is the operator
  variant used by the API and the dashboard.

## Guarantees in the request path

- No Docker call in the request path. Handlers read the `watch` snapshot and write
  atomics in `Activity`.
- One start per group no matter how many requests wait: a wake is a flag, not a queued
  message, and the actor only starts from `stopped`/`failed`.
- Held requests are bounded by `max_waiters`; beyond that an immediate 503.
- No buffering of whole bodies in the native proxy; `copy_bidirectional` and hyper streams
  with backpressure over TCP.
- Stop only when `inflight == 0` and `idle_timeout` has elapsed; a wake during `stopping`
  is remembered and executed right afterwards.
- After a failed start, traffic-driven wakes are ignored by the actor for 10 s
  (`FAILED_RETRY_INTERVAL`, measured on a monotonic clock) and requests are answered
  immediately; then the next one triggers a new attempt. `sds wake` and the dashboard
  bypass the pause.
- Observation errors are not state: an `inspect` that fails for any reason other than
  "not found" keeps the last known state, so a Docker API hiccup never puts a busy group
  to sleep. Inspects run in parallel with a 5 s bound each.
- `failed` is left only by a new start attempt, which probes health. Containers that run
  but never became healthy do not make the group `running`; they are stopped once
  `idle_timeout` has elapsed, and `sds sleep` stops them right away.
- A failed `docker stop` is retried with a backoff of one `idle_timeout`, and the error
  stays visible in `last_error` until a stop succeeds.

## Running from source

```bash
cargo run -- run                          # dashboard when attached to a terminal
cargo run -- run --headless               # logs only
cargo run -- run --config sds.toml        # explicit config
cargo run -- run --bind 127.0.0.1:9901    # next to an installed sds on :9900
cargo run -- status                       # any other command from a second shell
```

Arguments after `--` go to `sds`. Without `--config` the usual search applies
(`$SDS_CONFIG`, `./sds.toml`, `~/.config/sds/sds.toml`, `/etc/sds/sds.toml`), then the
built-in defaults. The `justfile` wraps the common cases: `just init` writes `sds.toml`
into the repository, `just run` / `just run-headless` start the daemon with it, `just top`
and `just status` talk to it. The web overview is then at `http://127.0.0.1:9900/`, the
API documentation at `/docs`.

The version shown by `--version`, the web overview, the API and the dashboard comes from
`build.rs`: `SDS_VERSION` when set (release builds and the image), otherwise
`git describe --tags` of the checkout, e.g. `0.3.0-3-g34a5faa` three commits after 0.3.0.
`cargo install --git` builds without git metadata and reports the `Cargo.toml` version.

## Tests

- Unit tests per module (`cargo test`), including the state machine against an in-memory
  `MockBackend` (`controller/actor.rs`): wake, idle stop, open connections, failed start
  with rate limiting, external start/stop events, wake during stop.
- Snippet generators and unit files are checked for the decisive lines.
- CLI tests (`tests/cli.rs`) via `assert_cmd`.
- End-to-end check against real Docker: `just smoke` (labelled nginx, forward-auth wake,
  idle stop), or by hand with `examples/native-proxy`.

## Release

Conventional Commits and commitizen decide the version: `fix` → patch, `feat` → minor,
`!` → major; other types do not trigger a release. Versions live in git tags only.
`.github/workflows/ci.yml` runs fmt, clippy, tests, hadolint and the commit-format check
on pull requests. `.github/workflows/release.yml` runs on `main`: it asks commitizen for
the next version, builds binaries for Linux and macOS with that version stamped into the
binary, pushes the image to `ghcr.io` and creates the tag and the GitHub release.

Development happens on `develop`, the default branch; feature branches and outside pull
requests target it. A release is a pull request from `develop` into `main`, merged with a
merge commit: a squash would hide the individual `feat`/`fix` commits from commitizen and
let the two branches drift apart. `main` accepts changes only through pull requests with
green CI and refuses force pushes; `develop` refuses force pushes and deletion.

## Extension points

- Another proxy type: add a render function in `integrate/mod.rs`.
- Another container backend (Podman speaks the same API; others): implement
  `docker::Backend`.
- Config reload: supervisor and admin server receive a `watch::Receiver<Arc<Config>>`;
  the signal handling in `cli/run.rs` is already prepared.
- Metrics: `Registry::snapshots()` provides everything a Prometheus endpoint needs.

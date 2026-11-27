# sds commands and flags

Checked against `src/cli/` of sds 0.5.1. The installed binary is authoritative: if
`sds <command> --help` differs from this page, follow `--help` and tell the user (ground
rule 8).

## Global

| Flag | Meaning |
|---|---|
| `-c, --config <PATH>` | `sds.toml` to use. Default search: `$SDS_CONFIG`, `./sds.toml`, `~/.config/sds/sds.toml`, `/etc/sds/sds.toml`. Accepted by every command. |
| `-h, --help` | Help for `sds` or any command. |
| `-V, --version` | Prints `sds <version>`; a build from source prints a git-describe string such as `0.5.1-3-gabc1234`. |

## Commands that talk to a running daemon

`status`, `wake`, `sleep`, `hold`, `release` and `top` reach the daemon over its admin API.
They also accept:

| Flag | Meaning |
|---|---|
| `--url <URL>` | Base URL of the daemon (env `SDS_URL`; default derived from `admin.bind` in `sds.toml`). |
| `--token <TOKEN>` | Admin token (env `SDS_TOKEN`; default `admin.token` from `sds.toml` if readable). Prefer the environment or the file: a token on the command line ends up in the shell history and the transcript. |

| Command | Arguments and flags | Notes |
|---|---|---|
| `sds status` | `--json`, `--events` | Table of groups: state, members, requests, last request, sleeps in, idle timeout, notes (warnings and `last_error`). `--json` prints the full snapshot; `--events` adds the last 30 events. |
| `sds wake <group>` | `--wait` | Starts the group. `--wait` blocks until it is ready or until the group's `hold_timeout` (default 60 s); then it prints `error: 504 Gateway Timeout: timeout` although the start may still succeed (ground rule 3). |
| `sds sleep <group>` | | Stops the group now; the next request through the proxy wakes it. |
| `sds hold <group>` | | Stops the group and blocks wakes until `sds release`. No TTL option; the hold lives in the daemon's memory and ends with `sds release` or a restart of sds (ground rule 6). |
| `sds release <group>` | | Lifts a hold; the next request wakes the group. |
| `sds top` | `--demo` | Terminal dashboard (alias `sds attach`). Needs an interactive terminal, so the user runs it. `--demo` plays simulated groups without Docker or a daemon. |

Group states: `stopped`, `starting`, `running`, `stopping`, `failed`. A held group shows
`(held)` after its state in the table.

## Local commands (no daemon needed)

| Command | Arguments and flags | Notes |
|---|---|---|
| `sds check` | `--json` | Validates `sds.toml`, Docker access and the discovered groups without starting anything. Exit code 1 when something needs attention (warnings count). Talks to Docker directly, so it needs access to the Docker socket. |
| `sds config` | `--path` | Prints the effective configuration with `admin.token` shown as `<redacted>`; `--path` prints only the file that would be used. Use this instead of printing `sds.toml`. |
| `sds init` | `--path <PATH>` (default `sds.toml`), `--force`, `--stdout` | Writes a commented `sds.toml`; refuses to overwrite without `--force`. `--stdout` prints it instead. |
| `sds integrate <nginx\|caddy\|traefik> <group>` | `--host <HOST>` (default `app.example.com`), `--upstream <UPSTREAM>` (default `http://127.0.0.1:18080`), `--sds-url <SDS_URL>` (default from `admin.bind`), `--hold-timeout <SECONDS>` (default from `sds.toml`) | Prints a reverse-proxy snippet. **With `admin.token` set the snippet contains the token in clear text**: never print it unhidden, and write it only with the recipe in [setup.md](setup.md) (temporary file, `install -m 600` or `-m 640 -g <group>`, token guard only when `sds config` shows a token); pass the daemon's config with `-c` because an unreadable config silently gives a snippet without token (ground rule 5). |

## Commands the agent does not run on its own

These change the installation; run them only when the user asks (ground rule 4).

| Command | Arguments and flags | Notes |
|---|---|---|
| `sds run` | `--headless`, `--bind <ADDR>`, `--insecure` | The daemon itself. Usually started by the service unit, not by hand. |
| `sds service show` | `--binary <BINARY>`, `--user <USER>` (default `root`) | Prints the systemd unit (Linux) or launchd plist (macOS) without writing it. Safe to run. |
| `sds service install` | `--binary <BINARY>`, `--user <USER>` (default `root`) | Writes the unit to its system location (needs root) and prints the enable command. |
| `sds service uninstall` | | Prints the commands that remove the unit. |
| `sds update` | `--check`, `--tag <VERSION>`, `--force` | Replaces the binary with a signed release. `--check` only reports whether a newer release exists and is safe to run. A running daemon keeps the old code until it is restarted. |

## Errors

A failed command prints `error: …` to stderr and exits 1. Messages you will meet:

| Message | Meaning |
|---|---|
| `error: unknown group …` | No group of that name; list the names with `sds status`. |
| ``error: cannot reach sds at <URL> (is `sds run` running? …)`` | No daemon at that URL; check `sds config --path`, `--url` or whether the service runs. |
| `error: sds rejected the token; pass --token or set SDS_TOKEN` | `admin.token` is set and the token is missing or wrong; prefer `SDS_TOKEN` or a readable `sds.toml` over `--token`. |
| `error: 504 Gateway Timeout: timeout` | `sds wake --wait` hit `hold_timeout`; not a failed start (ground rule 3). |

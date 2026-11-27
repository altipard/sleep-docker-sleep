# Built-in MCP server: design

Status: proposal, nothing implemented. Verified against `develop` at `8841c7e`
(file:line references are to that commit; first written against `d8e932e`).
Statements marked **assumption** or **unverified** were not checked.

## Goal

Developers run more containers than they need. `sds` already knows which groups are idle
and can stop and start them. `sds mcp` lets a coding agent (Claude Code, Cursor, any MCP
client) use that knowledge: wake the stack before tests, see what is running that nobody
uses, find out why a group did not start, and see which containers `sds` does not manage.

Non-goals: a general Docker tool, log access, running commands in containers, replacing
`sds top` or the web overview.

## 1. Use cases and what the API gives today

| # | Use case | Today | Gap |
|---|---|---|---|
| 1 | Wake the stack before tests | Yes: `POST /groups/{g}/wake?wait=true` (`admin/api.rs:62-90`), CLI `sds wake --wait` (`cli/status.rs:166`) | Wait is bounded by the group's `hold_timeout` (default 60 s, `config/settings.rs:183`; `controller/handle.rs:160`), not `start_timeout` (90 s, `settings.rs:178`). A `504 timeout` therefore does **not** mean the start failed; it can still be `starting`. The adapter must keep polling. |
| 2 | Keep the stack up while working, release after | **No.** `hold` means the opposite: it stops the group and blocks wakes (`api.rs:110-113`). An operator wake does not count as activity (`api.rs:74-79` calls `force_wake`, not `touch`), so a woken group is stopped after `idle_timeout` (default 15 min, `settings.rs:177`; `actor.rs:476-481`) unless requests flow through the proxy. Tests that hit published ports directly are invisible to `sds` (`docs/handbook/limitations.md`, "counts requests, not connections"). | Needs a keep-awake lease with TTL (gap C). `handle.begin_inflight()` (`handle.rs:78`) already blocks idle stop and can back it. |
| 3 | Hold a group down during maintenance | `POST …/hold` and `…/release` work (`api.rs:111-128`) | A hold has **no expiry and no owner** and lives only in memory (`controller/registry.rs:35-36`, `docs/handbook/roadmap.md` "Hold that survives a restart"). A dead agent leaves the group down until a human releases it (gap A). |
| 4 | What is running that I do not need (managed groups) | Yes: status has `state`, `last_activity_ms`, `requests_total` and `sleeps_in_ms()` (`controller/snapshot.rs:40-67, 94`) | None for managed groups. The agent can rank by idle time and call `sleep`. |
| 5 | What is running that is **not** managed | **No.** `discover()` only emits labelled or `[groups.*]` containers (`docker/discovery.rs:56-140`). Everything else, and containers with `keep_awake` or `sds.enable=false`, is dropped without a trace (`discovery.rs:99-107`). `Backend::list()` returns all containers (`docker/backend.rs:74-76`) but no API route exposes it. `sds check` talks to Docker directly (`cli/check.rs:30,56`), so it only works where the Docker socket is reachable. | New read-only endpoint `GET /api/v1/containers` with a reason per container (gap B). `ContainerInfo` has only id, name, labels, running, status, health, has_healthcheck (`backend.rs:34-46`): no image, ports or resource usage. "Unneeded" can be judged only by state and compose project, not by CPU or memory. |
| 6 | Why did this group fail to start | Mostly: `last_error`, `warnings`, per-member `health` and `running` (`snapshot.rs:62-64`) plus the global event ring (`controller/events.rs:25-30, 72-74`; `status.events`). Events carry `group`, so the adapter can filter. | No container logs: `Backend` has no log call (`backend.rs:69-84`). Deliberately not added (see section 4). |
| 7 | Which containers need which labels | Partly: the label set is `config/labels.rs:100-115`; group naming rules `labels.rs:63`; patterns in `docs/handbook/labels.md`. Discovery warnings name broken containers (`discovery.rs:95,104`). | Needs the unmanaged list (gap B) and a pure function container -> suggested labels. The label list is a local `const` inside `parse_labels` (`labels.rs:100`), so it must be hoisted to share it with the suggestion text. Suggestions are text only; the server never edits compose files. |

### API gaps (summary)

- **A. Hold lease:** `hold` accepts no TTL and records no owner (`api.rs:111,120-128`;
  `registry.rs:85-103`).
- **B. Unmanaged containers:** no route; `Discovery` throws the reason away
  (`discovery.rs:35-40`).
- **C. Keep-awake lease:** nothing keeps a woken group up against `idle_timeout`.
- **D. Wake wait semantics:** `wait=true` ends at `hold_timeout`, not `start_timeout`
  (`handle.rs:160`). Solved in the adapter by polling, no daemon change.
- **E. Path encoding:** `ApiClient` interpolates the group name into the URL unescaped
  (`cli/client.rs:70,87,94`), and valid names may contain `?`, `#` or `%`
  (`config/file.rs:209-216` only forbids `/` and whitespace). With model-supplied input
  the adapter must validate against a known group and percent-encode. Harmless for the
  CLI, relevant for MCP.
- **F. Unknown query parameters are ignored** by axum `Query` (`WakeParams` has no
  `deny_unknown_fields`, `api.rs:55-60`). An older daemon would accept `hold?ttl=30m` and
  hold **without** expiry, silently. See section 6.

## 2. Transport and process model

| | `sds mcp` stdio adapter over `ApiClient` | HTTP endpoint inside the daemon |
|---|---|---|
| Auth | Reuses existing token resolution: `--token` / `SDS_TOKEN`, else `admin.token` from `sds.toml` (`cli/client.rs:29-40`, `cli/mod.rs:67-71`). Nothing new. The MCP client already authenticated the user by spawning the process. | New route needs its own auth and Origin/DNS-rebinding checks. The admin middleware covers only the routes in `admin/mod.rs:55-65`. Admin may be bound to a non-loopback address with a token (`admin/mod.rs`, `cli/check.rs:24`). |
| Local / remote daemon | Both: `--url` / `SDS_URL` (`mod.rs:67`). Remote works through the same TLS proxy as the API. | Local is easy; remote means exposing one more path. |
| Lifecycle | One child process per client session, dies with the client. A crash cannot take the daemon down. | Long-lived, shared by all agents; needs session identity and per-session state. |
| Docker-socket privileges | None. The adapter never talks to Docker; the daemon stays the only holder of the socket. | The privileged process gains a new, model-reachable attack surface. |
| Dependencies | `rmcp` with server + stdio, in the CLI path only. | `rmcp` + axum integration (`transport-streamable-http-server`, more crates) in the daemon. |
| Fit with existing code | `ApiClient` is already `pub` (`cli/mod.rs:17`) and tested against a mock axum server (`client.rs:102-171`). | New server stack next to the admin router. |
| Weak points | stdout is the protocol, so nothing may print to it; tracing must go to stderr (the daemon's `init_tracing` is private to `cli/run.rs:169`, the adapter needs its own). Older daemon after `sds update` (section 6). | The daemon cannot tell which agent did what without extra plumbing. |

**Recommendation: stdio adapter** (`sds mcp`), a thin layer over the admin API. Every
privileged action stays behind the existing admin API and token. HTTP can be added later
if a shared team daemon needs it; the tool layer should be transport-agnostic so that costs
only a transport.

Daemon work is limited to small, generally useful API additions (gaps A, B, C), each
useful without MCP.

## 3. Tools, resources, prompts

Naming: `sds_` prefix (tool names are only unique per server and clients merge servers;
names use `[A-Za-z0-9_.-]`, per the tools spec). Outputs: `structuredContent` plus the
same JSON as text, as the spec recommends. Free text from containers lives only in
fields named `untrusted_*` (section 4).

Annotations are hints and must be treated as untrusted by clients (spec text, fetched
below); real enforcement is the mode flag.

### MVP (read-only mode, no daemon change)

| Tool | Input | Output | Annotations | API |
|---|---|---|---|---|
| `sds_status` | `{}` | version, docker endpoint, `warnings[]`, `groups[]` (name, state, held, source, members, last_activity age, sleeps_in, `untrusted_error`), last 20 events | read-only, idempotent, closed-world | `GET /api/v1/status` |
| `sds_group` | `{group}` | full snapshot incl. settings; unknown group returns a tool error that lists known names | read-only, idempotent | `GET /api/v1/groups/{g}` |
| `sds_diagnose` | `{group}` | derived in the adapter: state, retry-pause hint (10 s, `docs/handbook/architecture.md`), unhealthy or stopped members, held flag, group warnings, group's last events, a short plain next-step hint | read-only, idempotent | status + group |

### MVP, operate mode (`--mode operate`)

| Tool | Input | Output | Annotations | API |
|---|---|---|---|---|
| `sds_wake` | `{group, wait_seconds?: 1..300, default 45}` | `result`: `ready`, `starting` (not an error, call `sds_group` again), `failed` (with error), `held`, `overloaded`, `gone`, `unknown_group`; plus snapshot summary | not read-only, not destructive, idempotent | `POST …/wake?wait=true`, then poll `GET …/{g}` once per second until the deadline |
| `sds_sleep` | `{group}` | `requested`, then polls up to 30 s for `stopped` | not read-only, **destructive** (stops containers; in-container state without volumes is lost), idempotent | `POST …/sleep` |

`sds_sleep` is marked destructive although the daemon does it automatically on idle:
conservative annotations make clients ask. There is no "sleep all" and no wildcard tool;
every write names exactly one existing group.

### Wait semantics and timeouts

- The adapter, not the daemon, owns the deadline. Default 45 s: MCP clients have their own
  tool-call timeouts (**unverified**, varies by client), and a timeout that kills the call
  loses the result. `starting` is a normal answer; the model polls.
- Mapping of `wake?wait=true` responses (`api.rs:80-88`): 200 `ready`; 502 `failed:<reason>`
  (`failed`); 504 (`hold_timeout` elapsed) and 202 `starting` continue polling until
  `wait_seconds`; 503 `overloaded`; 410 `gone`; 409 `held` (never override an operator's
  hold; the tool says who to ask).
- `wake` on a group already `running` returns `ready` at once (`handle.rs:141-144`).
- `ApiClient` has a 120 s request timeout (`client.rs:38`). Single calls stay below it
  because `wait=true` ends at `hold_timeout`.

### v1 additions (need daemon changes)

| Tool | Input | Notes |
|---|---|---|
| `sds_hold` | `{group, ttl_minutes: 1..240, default 30}` | Requires gap A. **TTL is mandatory in MCP.** The CLI keeps unlimited holds. |
| `sds_release` | `{group}` | Lifts a hold; does not start the group (`api.rs:115`). |
| `sds_keep_awake` | `{group, ttl_minutes: 1..240}` | Requires gap C. Renewable by calling again. Meant for "I run tests for 20 min against published ports". |
| `sds_unmanaged` | `{}` | Requires gap B. Per container: name, state, compose project/service, `reason` enum (`no_sds_labels`, `sds_enable_false`, `keep_awake`, `invalid_labels`, `missing_enable`, `duplicate_group`), sds.* labels only. |
| `sds_suggest_labels` | `{container}` | Pure function over the same data: proposes `sds.enable`, `sds.group`, health choice per `labels.md`. Output is text and a compose snippet; nothing is written. |

### Resources and prompts (later, optional)

Resources: `sds://status`, `sds://group/{name}`, `sds://labels` (label reference
generated from the same constant as the parser, so it cannot drift). Many clients handle
resources less well than tools (**assumption**), so tools stay the primary surface.

Prompts (templates, no behaviour): `sds_tidy_up` ("list idle groups, propose which to
sleep, do not act without confirmation"), `sds_why_not_starting <group>`,
`sds_label_my_stack`. Prompt text is static; no container text is interpolated into it.

### Lease design (no leaked holds)

Today: no expiry, no owner (see use case 3). Proposal:

- `POST /groups/{g}/hold?ttl=30m` (humantime, as in labels). `registry.held` becomes a map
  `name -> HoldInfo { until_ms: Option<u64>, owner: Option<String> }`
  (`registry.rs:35-39`). Without `ttl` behaviour is unchanged.
- Snapshot gains `held_until_ms` and `held_by` (both `Option`, `#[serde(default)]`, so an
  older client still parses them; `held` already works that way, `snapshot.rs:44`).
- Expiry is checked on the supervisor tick, calls the existing release path and emits the
  existing `Released` event with message "hold expired" (`events.rs:18`).
- `owner` is informational, taken from header `X-SDS-Client: mcp/<version>`; it is not an
  authorization input. It also gives an audit trail inside the daemon for free
  (event messages go to `/api/v1/events` and stderr, `events.rs:52-58`).
- A config ceiling `admin.max_hold` (e.g. 8 h) bounds any TTL.
- The adapter also tracks holds and keep-awake leases it created and releases them on
  stdin EOF or SIGTERM. That is best effort only; SIGKILL leaves the lease, so the daemon
  TTL is the actual guarantee.
- Keep-awake: `POST /groups/{g}/keepalive?ttl=…` stores a deadline and holds an
  `InflightGuard` (`handle.rs:78`) until then. Idle stop already honours `inflight`
  (`actor.rs:467`). Snapshot field `awake_until_ms`. A hold and a keep-awake on the same
  group are mutually exclusive (409).
- Lost on daemon restart, like holds today; the roadmap item for persistence covers both.

## 4. Security

### Modes and enabling writes

- **Read-only is the default.** `sds mcp` registers only the read tools. Writes need an
  explicit `--mode operate` (or `SDS_MCP_MODE=operate`) in the client's server entry.
  Reasons: agents often auto-approve tools; a repo-committed `.mcp.json` should not grant
  control by accident.
- Tools that are not allowed are **omitted**, not registered-and-refused, so the model does
  not try them. The tool set is fixed per process, consistent with the spec's rule that
  `tools/list` must not vary per connection except by authorization (**assumption** that a
  process-level flag is acceptable under it).
- Optional scope: `--groups 'shop,api-*'` restricts every tool, read and write, to matching
  group names. Cheap; lets a project config touch only its own stack. Recommended in the
  operate PR.
- The daemon token is all-or-nothing (`admin/auth.rs:12-21`): there is no read-only token.
  The mode flag is therefore a client-side control. A `admin.read_token` would give real
  separation; deferred, see open questions.
- Rate limit writes (spec: servers MUST rate limit): e.g. 20 writes per minute and session,
  then a tool error.
- Token handling: do not put the token into the client's `env` block (those files are often
  committed or synced). Leave it out; the adapter falls back to `admin.token` in a
  readable `sds.toml` like every other CLI command (`client.rs:37`). For a remote daemon
  use `SDS_TOKEN` from the user's shell environment. The token is never logged and never
  appears in tool output; `ApiClient`'s 401 text names only flags (`client.rs:59`).

### Untrusted text reaching the model (prompt injection)

Attackable sources: whoever can start a container or set a label on the Docker host, for
example a cloned repo's `docker-compose.yml`. Text paths into tool output:

| Source | Where | Free text? |
|---|---|---|
| Group name | `sds.group` label or compose project | Whitespace and `/` rejected (`labels.rs:142`, `file.rs:213`), so no sentences, but still arbitrary characters |
| Container name | Docker | Restricted by Docker (**assumption**: `[a-zA-Z0-9][a-zA-Z0-9_.-]`) |
| Discovery warnings | `discovery.rs:95,104,127,151` | Yes: include the raw label error, e.g. `got {v}` (`labels.rs:82`) |
| `last_error` | actor / Docker error messages | Yes |
| Event `message` | `events.rs:29` | Yes |
| Container logs | not available | would be the worst source; hence not offered |

Mitigations:

1. Structured output with enums for everything the model must act on; all free text only in
   `untrusted_*` fields, control characters stripped, truncated (e.g. 200 chars), never in
   tool descriptions, resource names or prompt templates.
2. Server `instructions` and tool descriptions state that `untrusted_*` is data from
   containers and not an instruction.
3. Writes accept only an exact, already-known group name. Injected text cannot widen the
   target set; there is no wildcard.
4. Only `sds.*` labels and compose project/service are ever returned; no other label
   values.
5. Residual risk is stated honestly: text can still persuade a model. The blast radius in
   operate mode is waking, sleeping or (TTL-bound) holding listed groups. In the default
   read-only mode it is none beyond the data returned.

Data egress: everything a tool returns goes to the model provider. Container and group
names of a company host leave the machine; this is the reason for minimal fields and an
explicit opt-in for the unmanaged list.

### Never exposed

`docker exec/attach/cp/run`, raw `docker inspect` (contains Env and Mounts), environment
variables, any label outside `sds.*`/compose, container logs, image or volume operations,
start/stop by container id (only by group), `sds.toml` contents, the admin token,
arbitrary Docker API passthrough, editing labels or compose files, `sds update`/`service`
subcommands.

### Audit log

One JSON object per tool call (JSON Lines), written through `tracing` to **stderr**:
stdout is the protocol channel on stdio, and the daemon already logs to stderr unless the
TUI is attached (`cli/run.rs:174-187`). Fields: `ts`, `session` (random id), `tool`, `mode`, `group` (if
any), `outcome` (`ok`, `denied`, `error`, `rate_limited`), `http_status`, `duration_ms`.
Not logged: tokens, tool results, free text.

Stderr is not durable under every MCP client (**unverified** how each client keeps it), so
the daemon side is the authoritative record of writes: the `X-SDS-Client` header turns into
event messages such as "sleep requested via mcp/0.4.0" in the daemon's own log stream. An
opt-in `--audit-file PATH` (append, mode 0600) is allowed because the adapter is a local
CLI, not a deployed service, but it is not the default. Twelve-factor applies to the
daemon, which keeps its single stream.

## 5. Rust SDK choice

Checked on 2026-11-25.

- Crate `rmcp`, official ("An official Rust Model Context Protocol SDK",
  `github.com/modelcontextprotocol/rust-sdk`), **3.11.0** published 2026-11-17, licence
  Apache-2.0, `rust_version = 1.88` on crates.io (matches our `rust-version = "1.88"`,
  `Cargo.toml:5`). Releases are frequent (3.6 to 3.11 between 2026-10-20 and 2026-11-17),
  so expect churn; 3.x is a recent major with a migration guide.
- Current MCP revision: **2026-07-28** ("Current" on the versioning page). It replaces the
  `initialize` handshake with per-request `_meta` and `server/discover`; revisions up to
  `2025-11-25` still use the handshake and most installed clients are likely to
  (**assumption**). rmcp README states it implements 2026-07-28 and stays compatible with
  2025-11-25 and earlier.
- Features needed: `server`, `macros` (both in `default`), `transport-io` (stdio). Not
  needed: `auth`, `client`, `transport-streamable-http-*`, `reqwest`. Do not enable
  `schemars` chrono extras beyond what `server` pulls in; **unverified** whether
  `default-features = false, features = ["server","macros","transport-io"]` drops
  `base64` safely.
- Dependency weight, measured by locking our own `Cargo.toml` plus rmcp with
  `server` + `transport-io`: `Cargo.lock` grows from 384 to **411 packages (+27)**, measured at `d8e932e`
  (`Cargo.lock` still has 384 packages at `8841c7e`):
  rmcp, rmcp-macros, schemars (+derive, dyn-clone, ref-cast), chrono (+iana-time-zone,
  windows crates), futures-executor, sha2/digest stack, pastey,
  unicode-normalization and others. Release binary size delta: **not measured**.
  New crates must pass `cargo audit` (not run).
- Throwaway prototype (scratchpad, not in the repo): an `rmcp` 3.11.0 server with one
  `#[tool]` carrying `read_only_hint`, compiles with stable and with `cargo +1.88.0 check`,
  and over stdio answered `initialize` (client version 2025-11-25), `tools/list` and
  `tools/call` correctly. **Not tested:** the 2026-07-28 `server/discover` path and any
  real MCP client.
- Hand-written JSON-RPC alternative: stdio is newline-delimited JSON and the MVP needs only
  `initialize`, `tools/list`, `tools/call`, `ping`; probably a few hundred lines
  (**estimate**). But two lifecycles now coexist (handshake and `_meta`/`server/discover`),
  and schema generation, error mapping, cancellation and future spec drift would be ours.

**Recommendation: `rmcp`**, pinned to a minor in `Cargo.toml`, behind a cargo feature `mcp`
(default on, so release binaries include it and packagers can opt out). Keep the tool layer
as plain async functions over an `McpBackend` trait so a later swap or a second transport
costs little and tests do not need rmcp.

## 6. Errors, versioning, tests, docs, client config

### Error behavior

Tool failures return `isError: true` with a short actionable text (the spec wants
execution errors visible to the model); JSON-RPC errors only for malformed requests or
unknown tools.

| Situation | Result |
|---|---|
| Daemon unreachable | tool error: "cannot reach sds at URL (is `sds run` running?)" (from `client.rs:55-57`) |
| 401 | "token rejected; set SDS_TOKEN or admin.token" |
| Unknown group (404, `api.rs:44-46`) | error listing known names, truncated |
| Held (409) | `held`, "operator hold; release it first" |
| Write in read-only mode | tool is not listed; a forced call gets an unknown-tool error |
| Daemon 5xx / bad JSON | tool error with status only, no body echo |

### Versioning against an older daemon

- Daemon `version` can be a git-describe string (`docs/handbook/architecture.md`), so do
  not parse semver; probe by behaviour. A route that does not exist returns an **empty
  404**, while a missing group returns JSON with `error` (`api.rs:45`). The adapter uses
  that to answer "daemon too old, restart it after `sds update`". This is a common state: the
  update replaces the binary, the running daemon keeps the old code.
- New snapshot fields are `Option`/`#[serde(default)]` so a new adapter parses an old
  daemon (`ApiClient` deserializes typed `StatusResponse`, `client.rs:64-67`). Unknown
  fields from a newer daemon are ignored by serde by default.
- **Hold TTL on an old daemon:** unknown query parameters are ignored (gap F), so the hold
  would be unlimited. After `hold?ttl=` the adapter must check the response or snapshot for
  `held_until_ms`; if absent it releases at once and reports "daemon does not support hold
  TTL". Same for keep-awake (old daemon: 404 route, safe).
- Protocol: advertise what rmcp negotiates; no custom handling.

### Tests

Same bar as `docs/development/production-hardening.tdd.md` (RED then GREEN per behaviour,
80 % line coverage, `cargo +1.88.0 check --locked`, clippy `-D warnings`).

- Tool layer against the mock axum server pattern (`cli/client.rs:102-171`) and against the
  real admin router with `test_support::registry` (`admin/tests.rs:24-29`).
- stdio contract via `assert_cmd` (`tests/cli.rs`): spawn `sds mcp` against a mock daemon,
  send `initialize`, `tools/list`, `tools/call`; assert **stdout contains only JSON-RPC
  lines**; assert write tools are absent in read-only mode; golden `tools/list` snapshot
  including annotations.
- Injection fixtures: group, warning, `last_error` containing instructions, ANSI and very
  long text; assert `untrusted_*` placement, stripping and truncation.
- Lease tests with paused tokio time: TTL expiry, renewal, cap, restart forgets, hold vs
  keep-awake exclusion, old-daemon hold TTL fallback.
- Wait semantics: 504 then ready within `wait_seconds`; failed start; held mid-wait
  (`handle.rs:165-183`).
- Audit line shape; no token in any log line.

### Docs

New handbook chapter `docs/handbook/mcp.md` (+ `SUMMARY.md`), updates to `api.md` and
`src/admin/assets/openapi.json` for each new route, `operations.md` (leases),
`roadmap.md` (remove "hold survives restart" only when done; add the read-only token),
README mention. `sds mcp --help` documents modes and env vars.

### Client config examples (format **unverified in this session**; check each client's docs)

Claude Code:

```bash
claude mcp add sds -- sds mcp                       # read-only
claude mcp add sds -- sds mcp --mode operate        # may wake and sleep
```

Project-scoped `.mcp.json` (also the usual shape for Cursor and similar clients):

```json
{
  "mcpServers": {
    "sds": { "command": "sds", "args": ["mcp", "--groups", "myproject*"] }
  }
}
```

Remote daemon: add `"env": { "SDS_URL": "https://sds.internal.example" }` and provide
`SDS_TOKEN` through the shell environment, not this file. Global flag order:
`sds --config PATH mcp`.

## 7. Implementation plan

Conventional Commits; `feat` releases a minor version, so PRs 1 to 3 are separate
release-worthy steps.

| PR | Content | Effort |
|---|---|---|
| 1 `feat(mcp): read-only stdio server` | `mcp` feature and rmcp dependency, `sds mcp`, `sds_status`, `sds_group`, `sds_diagnose`, sanitizer, stderr audit log, path encoding and group validation (gap E), tests, 1.88 and audit check | 2 d |
| 2 `feat(mcp): operate mode` | `--mode operate`, `sds_wake` with polling wait, `sds_sleep`, `--groups`, rate limit, `X-SDS-Client` header | 1.5 d |
| 3 `feat(admin): hold ttl and owner` | gap A: registry map, expiry tick, snapshot fields, `sds hold --ttl`, openapi, docs, tests | 2 d |
| 4 `feat(mcp): hold and release tools` | TTL mandatory, old-daemon fallback, release on exit | 1 d |
| 5 `feat(admin): list unmanaged containers` | gap B: `Discovery.unmanaged` with reasons, `GET /api/v1/containers`, hoist label list; `sds_unmanaged`, `sds_suggest_labels` | 2 d |
| 6 `feat(admin): keep-awake lease` | gap C, `sds_keep_awake`, restart-persistence follow-up stays on the roadmap | 1.5 d |
| 7 `docs(mcp): handbook, resources, prompts` | chapter, client configs, optional resources and prompts | 1 d |

About 11 working days. PR 1 delivers value on its own and carries no write risk.

### Risks

1. **Prompt injection through container text** is only mitigated, not solved; read-only
   default and exact-name writes bound it.
2. **Leaked holds** until PR 3 exists, so no hold tool before then. Old-daemon silent
   unlimited hold needs the explicit check.
3. **rmcp churn**: 3.x with a release every 1 to 2 weeks and a moving spec (2026-07-28 is new).
   Pin, keep the tool layer SDK-free.
4. **Dependency growth** (+27 crates, chrono and the sha2 stack) in a small daemon binary;
   audit and binary-size measurement missing.
5. **Idle semantics surprise**: agents may expect `wake` to keep a group up. The tool
   description and the result must say that it can sleep again after `idle_timeout`
   without requests.
6. **Data egress** of container names to the model provider.
7. Two instances on one daemon fight over groups (`limitations.md`); an agent waking
   groups is one more actor. Not new, but more visible.

### Open questions (recommended answer first)

1. Read-only by default, writes via `--mode operate`? **Yes.**
2. No hold tool until daemon-side TTL exists? **Yes**, hold/release come with PR 3 and 4.
3. Do you run tests against published ports (invisible to `sds`)? If yes, keep-awake
   lease is a v1 must; **recommend yes, PR 6 right after hold TTL.**
4. Ever expose container logs? **No.** Highest injection and secret risk; the agent has its
   own shell for `docker logs`.
5. Ship `rmcp` in the default binary behind a `mcp` feature, default on? **Yes.**
6. Audit to stderr plus daemon events only, file opt-in? **Yes.**
7. HTTP transport later? **Not now**; stdio plus `--url` covers remote daemons.
8. Add `--groups` scoping in PR 2? **Yes**, it is the cheap project-level safety.
9. Daemon read-only token (`admin.read_token`) for real separation? **Later**, when a
   shared team daemon exists.
10. Is it acceptable that `sds_unmanaged` sends names of all host containers to the model?
    **Only on explicit use; consider an opt-in flag for it.**

## Sources

- rmcp on crates.io (version, MSRV, features, dependencies via the crates.io API):
  https://crates.io/crates/rmcp and https://crates.io/api/v1/crates/rmcp
- rmcp repository and README (protocol revisions, lifecycle, features):
  https://github.com/modelcontextprotocol/rust-sdk
  and `crates/rmcp/src/model.rs` on `main` (`ProtocolVersion` constants)
- MCP versioning, current revision 2026-07-28:
  https://modelcontextprotocol.io/specification/versioning
- MCP tools (annotations are untrusted, structured content, error model, security
  considerations): https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- Inspiration: https://github.com/erickochen/purple (README mentions an MCP server with a
  read-only mode and a JSON Lines audit log; its wiki with the details was **not** read,
  nothing was copied).

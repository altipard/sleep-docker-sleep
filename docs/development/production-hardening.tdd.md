# Production hardening: TDD evidence

## Source and journeys

No external plan file was used. The journeys came from the local production audit:

- As an operator, I want authenticated reverse-proxy integrations so untrusted clients cannot control SDS.
- As an operator, I want bounded Docker operations so a sick daemon cannot wedge a group.
- As an operator, I want secrets redacted and updates verified before execution.
- As a maintainer, I want reproducible boundary tests, dependency auditing and coverage so regressions block releases.

## RED / GREEN checkpoints

| Behavior | RED evidence | GREEN evidence |
|---|---|---|
| Traefik token transport, waiting-page auth, config redaction, public error leakage | `65e7d53`: four focused tests failed for the intended reasons | `baf3fad`: all four pass; real Traefik contract later passes |
| Hanging Docker start, inspect and stop | `c6e6d8c`: each operation exceeded its deadline | `fc19172`: all hanging-backend tests and all 20 actor tests pass |
| Non-loopback admin exposure | Compile-time RED: missing exposure validation | `2b45007`: token or explicit `--insecure` required |
| CDN Swagger and persistent browser token | `1f2d96b`: CDN and `localStorage` assertions fail | `bf0a82a`: local embedded Swagger and `sessionStorage` pass |
| Unsigned self-update | `1c2ce32`: verification and release signing absent | `0426137`: valid/tampered signatures and OpenSSL interoperability pass |
| Invalid Traefik timeout field | Real Traefik v3.3.7: `field not found, node: forwardAuthTimeout`; unit regression added | Field removed; real authenticated request, sleep/wake and upstream token-stripping pass |
| TUI held-state visibility | `stopped (held)` absent in rendered dashboard | State column widened; rendering regression passes |

## Test specification

| Guarantee | Test surface | Result |
|---|---|---|
| Token never appears in Traefik URLs and is stripped before the application | `src/integrate/mod.rs`, `examples/traefik-central/smoke.sh` | PASS |
| Waiting page uses the same authentication and generic errors | `src/admin/tests.rs`, `just smoke` | PASS |
| Docker start/stop/inspect operations have deadlines | `src/controller/actor.rs` hanging mock backend | PASS |
| Update signatures are checked before installation; previous binary is retained | `src/cli/update.rs` unit tests | PASS |
| HTTP API, SSE, native HTTP/TCP proxies, registry and CLI work against real local sockets | `src/admin/tests.rs`, `src/proxy/tests.rs`, `src/controller/registry_tests.rs`, CLI modules | PASS |
| Rust version advertised by the package matches its dependencies | `cargo +1.88.0 check --locked` | PASS |
| Locked dependencies have no known RustSec advisory | `cargo audit` | PASS, 384 crates scanned |
| Line coverage remains at least 80% | `cargo llvm-cov --locked --all-features --workspace --ignore-filename-regex 'src/test_support.rs' --fail-under-lines 80` | PASS, 80.45% |

## Final verification

- `cargo test --locked --all-features`: 96 unit tests plus 8 CLI integration tests passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo audit`: passed with no reported vulnerability.
- `cargo +1.88.0 check --locked`: passed.
- `just smoke`: passed with real Docker, including auth and token redaction.
- `just smoke-traefik`: passed with real Traefik v3.3.7, including sleep/wake and token stripping.
- `cargo build --release --locked`: passed.
- `docker build -t sleep-docker-sleep:audit .`: passed; the container reports `sds 0.0.0-dev`
  when built without the release `SDS_VERSION` argument.

## Known gaps

This is engineering evidence, not a formal security certification. Interactive daemon/TUI
control flow and some concrete Docker-engine transport errors still have lower coverage
than the state machine and HTTP boundaries. No production traffic or external host was
used for testing; all application and proxy E2E resources were local and cleaned up.

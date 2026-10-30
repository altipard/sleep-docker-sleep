# Changelog

## Unreleased

### Security

- Require authentication for non-loopback admin binds and waiting pages; `--insecure` is now an explicit opt-in.
- Send Traefik tokens only to forward-auth, never in URLs or to the application.
- Redact tokens from `sds config`, embed Swagger locally, and remove Docker errors from public pages.
- Verify Ed25519 signatures before installing updates and retain the previous binary for rollback.

### Fixed

- Bound hanging Docker start, inspect and stop operations.
- Generate valid Traefik v3.3 configuration and preserve held-state visibility in the TUI.
- Correct the MSRV to Rust 1.88 and keep locked release and Docker builds reproducible.

### Verification

- Add API, proxy, registry, CLI and update tests, plus real Docker and Traefik smoke tests.
- Require at least 80% line coverage and run RustSec audits in CI.

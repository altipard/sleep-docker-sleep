//! The endpoint reverse proxies call before forwarding a request.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::SharedState;
use super::page::{OutcomeText, WaitPage, wants_html};
use crate::config::{UnknownGroupPolicy, WakeMode};
use crate::controller::{StateKind, WakeOutcome};

/// Query parameters understood by `/_sds/check/{group}`.
#[derive(Debug, serde::Deserialize)]
pub struct CheckParams {
    /// Override the group's `wake` setting (`hold` or `page`).
    pub mode: Option<String>,
    /// Status code for "not ready" answers. nginx `auth_request` only understands 401/403,
    /// Traefik and Caddy pass 503 through to the client. Only 4xx/5xx are accepted.
    pub deny: Option<u16>,
}

impl CheckParams {
    fn deny_status(&self) -> StatusCode {
        self.deny
            .and_then(|c| StatusCode::from_u16(c).ok())
            .filter(|c| c.is_client_error() || c.is_server_error())
            .unwrap_or(StatusCode::SERVICE_UNAVAILABLE)
    }

    fn wake_mode(&self) -> Option<WakeMode> {
        match self.mode.as_deref().map(str::parse::<WakeMode>) {
            Some(Ok(mode)) => Some(mode),
            Some(Err(e)) => {
                tracing::warn!("forward-auth: {e}; using the group's wake setting");
                None
            }
            None => None,
        }
    }
}

pub async fn check(
    State(state): State<SharedState>,
    Path(group): Path<String>,
    Query(params): Query<CheckParams>,
    headers: HeaderMap,
) -> Response {
    let html = wants_html(&headers);
    let deny = params.deny_status();

    let Some(handle) = state.registry.get(&group) else {
        return match state.config.admin.unknown_group {
            UnknownGroupPolicy::Allow => {
                // A typo in the proxy config must not take a site down. The warning is
                // visible in the logs and the event stream.
                tracing::warn!(%group, "forward-auth for unknown group; allowing request");
                (StatusCode::OK, [("x-sds-state", "unknown-group")]).into_response()
            }
            UnknownGroupPolicy::Deny => {
                tracing::warn!(%group, "forward-auth for unknown group; denying request");
                unknown_group_page(&group, deny).into_response(html)
            }
        };
    };

    let outcome = handle.ensure_ready_with(params.wake_mode()).await;
    respond(&group, outcome, html, deny)
}

/// Waiting page for proxies that cannot relay the check response body (nginx).
pub async fn wait(State(state): State<SharedState>, Path(group): Path<String>) -> Response {
    let Some(handle) = state.registry.get(&group) else {
        return unknown_group_page(&group, StatusCode::NOT_FOUND).into_response(true);
    };
    let snap = handle.snapshot();
    if snap.held && snap.state != StateKind::Running {
        return OutcomeText::of(&WakeOutcome::Held).page(&group, StatusCode::SERVICE_UNAVAILABLE).into_response(true);
    }
    match snap.state {
        StateKind::Running => WaitPage {
            group: &group,
            title: "Ready",
            message: "The service is up. Reloading…",
            status: StatusCode::OK,
            refresh_secs: Some(0),
            state: "running",
        }
        .into_response(true),
        StateKind::Failed if snap.in_retry_pause() => WaitPage {
            group: &group,
            title: "Start failed",
            message: &format!(
                "{} — sds retries on the next request.",
                snap.last_error.unwrap_or_else(|| "unknown error".into())
            ),
            status: StatusCode::SERVICE_UNAVAILABLE,
            refresh_secs: Some(10),
            state: "failed",
        }
        .into_response(true),
        StateKind::Failed | StateKind::Stopped | StateKind::Starting | StateKind::Stopping => {
            handle.request_wake();
            WaitPage {
                group: &group,
                title: "Your service will be up in a moment",
                message: "It was asleep to save resources and is starting now. This page reloads automatically.",
                status: StatusCode::SERVICE_UNAVAILABLE,
                refresh_secs: Some(3),
                state: &snap.state.to_string(),
            }
            .into_response(true)
        }
    }
}

fn unknown_group_page(group: &str, status: StatusCode) -> WaitPage<'_> {
    WaitPage {
        group,
        title: "Unknown service",
        message: "sds does not know this group. Check the reverse proxy configuration.",
        status,
        refresh_secs: None,
        state: "unknown-group",
    }
}

fn respond(group: &str, outcome: WakeOutcome, html: bool, deny: StatusCode) -> Response {
    if outcome == WakeOutcome::Ready {
        return (StatusCode::OK, [("x-sds-state", "running")]).into_response();
    }
    OutcomeText::of(&outcome).page(group, deny).into_response(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(mode: Option<&str>, deny: Option<u16>) -> CheckParams {
        CheckParams { mode: mode.map(str::to_owned), deny }
    }

    #[test]
    fn deny_accepts_only_error_codes() {
        assert_eq!(params(None, None).deny_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(params(None, Some(401)).deny_status(), StatusCode::UNAUTHORIZED);
        assert_eq!(params(None, Some(502)).deny_status(), StatusCode::BAD_GATEWAY);
        // 2xx/3xx would turn "not ready" into an allow or a redirect.
        assert_eq!(params(None, Some(200)).deny_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(params(None, Some(302)).deny_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(params(None, Some(99)).deny_status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn mode_falls_back_to_group_setting_when_invalid() {
        assert_eq!(params(Some("page"), None).wake_mode(), Some(WakeMode::Page));
        assert_eq!(params(Some("HOLD"), None).wake_mode(), Some(WakeMode::Hold));
        assert_eq!(params(Some("bogus"), None).wake_mode(), None);
        assert_eq!(params(None, None).wake_mode(), None);
    }
}

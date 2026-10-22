//! The endpoint reverse proxies call before forwarding a request.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::SharedState;
use super::page::{WaitPage, wants_html};
use crate::config::WakeMode;
use crate::controller::{StateKind, WakeOutcome};

/// Query parameters understood by `/_sds/check/{group}`.
#[derive(Debug, serde::Deserialize)]
pub struct CheckParams {
    /// Override the group's `wake` setting (`hold` or `page`).
    pub mode: Option<String>,
    /// Status code for "not ready" answers. nginx `auth_request` only understands 401/403,
    /// Traefik and Caddy pass 503 through to the client.
    pub deny: Option<u16>,
}

pub async fn check(
    State(state): State<SharedState>,
    Path(group): Path<String>,
    Query(params): Query<CheckParams>,
    headers: HeaderMap,
) -> Response {
    let html = wants_html(&headers);
    let deny = params.deny.and_then(|c| StatusCode::from_u16(c).ok()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);

    let Some(handle) = state.registry.get(&group) else {
        // Fail open: a typo in the proxy config must not take a site down. The warning is
        // visible in the logs and the event stream.
        tracing::warn!(%group, "forward-auth for unknown group; allowing request");
        return (StatusCode::OK, [("x-sds-state", "unknown-group")]).into_response();
    };

    let outcome = match params.mode.as_deref().map(str::parse::<WakeMode>) {
        Some(Ok(WakeMode::Page)) => {
            handle.touch();
            if handle.is_running() {
                WakeOutcome::Ready
            } else {
                let snap = handle.snapshot();
                if snap.state == StateKind::Failed
                    && crate::controller::epoch_ms().saturating_sub(snap.state_since_ms)
                        < crate::controller::FAILED_RETRY_INTERVAL_MS
                {
                    WakeOutcome::Failed(snap.last_error.unwrap_or_default())
                } else {
                    handle.request_wake();
                    WakeOutcome::Starting
                }
            }
        }
        Some(Ok(WakeMode::Hold)) => {
            // Same as ensure_ready but forcing hold even if the group says page.
            handle.touch();
            if handle.is_running() {
                WakeOutcome::Ready
            } else {
                let snap = handle.snapshot();
                handle.request_wake();
                match handle.activity().try_wait(snap.settings.max_waiters) {
                    Some(_slot) => handle.wait_ready(snap.settings.hold_timeout, snap.wakes_total).await,
                    None => WakeOutcome::Overloaded,
                }
            }
        }
        _ => handle.ensure_ready().await,
    };

    respond(&group, outcome, html, deny)
}

/// Waiting page for proxies that cannot relay the check response body (nginx).
pub async fn wait(State(state): State<SharedState>, Path(group): Path<String>, headers: HeaderMap) -> Response {
    let _ = &headers;
    let html = true;
    let Some(handle) = state.registry.get(&group) else {
        return WaitPage {
            group: &group,
            title: "Unknown service",
            message: "sds does not know this group. Check the reverse proxy configuration.",
            status: StatusCode::NOT_FOUND,
            refresh_secs: None,
            state: "unknown-group",
        }
        .into_response(html);
    };
    let snap = handle.snapshot();
    match snap.state {
        StateKind::Running => WaitPage {
            group: &group,
            title: "Ready",
            message: "The service is up. Reloading…",
            status: StatusCode::OK,
            refresh_secs: Some(0),
            state: "running",
        }
        .into_response(html),
        StateKind::Failed => WaitPage {
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
        .into_response(html),
        StateKind::Stopped | StateKind::Starting | StateKind::Stopping => {
            handle.request_wake();
            WaitPage {
                group: &group,
                title: "Waking up",
                message: "The service was asleep and is starting now. This page reloads automatically.",
                status: StatusCode::SERVICE_UNAVAILABLE,
                refresh_secs: Some(3),
                state: &snap.state.to_string(),
            }
            .into_response(html)
        }
    }
}

fn respond(group: &str, outcome: WakeOutcome, html: bool, deny: StatusCode) -> Response {
    match outcome {
        WakeOutcome::Ready => (StatusCode::OK, [("x-sds-state", "running")]).into_response(),
        WakeOutcome::Starting => WaitPage {
            group,
            title: "Waking up",
            message: "The service was asleep and is starting now. This page reloads automatically.",
            status: deny,
            refresh_secs: Some(3),
            state: "starting",
        }
        .into_response(html),
        WakeOutcome::Timeout => WaitPage {
            group,
            title: "Still starting",
            message: "The service is taking longer than expected. This page keeps retrying.",
            status: deny,
            refresh_secs: Some(5),
            state: "starting",
        }
        .into_response(html),
        WakeOutcome::Failed(reason) => WaitPage {
            group,
            title: "Start failed",
            message: &format!("{reason} — sds retries shortly."),
            status: deny,
            refresh_secs: Some(10),
            state: "failed",
        }
        .into_response(html),
        WakeOutcome::Overloaded => WaitPage {
            group,
            title: "Busy",
            message: "Too many requests are waiting for this service to start. Please retry in a moment.",
            status: deny,
            refresh_secs: Some(5),
            state: "overloaded",
        }
        .into_response(html),
        WakeOutcome::Gone => WaitPage {
            group,
            title: "Service removed",
            message: "The service disappeared while waiting.",
            status: deny,
            refresh_secs: Some(10),
            state: "gone",
        }
        .into_response(html),
    }
}

#[allow(dead_code)]
fn _unused(_: HashMap<String, String>) {}

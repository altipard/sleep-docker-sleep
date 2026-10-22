use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};

use super::SharedState;
use crate::controller::{GroupSnapshot, SdsEvent, WakeOutcome, epoch_ms};

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub version: String,
    pub now_ms: u64,
    pub uptime_ms: u64,
    pub docker: String,
    pub last_discovery_ms: u64,
    pub warnings: Vec<String>,
    pub groups: Vec<GroupSnapshot>,
    pub events: Vec<SdsEvent>,
}

pub fn build_status(state: &SharedState) -> StatusResponse {
    StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        now_ms: epoch_ms(),
        uptime_ms: u64::try_from(state.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        docker: state.registry.docker_endpoint.clone(),
        last_discovery_ms: state.registry.last_discovery_ms(),
        warnings: state.registry.discovery_warnings(),
        groups: state.registry.snapshots(),
        events: state.registry.bus().recent(),
    }
}

pub async fn status(State(state): State<SharedState>) -> Json<StatusResponse> {
    Json(build_status(&state))
}

fn not_found(group: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": format!("unknown group `{group}`") }))).into_response()
}

pub async fn group(State(state): State<SharedState>, Path(group): Path<String>) -> Response {
    match state.registry.get(&group) {
        Some(h) => Json(h.snapshot()).into_response(),
        None => not_found(&group),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct WakeParams {
    /// Block until the group is ready (or `hold_timeout` elapses).
    #[serde(default)]
    pub wait: bool,
}

pub async fn wake(
    State(state): State<SharedState>,
    Path(group): Path<String>,
    Query(params): Query<WakeParams>,
) -> Response {
    let Some(handle) = state.registry.get(&group) else {
        return not_found(&group);
    };
    if !params.wait {
        handle.request_wake();
        return (StatusCode::ACCEPTED, Json(serde_json::json!({ "group": group, "requested": "wake" })))
            .into_response();
    }
    let snap = handle.snapshot();
    handle.request_wake();
    let outcome = if handle.is_running() {
        WakeOutcome::Ready
    } else {
        handle.wait_ready(snap.settings.hold_timeout.max(Duration::from_secs(1)), snap.wakes_total).await
    };
    let (code, text) = match &outcome {
        WakeOutcome::Ready => (StatusCode::OK, "running".to_owned()),
        WakeOutcome::Timeout => (StatusCode::GATEWAY_TIMEOUT, "timeout".to_owned()),
        WakeOutcome::Failed(r) => (StatusCode::BAD_GATEWAY, format!("failed: {r}")),
        WakeOutcome::Starting => (StatusCode::ACCEPTED, "starting".to_owned()),
        WakeOutcome::Overloaded => (StatusCode::SERVICE_UNAVAILABLE, "overloaded".to_owned()),
        WakeOutcome::Gone => (StatusCode::GONE, "gone".to_owned()),
    };
    (code, Json(serde_json::json!({ "group": group, "result": text, "snapshot": handle.snapshot() }))).into_response()
}

pub async fn sleep(State(state): State<SharedState>, Path(group): Path<String>) -> Response {
    match state.registry.get(&group) {
        Some(h) => {
            h.request_sleep();
            (StatusCode::ACCEPTED, Json(serde_json::json!({ "group": group, "requested": "sleep" }))).into_response()
        }
        None => not_found(&group),
    }
}

/// SSE stream: a full `status` document every 2 s and on every event, plus each `event`.
pub async fn events(State(state): State<SharedState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.registry.bus().subscribe();
    let stream = async_stream(move |tx| async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            let status_json = serde_json::to_string(&build_status(&state)).unwrap_or_else(|_| "{}".into());
            if tx.send(Event::default().event("status").data(status_json)).await.is_err() {
                return;
            }
            tokio::select! {
                _ = tick.tick() => {}
                ev = rx.recv() => match ev {
                    Ok(ev) => {
                        let data = serde_json::to_string(&ev).unwrap_or_default();
                        if tx.send(Event::default().event("event").data(data)).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Tiny helper: run `producer` with a sender and expose the receiver as a stream.
fn async_stream<F, Fut>(producer: F) -> impl Stream<Item = Result<Event, Infallible>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Event>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(producer(tx));
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|ev| (Ok(ev), rx)) })
}

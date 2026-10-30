//! Admin HTTP server: JSON API, forward-auth endpoint for reverse proxies, waiting page,
//! server-sent events and the embedded web overview.
//!
//! Routes (prefixing `/api` and `/_sds/check` with a bearer token when configured):
//!
//! | Method | Path                          | Purpose                                   |
//! |--------|-------------------------------|-------------------------------------------|
//! | GET    | `/`                           | web overview                              |
//! | GET    | `/healthz`                    | liveness of `sds` itself                  |
//! | GET    | `/docs`                       | Swagger UI for the API                    |
//! | GET    | `/openapi.json`               | OpenAPI specification                     |
//! | GET    | `/_sds/check/{group}`         | forward-auth: 200 when running, else wake |
//! | GET    | `/_sds/wait/{group}`          | waiting page (for nginx `error_page`)     |
//!
//! `/_sds/wait` triggers a wake and is protected by the same token as the check endpoint.
//! nginx adds that token in its internal waiting-page location, so browsers never see it.
//! | GET    | `/api/v1/status`              | everything the UI shows                   |
//! | GET    | `/api/v1/groups/{group}`      | one group                                 |
//! | POST   | `/api/v1/groups/{group}/wake` | start now (`?wait=true` blocks until ready)|
//! | POST   | `/api/v1/groups/{group}/sleep`| stop now                                  |
//! | POST   | `/api/v1/groups/{group}/hold` | stop and ignore wakes until released      |
//! | POST   | `/api/v1/groups/{group}/release`| lift a hold                             |
//! | GET    | `/api/v1/events`              | SSE: `status` and `event` messages        |

pub mod api;
mod auth;
mod forward_auth;
mod page;
pub use page::{OutcomeText, page_for_outcome};
mod web;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::controller::Registry;

pub struct AppState {
    pub registry: Arc<Registry>,
    pub config: Arc<Config>,
    pub started_at: Instant,
}

pub type SharedState = Arc<AppState>;

pub fn router(state: SharedState) -> Router {
    let protected = Router::new()
        .route("/api/v1/status", get(api::status))
        .route("/api/v1/groups/{group}", get(api::group))
        .route("/api/v1/groups/{group}/wake", post(api::wake))
        .route("/api/v1/groups/{group}/sleep", post(api::sleep))
        .route("/api/v1/groups/{group}/hold", post(api::hold))
        .route("/api/v1/groups/{group}/release", post(api::release))
        .route("/api/v1/events", get(api::events))
        .route("/_sds/check/{group}", get(forward_auth::check).head(forward_auth::check))
        .route("/_sds/wait/{group}", get(forward_auth::wait))
        .layer(axum::middleware::from_fn_with_state(Arc::clone(&state), auth::require_token));

    Router::new()
        .route("/", get(web::index))
        .route("/logo.svg", get(web::logo))
        .route("/docs", get(web::docs))
        .route("/swagger-ui.css", get(web::swagger_css))
        .route("/swagger-ui-bundle.js", get(web::swagger_js))
        .route("/openapi.json", get(web::openapi))
        .route("/healthz", get(|| async { "ok" }))
        .merge(protected)
        .with_state(state)
}

/// Serve until `cancel` fires. Binds before returning the future's first poll result so a
/// port clash surfaces at startup.
pub async fn serve(state: SharedState, cancel: CancellationToken) -> Result<()> {
    let bind = state.config.admin.bind;
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("admin: bind {bind}"))?;
    tracing::info!(%bind, "admin API listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .context("admin server failed")
}

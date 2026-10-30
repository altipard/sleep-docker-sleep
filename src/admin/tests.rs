use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::{AppState, router};
use crate::config::{Config, GroupSettings, HealthSpec, WakeMode};
use crate::test_support;

fn settings() -> GroupSettings {
    GroupSettings {
        start_grace: Duration::ZERO,
        health: HealthSpec::Grace,
        wake: WakeMode::Page,
        ..GroupSettings::default()
    }
}

fn state(running: bool) -> Arc<AppState> {
    let (registry, _backend) = test_support::registry("shop", &["app"], running, settings());
    let mut config = Config::default();
    config.admin.token = Some("secret".into());
    Arc::new(AppState { registry, config: Arc::new(config), started_at: Instant::now() })
}

async fn request(app: Router, method: Method, uri: &str, token: Option<&str>) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("x-sds-token", token);
    }
    app.oneshot(request.body(Body::empty()).unwrap()).await.unwrap()
}

#[tokio::test]
async fn public_assets_are_local_and_control_routes_require_headers() {
    let app = router(state(false));
    for path in ["/healthz", "/", "/docs", "/openapi.json", "/logo.svg", "/swagger-ui.css", "/swagger-ui-bundle.js"] {
        let response = request(app.clone(), Method::GET, path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let content_type = response.headers().get("content-type").expect("content type");
        assert!(!content_type.is_empty());
    }
    for path in ["/api/v1/status", "/_sds/check/shop?token=secret", "/_sds/wait/shop"] {
        let response = request(app.clone(), Method::GET, path, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn status_and_operator_actions_cover_success_and_not_found() {
    let app = router(state(true));
    let response = request(app.clone(), Method::GET, "/api/v1/status", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["groups"][0]["name"], "shop");
    assert_eq!(status["docker"], "mock://docker");

    let response = request(app.clone(), Method::GET, "/api/v1/groups/shop", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = request(app.clone(), Method::GET, "/api/v1/groups/missing", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    for action in ["sleep", "hold", "release", "wake"] {
        let response =
            request(app.clone(), Method::POST, &format!("/api/v1/groups/shop/{action}"), Some("secret")).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{action}");
    }
    let response = request(app.clone(), Method::POST, "/api/v1/groups/shop/wake?wait=true", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = request(app.clone(), Method::POST, "/api/v1/groups/missing/sleep", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn waiting_and_check_pages_share_generic_public_errors() {
    let app = router(state(false));
    let response = request(app.clone(), Method::GET, "/_sds/wait/shop", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("sleep-docker-sleep"));
    assert!(!html.contains("id-app"));

    let response = request(app.clone(), Method::GET, "/_sds/check/missing", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-sds-state").unwrap(), "unknown-group");
}

#[tokio::test]
async fn running_groups_are_ready_and_unknown_groups_can_fail_closed() {
    let ready = state(true);
    let handle = ready.registry.get("shop").unwrap();
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(
        Duration::from_secs(1),
        snapshots.wait_for(|s| s.state == crate::controller::StateKind::Running),
    )
    .await
    .unwrap()
    .unwrap();
    let app = router(ready);
    for path in ["/_sds/check/shop", "/_sds/wait/shop"] {
        let response = request(app.clone(), Method::GET, path, Some("secret")).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }

    let mut state = state(false);
    let mut config = Config::default();
    config.admin.token = Some("secret".into());
    config.admin.unknown_group = crate::config::UnknownGroupPolicy::Deny;
    Arc::get_mut(&mut state).unwrap().config = Arc::new(config);
    let response = request(router(state), Method::GET, "/_sds/check/missing", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn event_stream_emits_an_initial_status_document() {
    let app = router(state(true));
    let response = request(app, Method::GET, "/api/v1/events", Some("secret")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let mut text = String::new();
    for _ in 0..4 {
        let frame = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("initial SSE frame")
            .expect("stream remains open")
            .unwrap();
        text.push_str(&String::from_utf8_lossy(&frame));
        if text.contains("\"groups\"") {
            break;
        }
    }
    assert!(text.contains("status"), "unexpected SSE payload: {text}");
    assert!(text.contains("\"groups\""), "unexpected SSE payload: {text}");
}

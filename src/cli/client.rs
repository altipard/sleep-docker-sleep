//! HTTP client for the admin API, used by `status`, `wake`, `sleep`, `hold` and `release`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;

use super::ClientArgs;
use crate::admin::api::StatusResponse;
use crate::config;
use crate::controller::GroupSnapshot;

/// Base URL of the admin API as seen from this host. A wildcard bind is reached via
/// loopback; formatting through `SocketAddr` puts IPv6 literals in brackets.
pub(super) fn admin_url(bind: SocketAddr) -> String {
    let ip = if bind.ip().is_unspecified() { Ipv4Addr::LOCALHOST.into() } else { bind.ip() };
    format!("http://{}", SocketAddr::new(ip, bind.port()))
}

pub struct ApiClient {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl ApiClient {
    /// Resolve URL and token from flags, environment and (if readable) sds.toml.
    pub fn from_args(explicit_config: Option<&Path>, args: &ClientArgs) -> Result<Self> {
        let cfg = config::load(explicit_config).map(|(c, _)| c).ok();
        let base = match &args.url {
            Some(u) => u.trim_end_matches('/').to_owned(),
            None => {
                admin_url(cfg.as_ref().map(|c| c.admin.bind).unwrap_or_else(|| config::Config::default().admin.bind))
            }
        };
        let token = args.token.clone().or_else(|| cfg.and_then(|c| c.admin.token));
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build()?;
        Ok(Self { base, token, http })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            r = r.header("x-sds-token", t);
        }
        r
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = r.send().await.with_context(|| {
            format!("cannot reach sds at {} (is `sds run` running? use --url to point elsewhere)", self.base)
        })?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            bail!("sds rejected the token; pass --token or set SDS_TOKEN");
        }
        Ok(resp)
    }

    pub async fn status(&self) -> Result<StatusResponse> {
        let resp = self.send(self.req(reqwest::Method::GET, "/api/v1/status")).await?;
        Ok(resp.error_for_status()?.json().await?)
    }

    pub async fn wake(&self, group: &str, wait: bool) -> Result<(StatusCode, serde_json::Value)> {
        let path = format!("/api/v1/groups/{group}/wake{}", if wait { "?wait=true" } else { "" });
        let resp = self.send(self.req(reqwest::Method::POST, &path)).await?;
        let code = resp.status();
        let body = resp.json().await.unwrap_or(serde_json::Value::Null);
        Ok((code, body))
    }

    pub async fn sleep(&self, group: &str) -> Result<(StatusCode, serde_json::Value)> {
        self.action(group, "sleep").await
    }

    /// `true` puts the group on hold, `false` releases it.
    pub async fn set_hold(&self, group: &str, held: bool) -> Result<(StatusCode, serde_json::Value)> {
        self.action(group, if held { "hold" } else { "release" }).await
    }

    async fn action(&self, group: &str, action: &str) -> Result<(StatusCode, serde_json::Value)> {
        let resp = self.send(self.req(reqwest::Method::POST, &format!("/api/v1/groups/{group}/{action}"))).await?;
        let code = resp.status();
        let body = resp.json().await.unwrap_or(serde_json::Value::Null);
        Ok((code, body))
    }

    pub async fn group(&self, group: &str) -> Result<Option<GroupSnapshot>> {
        let resp = self.send(self.req(reqwest::Method::GET, &format!("/api/v1/groups/{group}"))).await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(resp.error_for_status()?.json().await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router};

    #[test]
    fn admin_url_brackets_ipv6() {
        assert_eq!(admin_url("127.0.0.1:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("0.0.0.0:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("[::1]:9900".parse().unwrap()), "http://[::1]:9900");
        assert_eq!(admin_url("[::]:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("[fd00::5]:9900".parse().unwrap()), "http://[fd00::5]:9900");
    }

    async fn api(method: reqwest::Method, uri: Uri, headers: HeaderMap) -> Response {
        if headers.get("x-sds-token").and_then(|v| v.to_str().ok()) != Some("secret") {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        match (method.as_str(), uri.path()) {
            ("GET", "/api/v1/status") => Json(serde_json::json!({
                "version": "test",
                "now_ms": 1,
                "uptime_ms": 2,
                "docker": "mock",
                "last_discovery_ms": 3,
                "warnings": [],
                "groups": [],
                "events": []
            }))
            .into_response(),
            ("GET", "/api/v1/groups/shop") => {
                Json(crate::test_support::snapshot("shop", crate::controller::StateKind::Running)).into_response()
            }
            ("POST", "/api/v1/groups/shop/wake") => {
                (StatusCode::OK, Json(serde_json::json!({"result": "running"}))).into_response()
            }
            ("POST", "/api/v1/groups/shop/sleep" | "/api/v1/groups/shop/hold" | "/api/v1/groups/shop/release") => {
                (StatusCode::ACCEPTED, Json(serde_json::json!({"requested": "ok"}))).into_response()
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    #[tokio::test]
    async fn client_sends_tokens_and_handles_all_operator_responses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, Router::new().fallback(api)).await.unwrap() });
        let args = crate::cli::ClientArgs { url: Some(format!("http://{addr}")), token: Some("secret".into()) };
        let client = ApiClient::from_args(None, &args).unwrap();
        assert_eq!(client.base(), format!("http://{addr}"));
        assert_eq!(client.status().await.unwrap().version, "test");
        assert_eq!(client.wake("shop", true).await.unwrap().0, StatusCode::OK);
        assert_eq!(client.sleep("shop").await.unwrap().0, StatusCode::ACCEPTED);
        assert_eq!(client.set_hold("shop", true).await.unwrap().0, StatusCode::ACCEPTED);
        assert_eq!(client.set_hold("shop", false).await.unwrap().0, StatusCode::ACCEPTED);
        assert_eq!(client.group("shop").await.unwrap().unwrap().name, "shop");
        assert!(client.group("missing").await.unwrap().is_none());

        let wrong = ApiClient::from_args(
            None,
            &crate::cli::ClientArgs { url: Some(format!("http://{addr}")), token: Some("wrong".into()) },
        )
        .unwrap();
        assert!(wrong.status().await.unwrap_err().to_string().contains("rejected the token"));
        server.abort();
    }
}

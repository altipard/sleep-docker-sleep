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
    use super::admin_url;

    #[test]
    fn admin_url_brackets_ipv6() {
        assert_eq!(admin_url("127.0.0.1:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("0.0.0.0:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("[::1]:9900".parse().unwrap()), "http://[::1]:9900");
        assert_eq!(admin_url("[::]:9900".parse().unwrap()), "http://127.0.0.1:9900");
        assert_eq!(admin_url("[fd00::5]:9900".parse().unwrap()), "http://[fd00::5]:9900");
    }
}

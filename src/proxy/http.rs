//! Layer-7 proxy: understands requests, serves the waiting page, passes upgrades
//! (WebSockets) through, streams bodies without buffering them.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ServerBuilder;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::admin::page_for_outcome;
use crate::config::ProxySettings;
use crate::controller::{GroupHandle, InflightGuard, WakeOutcome};

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the upstream may take to answer with response headers. Bodies stream without
/// a limit (downloads, SSE); only a silent upstream is cut off.
const UPSTREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(60);
/// Headers that describe this hop, not the message (RFC 9110 §7.6.1); never forwarded.
const HOP_BY_HOP: &[&str] =
    &["connection", "keep-alive", "proxy-connection", "proxy-authenticate", "proxy-authorization", "te", "trailer"];

type OutBody = BoxBody<Bytes, hyper::Error>;

/// Keeps the in-flight guard alive until the response body has been fully sent.
struct GuardedBody {
    inner: Pin<Box<OutBody>>,
    _guard: InflightGuard,
}

impl Body for GuardedBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.inner.as_mut().poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

fn full(text: impl Into<Bytes>) -> OutBody {
    Full::new(text.into()).map_err(|never| match never {}).boxed()
}

/// Accept loop; runs until the listener is dropped by the caller.
pub async fn serve(
    listener: Arc<TcpListener>,
    handle: Arc<GroupHandle>,
    settings: ProxySettings,
    limit: Arc<Semaphore>,
) {
    let client: Client<HttpConnector, Incoming> =
        Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).set_host(false).build_http();
    let upstream: Arc<str> = Arc::from(settings.upstream.as_str());
    loop {
        let Ok(permit) = Arc::clone(&limit).acquire_owned().await else {
            return;
        };
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(group = %handle.name(), "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let handle = Arc::clone(&handle);
        let client = client.clone();
        let upstream = Arc::clone(&upstream);
        tokio::spawn(async move {
            let _permit = permit;
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let handle = Arc::clone(&handle);
                let client = client.clone();
                let upstream = Arc::clone(&upstream);
                async move { Ok::<_, std::convert::Infallible>(proxy_request(req, handle, client, &upstream, peer).await) }
            });
            let mut builder = ServerBuilder::new(TokioExecutor::new());
            builder.http1().timer(TokioTimer::new()).header_read_timeout(HEADER_READ_TIMEOUT).keep_alive(true);
            if let Err(e) = builder.serve_connection_with_upgrades(io, service).await {
                tracing::debug!(%peer, "connection ended: {e}");
            }
        });
    }
}

async fn proxy_request(
    mut req: Request<Incoming>,
    handle: Arc<GroupHandle>,
    client: Client<HttpConnector, Incoming>,
    upstream: &str,
    peer: std::net::SocketAddr,
) -> Response<OutBody> {
    let wants_html =
        req.headers().get(hyper::header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("text/html"));
    let guard = handle.begin_inflight();
    match handle.ensure_ready().await {
        WakeOutcome::Ready => {}
        outcome => {
            let (status, html, json, retry) = page_for_outcome(handle.name(), &outcome);
            let mut resp = Response::builder().status(status);
            if wants_html {
                resp = resp.header(CONTENT_TYPE, "text/html; charset=utf-8").header("refresh", retry.to_string());
            } else {
                resp = resp.header(CONTENT_TYPE, "application/json");
            }
            return resp
                .header(RETRY_AFTER, retry.to_string())
                .header("cache-control", "no-store")
                .body(full(if wants_html { html } else { json }))
                .unwrap_or_else(|_| Response::new(full("")));
        }
    }

    // Rewrite the request line to an absolute URI for the HTTP/1 client.
    let path = req.uri().path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "/".into());
    let target = match format!("http://{upstream}{path}").parse::<hyper::Uri>() {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(group = %handle.name(), "bad upstream uri: {e}");
            return error_response(StatusCode::BAD_GATEWAY, "bad upstream");
        }
    };
    *req.uri_mut() = target;
    let is_upgrade = req.headers().contains_key(hyper::header::UPGRADE);
    strip_hop_by_hop(req.headers_mut(), is_upgrade);
    let forwarded = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|v| format!("{v}, {}", peer.ip()))
        .unwrap_or_else(|| peer.ip().to_string());
    if let Ok(v) = HeaderValue::from_str(&forwarded) {
        req.headers_mut().insert("x-forwarded-for", v);
    }
    req.headers_mut().entry("x-forwarded-proto").or_insert_with(|| HeaderValue::from_static("http"));

    let client_upgrade = if is_upgrade { Some(hyper::upgrade::on(&mut req)) } else { None };

    let mut resp = match tokio::time::timeout(UPSTREAM_HEADER_TIMEOUT, client.request(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(group = %handle.name(), %upstream, "upstream request failed: {e}");
            return error_response(StatusCode::BAD_GATEWAY, "upstream unavailable");
        }
        Err(_) => {
            tracing::warn!(group = %handle.name(), %upstream, "upstream did not answer within {UPSTREAM_HEADER_TIMEOUT:?}");
            return error_response(StatusCode::GATEWAY_TIMEOUT, "upstream timeout");
        }
    };
    let switching = resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    strip_hop_by_hop(resp.headers_mut(), switching);

    if switching {
        if let Some(client_upgrade) = client_upgrade {
            let server_upgrade = hyper::upgrade::on(&mut resp);
            let name = handle.name().to_owned();
            tokio::spawn(async move {
                let _guard = guard;
                match tokio::try_join!(client_upgrade, server_upgrade) {
                    Ok((c, s)) => {
                        let mut c = TokioIo::new(c);
                        let mut s = TokioIo::new(s);
                        if let Err(e) = tokio::io::copy_bidirectional(&mut c, &mut s).await {
                            tracing::debug!(group = %name, "upgraded connection ended: {e}");
                        }
                    }
                    Err(e) => tracing::warn!(group = %name, "upgrade failed: {e}"),
                }
            });
            return resp.map(|b| b.boxed());
        }
    }

    resp.map(|body| {
        let guarded = GuardedBody { inner: Box::pin(body.boxed()), _guard: guard };
        BoxBody::new(guarded)
    })
}

/// Drop hop-by-hop headers, keeping `Connection`/`Upgrade` for a protocol switch.
fn strip_hop_by_hop(headers: &mut hyper::HeaderMap, keep_upgrade: bool) {
    let listed: Vec<String> = headers
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|t| t.trim().to_ascii_lowercase()))
        .filter(|t| !t.is_empty())
        .collect();
    for name in listed.iter().map(String::as_str).chain(HOP_BY_HOP.iter().copied()) {
        if keep_upgrade && (name == "connection" || name == "upgrade") {
            continue;
        }
        headers.remove(name);
    }
}

fn error_response(status: StatusCode, text: &str) -> Response<OutBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(format!("{text}\n")))
        .unwrap_or_else(|_| Response::new(full("")))
}

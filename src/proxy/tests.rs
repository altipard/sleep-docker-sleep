use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::config::{GroupSettings, HealthSpec};
use crate::test_support;

fn settings(mode: ProxyMode, upstream: SocketAddr) -> GroupSettings {
    GroupSettings {
        start_grace: Duration::ZERO,
        health: HealthSpec::Grace,
        proxy: Some(ProxySettings {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            upstream: upstream.to_string(),
            mode,
        }),
        ..GroupSettings::default()
    }
}

#[tokio::test]
async fn native_http_proxy_wakes_streams_and_scrubs_hop_headers() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let size = socket.read(&mut request).await.unwrap();
        seen_tx.send(String::from_utf8_lossy(&request[..size]).to_string()).unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
    });

    let group_settings = settings(ProxyMode::Http, upstream_addr);
    let proxy_settings = group_settings.proxy.clone().unwrap();
    let (registry, backend) = test_support::registry("shop", &["app"], false, group_settings);
    let bus = registry.bus().clone();
    let handle = registry.get("shop").unwrap();
    let proxy = ProxyTask::spawn(handle, proxy_settings.clone(), bus).unwrap();
    assert_ne!(proxy.listen().port(), 0);
    assert_eq!(proxy.settings(), proxy_settings);
    let effective_settings = ProxySettings { listen: proxy.listen(), ..proxy_settings.clone() };
    proxy.update(effective_settings.clone());
    assert_eq!(proxy.settings(), effective_settings);

    let mut client = TcpStream::connect(proxy.listen()).await.unwrap();
    client
        .write_all(
            b"GET /hello?q=1 HTTP/1.1\r\nHost: example.test\r\nConnection: x-remove\r\nX-Remove: secret\r\nX-Forwarded-For: 203.0.113.1\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = vec![0; 4096];
    let size = tokio::time::timeout(Duration::from_secs(2), client.read(&mut response)).await.unwrap().unwrap();
    let response = String::from_utf8_lossy(&response[..size]);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("ok"), "{response}");

    let upstream_request = seen_rx.await.unwrap();
    assert!(upstream_request.starts_with("GET /hello?q=1 HTTP/1.1"));
    assert!(upstream_request.contains("host: example.test") || upstream_request.contains("Host: example.test"));
    assert!(!upstream_request.to_ascii_lowercase().contains("x-remove"));
    assert!(upstream_request.contains("203.0.113.1, 127.0.0.1"));
    assert!(upstream_request.to_ascii_lowercase().contains("x-forwarded-proto: http"));
    assert_eq!(backend.start_calls.lock().unwrap().len(), 1);

    proxy.stop();
    registry.shutdown().await;
}

#[tokio::test]
async fn native_tcp_proxy_wakes_and_splices_bytes() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0; 4];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").await.unwrap();
    });

    let group_settings = settings(ProxyMode::Tcp, upstream_addr);
    let proxy_settings = group_settings.proxy.clone().unwrap();
    let (registry, backend) = test_support::registry("database", &["db"], false, group_settings);
    let proxy = ProxyTask::spawn(registry.get("database").unwrap(), proxy_settings, registry.bus().clone()).unwrap();
    let mut client = TcpStream::connect(proxy.listen()).await.unwrap();
    client.write_all(b"ping").await.unwrap();
    let mut response = [0; 4];
    tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut response)).await.unwrap().unwrap();
    assert_eq!(&response, b"pong");
    assert_eq!(backend.start_calls.lock().unwrap().len(), 1);

    proxy.stop();
    registry.shutdown().await;
}

#[test]
fn hop_by_hop_headers_follow_connection_tokens_and_upgrade_rules() {
    use hyper::HeaderMap;

    let mut headers = HeaderMap::new();
    headers.insert("connection", "X-Remove, upgrade".parse().unwrap());
    headers.insert("x-remove", "gone".parse().unwrap());
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("te", "trailers".parse().unwrap());
    headers.insert("x-keep", "yes".parse().unwrap());
    super::http::strip_hop_by_hop(&mut headers, true);
    assert!(!headers.contains_key("x-remove"));
    assert!(!headers.contains_key("te"));
    assert!(headers.contains_key("upgrade"));
    assert!(headers.contains_key("connection"));
    assert_eq!(headers.get("x-keep").unwrap(), "yes");

    super::http::strip_hop_by_hop(&mut headers, false);
    assert!(!headers.contains_key("upgrade"));
    assert!(!headers.contains_key("connection"));
}

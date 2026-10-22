//! Layer-4 proxy: accept, wake the group, connect upstream, splice bytes.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

use crate::config::ProxySettings;
use crate::controller::{EventBus, GroupHandle, WakeOutcome};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn serve(listener: TcpListener, handle: Arc<GroupHandle>, settings: ProxySettings, _bus: Arc<EventBus>) {
    loop {
        let (client, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(group = %handle.name(), "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let handle = Arc::clone(&handle);
        let upstream = settings.upstream.clone();
        tokio::spawn(async move {
            let _inflight = handle.begin_inflight();
            match handle.ensure_ready().await {
                WakeOutcome::Ready => {}
                outcome => {
                    tracing::debug!(group = %handle.name(), %peer, ?outcome, "dropping tcp connection");
                    return;
                }
            }
            let server = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&upstream)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(group = %handle.name(), %upstream, "upstream connect failed: {e}");
                    return;
                }
                Err(_) => {
                    tracing::warn!(group = %handle.name(), %upstream, "upstream connect timed out");
                    return;
                }
            };
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            let (mut c, mut s) = (client, server);
            if let Err(e) = tokio::io::copy_bidirectional(&mut c, &mut s).await {
                tracing::debug!(group = %handle.name(), %peer, "connection ended: {e}");
            }
        });
    }
}

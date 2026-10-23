//! Layer-4 proxy: accept, wake the group, connect upstream, splice bytes.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::config::{ProxySettings, WakeMode};
use crate::controller::{GroupHandle, WakeOutcome};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Accept loop; runs until the listener is dropped by the caller.
pub async fn serve(
    listener: Arc<TcpListener>,
    handle: Arc<GroupHandle>,
    settings: ProxySettings,
    limit: Arc<Semaphore>,
) {
    let upstream: Arc<str> = Arc::from(settings.upstream.as_str());
    loop {
        let Ok(permit) = Arc::clone(&limit).acquire_owned().await else {
            return;
        };
        let (client, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(group = %handle.name(), "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let handle = Arc::clone(&handle);
        let upstream = Arc::clone(&upstream);
        tokio::spawn(async move {
            let _permit = permit;
            let _inflight = handle.begin_inflight();
            // A byte stream has no "please wait" page: always hold, whatever `wake` says.
            match handle.ensure_ready_with(Some(WakeMode::Hold)).await {
                WakeOutcome::Ready => {}
                outcome => {
                    tracing::debug!(group = %handle.name(), %peer, ?outcome, "dropping tcp connection");
                    return;
                }
            }
            let server = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&*upstream)).await {
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

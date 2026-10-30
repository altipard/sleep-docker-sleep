//! Native TCP/HTTP proxy for groups without a reverse proxy in front.
//!
//! One [`ProxyTask`] owns one listening socket. Settings changes that keep the `listen`
//! address (new upstream, other mode) are pushed through a `watch` channel and restart only
//! the accept loop; the socket is never rebound, so there is no window in which the port
//! is still held by the old task. Established connections finish with the settings they
//! started with, like a reverse proxy reload.

mod http;
mod tcp;

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use futures_util::FutureExt;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{ProxyMode, ProxySettings};
use crate::controller::{EventBus, GroupHandle};

/// Upper bound of simultaneously open client connections per proxy listener. Beyond it,
/// `accept` waits instead of spawning without limit.
const MAX_CONNECTIONS: usize = 1024;

/// A running listener for one group.
pub struct ProxyTask {
    listen: SocketAddr,
    settings: watch::Sender<ProxySettings>,
    cancel: CancellationToken,
    _task: JoinHandle<()>,
}

impl ProxyTask {
    /// Bind `settings.listen` synchronously (so a port clash is reported right away) and
    /// serve in the background.
    pub fn spawn(handle: Arc<GroupHandle>, settings: ProxySettings, bus: Arc<EventBus>) -> Result<Self> {
        let std_listener = std::net::TcpListener::bind(settings.listen)
            .map_err(|e| anyhow::anyhow!("bind {}: {e}", settings.listen))?;
        std_listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(std_listener)?;
        // Report the effective address, including the OS-assigned port for `listen = 0`.
        let listen = listener.local_addr()?;
        let (tx, rx) = watch::channel(settings);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(listener, handle, rx, cancel.clone(), bus));
        Ok(Self { listen, settings: tx, cancel, _task: task })
    }

    pub fn settings(&self) -> ProxySettings {
        self.settings.borrow().clone()
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    /// Apply new settings for the same `listen` address without rebinding.
    pub fn update(&self, settings: ProxySettings) {
        debug_assert_eq!(settings.listen, self.listen, "update() must keep the listen address");
        self.settings.send_replace(settings);
    }

    /// Stop accepting. The socket closes as soon as the task observes the cancellation;
    /// established connections are left to finish.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

async fn run(
    listener: TcpListener,
    handle: Arc<GroupHandle>,
    mut settings: watch::Receiver<ProxySettings>,
    cancel: CancellationToken,
    _bus: Arc<EventBus>,
) {
    let listener = Arc::new(listener);
    let limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let current = settings.borrow_and_update().clone();
        let accept_loop = match current.mode {
            ProxyMode::Tcp => {
                tcp::serve(Arc::clone(&listener), Arc::clone(&handle), current, Arc::clone(&limit)).boxed()
            }
            ProxyMode::Http => {
                http::serve(Arc::clone(&listener), Arc::clone(&handle), current, Arc::clone(&limit)).boxed()
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => return,
            changed = settings.changed() => {
                if changed.is_err() {
                    return;
                }
                tracing::info!(group = %handle.name(), "native proxy settings changed; restarting accept loop");
            }
            () = accept_loop => return,
        }
    }
}

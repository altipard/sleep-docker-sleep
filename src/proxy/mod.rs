//! Native TCP/HTTP proxy for groups without a reverse proxy in front.

mod http;
mod tcp;

use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinHandle;

use crate::config::{ProxyMode, ProxySettings};
use crate::controller::{EventBus, GroupHandle};

/// A running listener for one group.
pub struct ProxyTask {
    settings: ProxySettings,
    task: JoinHandle<()>,
}

impl ProxyTask {
    /// Bind `settings.listen` synchronously (so a port clash is reported right away) and
    /// serve in the background.
    pub fn spawn(handle: Arc<GroupHandle>, settings: ProxySettings, bus: Arc<EventBus>) -> Result<Self> {
        let std_listener = std::net::TcpListener::bind(settings.listen)
            .map_err(|e| anyhow::anyhow!("bind {}: {e}", settings.listen))?;
        std_listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let task = match settings.mode {
            ProxyMode::Tcp => tokio::spawn(tcp::serve(listener, handle, settings.clone(), bus)),
            ProxyMode::Http => tokio::spawn(http::serve(listener, handle, settings.clone(), bus)),
        };
        Ok(Self { settings, task })
    }

    pub fn settings(&self) -> &ProxySettings {
        &self.settings
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

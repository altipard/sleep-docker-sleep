//! The control plane: one actor per group that decides when to start and stop containers,
//! a registry of those actors, and the supervisor that keeps the registry in sync with
//! Docker.
//!
//! Hot path guarantee: request handlers only touch atomics on [`Activity`] and read a
//! `watch` channel. Docker is never called from a request.

mod activity;
mod actor;
mod events;
mod handle;
mod health;
mod registry;
mod signals;
mod snapshot;
mod supervisor;
mod time;

#[cfg(test)]
mod registry_tests;

pub use activity::{Activity, InflightGuard};
pub use events::{EventBus, EventKind, SdsEvent};
pub use handle::{GroupHandle, WakeOutcome};
pub use registry::Registry;
pub use snapshot::{GroupSnapshot, MemberSnapshot, StateKind};
pub use supervisor::Supervisor;
pub use time::{epoch_ms, mono_ms};

/// Minimum pause between two start attempts after a failed wake-up. Traffic-driven wakes
/// inside this window are ignored by the actor; operator wakes are not.
pub const FAILED_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

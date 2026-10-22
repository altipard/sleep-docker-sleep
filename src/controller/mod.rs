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
mod snapshot;
mod supervisor;
mod time;

pub use activity::{Activity, InflightGuard};
pub use actor::Command;
pub use events::{EventBus, EventKind, SdsEvent};
pub use handle::{GroupHandle, WakeOutcome};
pub use registry::Registry;
pub use snapshot::{GroupSnapshot, MemberSnapshot, StateKind};
pub use supervisor::Supervisor;
pub use time::{epoch_ms, mono_ms};

/// Minimum pause between two start attempts after a failed wake-up.
pub const FAILED_RETRY_INTERVAL_MS: u64 = 10_000;

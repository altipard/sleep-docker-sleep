//! Container backend: a small trait over the Docker Engine API so the controller can be
//! tested without Docker, plus group discovery from labels and configuration.

mod backend;
mod discovery;
mod engine;

pub use backend::{Backend, BackendError, ContainerEvent, ContainerInfo, EventAction, HealthState};
pub use discovery::{Discovery, GroupSource, GroupSpec, MemberSpec, discover};
pub use engine::DockerEngine;

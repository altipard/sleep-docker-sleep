//! Configuration: the `sds.toml` file and the `sds.*` container labels.
//!
//! Precedence for a group's settings: container labels > static group entry in
//! `sds.toml` > `[defaults]` in `sds.toml` > built-in defaults.

mod file;
mod labels;
mod settings;

pub use file::{Config, ConfigSource, UnknownGroupPolicy, load, locate, validate_group_name};
pub use labels::{COMPOSE_PROJECT, COMPOSE_SERVICE, LabelError, ParsedLabels, parse_labels};
pub use settings::{GroupSettings, HealthSpec, ProxyMode, ProxySettings, SettingsOverlay, WakeMode};

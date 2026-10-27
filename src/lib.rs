#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! sleep-docker-sleep (`sds`): scale Docker containers to zero while idle and
//! wake them on the first incoming request.
//!
//! The crate is organised by domain:
//! - [`config`]: file configuration and container label parsing
//! - [`docker`]: the container backend (Docker Engine API) behind a trait
//! - [`controller`]: per-group state machine (sleep / wake) and supervisor
//! - [`admin`]: HTTP admin API, forward-auth endpoint, web overview
//! - [`proxy`]: optional native TCP/HTTP proxy for setups without a reverse proxy
//! - [`integrate`]: snippet generators for nginx, Caddy and Traefik
//! - [`service`]: launchd / systemd unit generation
//! - [`tui`]: terminal dashboard for `sds run`

pub mod admin;
pub mod cli;
pub mod config;
pub mod controller;
pub mod docker;
pub mod integrate;
pub mod proxy;
pub mod service;
pub mod tui;

pub const LABEL_PREFIX: &str = "sds.";

/// Version of this build: the release tag, or `git describe` for a local build (see
/// `build.rs`).
pub const VERSION: &str = env!("SDS_VERSION");

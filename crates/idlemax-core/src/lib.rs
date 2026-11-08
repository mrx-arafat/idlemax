//! Domain crate: contract types, config, paths, guard, quota, scheduling and advice rules.
//! Pure: no I/O, no clock. Time is always a `now_ms: UnixMs` parameter.
#![forbid(unsafe_code)]

pub mod advice;
pub mod config;
pub mod control;
pub mod doctor;
pub mod explain;
pub mod guard;
pub mod install;
pub mod job_draft;
pub mod job_run;
pub mod notify;
pub mod paths;
pub mod protocol;
pub mod quota;
pub mod reconcile;
pub mod schedule;
pub mod scheduler;
pub mod types;

pub use config::{Config, ConfigError, GuardPolicy};
pub use paths::{HomeError, Paths};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_COORD_LISTEN: &str = "127.0.0.1:7878";
pub const DEFAULT_COORD_URL: &str = "http://127.0.0.1:7878";
pub const DEFAULT_AGENT_LISTEN: &str = "127.0.0.1:7879";
pub const TOKEN_ENV: &str = "IDLEMAX_TOKEN";

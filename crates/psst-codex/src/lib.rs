//! Local, bounded Codex App Server activation boundary.

#![forbid(unsafe_code)]

mod app_server;
mod bootstrap;

pub use app_server::{AppServerConfig, AppServerError, CodexAppServerHost, ThreadPolicy};
pub use bootstrap::{CodexActivation, start_from_environment};

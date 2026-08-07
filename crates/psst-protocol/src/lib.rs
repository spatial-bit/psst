//! Stable, versioned HTTP contract shared by the Psst relay and client.

#![forbid(unsafe_code)]

mod auth;
mod dto;
mod error;
mod openapi;
mod timestamp;
mod validation;

pub use auth::*;
pub use dto::*;
pub use error::*;
pub use openapi::*;
pub use timestamp::*;
pub use validation::*;

/// Versioned JSON media type accepted and emitted by the relay.
pub const JSON_CONTENT_TYPE: &str = "application/json";

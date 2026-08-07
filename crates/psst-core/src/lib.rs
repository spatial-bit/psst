//! Protocol types and state rules for Psst.
//!
//! This crate intentionally performs no network, database, filesystem, MCP,
//! Claude, or Codex I/O.

#![forbid(unsafe_code)]

/// The protocol version implemented by this workspace.
pub const PROTOCOL_VERSION: &str = "v1";

#[cfg(test)]
mod tests {
    use super::PROTOCOL_VERSION;

    #[test]
    fn protocol_version_is_explicit() {
        assert_eq!(PROTOCOL_VERSION, "v1");
    }
}

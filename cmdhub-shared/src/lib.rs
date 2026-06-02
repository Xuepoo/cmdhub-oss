//! CmdHub Shared Types
//!
//! Common types, ACI (Agent-Computer Interface) schema definitions,
//! and error types used across cmdhub-cli and cmdhub-mcp.

pub mod aci;
pub mod error;

pub use aci::*;
pub use error::*;

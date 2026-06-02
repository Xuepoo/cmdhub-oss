//! CmdHub Skills Plugin System
//!
//! Extensible skill/plugin system for CmdHub.
//! Skills can provide custom command discovery strategies,
//! execution wrappers, and intent resolution logic.

pub mod skill;

pub use skill::*;

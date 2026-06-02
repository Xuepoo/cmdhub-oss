//! Error types for CmdHub.

use thiserror::Error;

/// Top-level CmdHub error type.
#[derive(Debug, Error)]
pub enum CmdHubError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Signature verification failed: {0}")]
    SignatureVerification(String),

    #[error("Database update failed: {0}")]
    UpdateFailed(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Command not found: {query}")]
    NotFound { query: String },

    #[error("Execution blocked by safety guardrail: risk_level={risk_level}, command={command}")]
    ExecutionBlocked {
        risk_level: String,
        command: String,
    },
}

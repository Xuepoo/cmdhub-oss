//! ACI (Agent-Computer Interface) schema types.
//!
//! These types define the machine-readable contract that CmdHub returns
//! to AI Agents for CLI command discovery and execution.

use serde::{Deserialize, Serialize};

/// The hierarchical level of a command node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    /// Root command (e.g., `tar`, `git`)
    Root,
    /// Sub-command (e.g., `git commit`, `tar create`)
    Sub,
    /// Argument/flag (e.g., `--verbose`, `-f`)
    Arg,
}

/// Security risk level for execution gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Read-only operations, no side effects.
    Safe,
    /// Local file modifications, network requests.
    Medium,
    /// Destructive deletions, privilege escalations.
    Dangerous,
}

/// Cross-platform installation instructions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallInstructions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pacman: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<String>,
}

/// The core ACI command contract returned by CmdHub search.
///
/// This is the primary data structure that AI Agents consume.
/// It provides everything needed to discover, understand, and execute a CLI command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AciCommandContract {
    /// Unique identifier (e.g., "org.github.mtoyoda.sl")
    pub app_id: String,
    /// Base command name (e.g., "sl")
    pub name: String,
    /// Materialized path (e.g., "sl.-l", "gh.pr.create")
    pub cmd_path: String,
    /// Hierarchical level
    pub node_type: NodeType,
    /// Agent-friendly description
    pub description: String,
    /// Security risk rating
    pub risk_level: RiskLevel,
    /// Ready-to-execute template (e.g., "sl -l")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_template: Option<String>,
    /// Cross-platform install commands
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_instructions: Option<InstallInstructions>,
}

/// Metadata about the local offline database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbMetadata {
    /// ETag for cache validation
    pub etag: String,
    /// Database version string
    pub version: String,
    /// Last update timestamp (Unix seconds)
    pub updated_at: i64,
    /// Total number of indexed apps
    pub app_count: u64,
    /// Total number of indexed commands
    pub command_count: u64,
}

/// Update check response from the cloud sync endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// Current latest version
    pub version: String,
    /// ETag for cache validation
    pub etag: String,
    /// CDN download URL for the .zst compressed database
    pub db_url: String,
    /// CDN download URL for the Ed25519 signature file
    pub sig_url: String,
    /// SHA-256 checksum of the .zst file
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aci_serialization_roundtrip() {
        let contract = AciCommandContract {
            app_id: "org.github.mtoyoda.sl".to_string(),
            name: "sl".to_string(),
            cmd_path: "sl.-l".to_string(),
            node_type: NodeType::Arg,
            description: "Display a train moving from left to right".to_string(),
            risk_level: RiskLevel::Safe,
            example_template: Some("sl -l".to_string()),
            install_instructions: None,
        };

        let json = serde_json::to_string(&contract).unwrap();
        let deserialized: AciCommandContract = serde_json::from_str(&json).unwrap();
        assert_eq!(contract.app_id, deserialized.app_id);
        assert_eq!(contract.cmd_path, deserialized.cmd_path);
        assert_eq!(contract.risk_level, deserialized.risk_level);
    }

    #[test]
    fn test_risk_level_json_values() {
        assert_eq!(serde_json::to_string(&RiskLevel::Safe).unwrap(), "\"safe\"");
        assert_eq!(
            serde_json::to_string(&RiskLevel::Dangerous).unwrap(),
            "\"dangerous\""
        );
    }
}

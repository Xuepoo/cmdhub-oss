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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoop: Option<String>,

    #[serde(flatten)]
    #[serde(default)]
    pub others: std::collections::HashMap<String, String>,
}

impl InstallInstructions {
    pub fn get_command(&self, key: &str) -> Option<&String> {
        match key {
            "brew" => self.brew.as_ref(),
            "apt" => self.apt.as_ref(),
            "pacman" => self.pacman.as_ref(),
            "cargo" => self.cargo.as_ref(),
            "scoop" => self.scoop.as_ref(),
            _ => self.others.get(key),
        }
    }
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
    /// Docker container image for isolated execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,
    /// Direct URL to official install shell scripts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_url: Option<String>,
    /// URL of the open-source code repository
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
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

/// Database record representing the `apps` table row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbApp {
    pub app_id: String,
    pub name: String,
    pub install_instructions: Option<String>,
}

/// Database record representing the `arguments` table row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbArgument {
    pub cmd_path: String,
    pub app_id: String,
    pub node_name: String,
    pub node_type: String,
    pub description: String,
    pub risk_level: String,
    pub example_template: Option<String>,
    pub docker_image: Option<String>,
    pub script_url: Option<String>,
    pub source_url: Option<String>,
}

/// Flattened database record representing the JOIN of `arguments` and `apps`.
///
/// This provides the exact structure returned by combining a specific
/// CLI command/argument with its parent app metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbAciRecord {
    pub app_id: String,
    pub name: String,
    pub cmd_path: String,
    pub node_type: String,
    pub description: String,
    pub risk_level: String,
    pub example_template: Option<String>,
    pub install_instructions: Option<String>,
    pub docker_image: Option<String>,
    pub script_url: Option<String>,
    pub source_url: Option<String>,
}

impl AciCommandContract {
    /// Extracts the node name from the cmd_path (the last component after '.')
    pub fn node_name(&self) -> &str {
        self.cmd_path.split('.').next_back().unwrap_or(&self.name)
    }

    /// Converts this contract into offline SQLite database records.
    pub fn to_db_records(&self) -> Result<(DbApp, DbArgument), crate::error::CmdHubError> {
        let install_instructions = if let Some(ref inst) = self.install_instructions {
            Some(serde_json::to_string(inst)?)
        } else {
            None
        };

        let app = DbApp {
            app_id: self.app_id.clone(),
            name: self.name.clone(),
            install_instructions,
        };

        let node_type_str = match self.node_type {
            NodeType::Root => "root",
            NodeType::Sub => "sub",
            NodeType::Arg => "arg",
        };

        let risk_level_str = match self.risk_level {
            RiskLevel::Safe => "safe",
            RiskLevel::Medium => "medium",
            RiskLevel::Dangerous => "dangerous",
        };

        let argument = DbArgument {
            cmd_path: self.cmd_path.clone(),
            app_id: self.app_id.clone(),
            node_name: self.node_name().to_string(),
            node_type: node_type_str.to_string(),
            description: self.description.clone(),
            risk_level: risk_level_str.to_string(),
            example_template: self.example_template.clone(),
            docker_image: self.docker_image.clone(),
            script_url: self.script_url.clone(),
            source_url: self.source_url.clone(),
        };

        Ok((app, argument))
    }
}

impl TryFrom<DbAciRecord> for AciCommandContract {
    type Error = crate::error::CmdHubError;

    fn try_from(record: DbAciRecord) -> Result<Self, Self::Error> {
        let node_type = match record.node_type.as_str() {
            "root" => NodeType::Root,
            "sub" => NodeType::Sub,
            "arg" => NodeType::Arg,
            other => {
                return Err(crate::error::CmdHubError::Validation(format!(
                    "Invalid node_type in database: '{}'",
                    other
                )))
            }
        };

        let risk_level = match record.risk_level.as_str() {
            "safe" => RiskLevel::Safe,
            "medium" => RiskLevel::Medium,
            "dangerous" => RiskLevel::Dangerous,
            other => {
                return Err(crate::error::CmdHubError::Validation(format!(
                    "Invalid risk_level in database: '{}'",
                    other
                )))
            }
        };

        let install_instructions = if let Some(ref inst_str) = record.install_instructions {
            if inst_str.trim().is_empty() {
                None
            } else {
                Some(serde_json::from_str(inst_str).map_err(|e| {
                    crate::error::CmdHubError::Validation(format!(
                        "Failed to parse install_instructions JSON: {}",
                        e
                    ))
                })?)
            }
        } else {
            None
        };

        Ok(AciCommandContract {
            app_id: record.app_id,
            name: record.name,
            cmd_path: record.cmd_path,
            node_type,
            description: record.description,
            risk_level,
            example_template: record.example_template,
            install_instructions,
            docker_image: record.docker_image,
            script_url: record.script_url,
            source_url: record.source_url,
        })
    }
}

/// SQL statement to create the physical table `apps`.
pub const CREATE_APPS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS apps (
    app_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    install_instructions TEXT
);
"#;

/// SQL statement to create the physical table `arguments`.
pub const CREATE_ARGUMENTS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS arguments (
    cmd_path TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    node_type TEXT NOT NULL,
    description TEXT NOT NULL,
    risk_level TEXT NOT NULL,
    example_template TEXT,
    docker_image TEXT,
    script_url TEXT,
    source_url TEXT,
    FOREIGN KEY(app_id) REFERENCES apps(app_id) ON DELETE CASCADE
);
"#;

/// SQL statement to create the FTS5 virtual table `apps_fts`.
pub const CREATE_APPS_FTS_TABLE: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS apps_fts USING fts5(
    cmd_path UNINDEXED,
    name,
    capabilities
);
"#;

/// SQL statement to create the sqlite-vec virtual table `commands_vec`.
pub const CREATE_COMMANDS_VEC_TABLE: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS commands_vec USING vec0(
    cmd_path TEXT PRIMARY KEY,
    embedding float[512]
);
"#;

/// The Reciprocal Rank Fusion (RRF) hybrid search query combining FTS5 and sqlite-vec.
pub const RRF_QUERY: &str = r#"
WITH fts_rank AS (
    SELECT cmd_path, row_number() OVER (ORDER BY bm25(apps_fts) ASC) as fts_pos
    FROM apps_fts WHERE apps_fts MATCH :query
),
vec_rank AS (
    SELECT cmd_path, row_number() OVER (ORDER BY distance ASC) as vec_pos
    FROM commands_vec
    WHERE embedding MATCH :query_vector AND k = 100
)
SELECT
    arg.cmd_path, arg.node_name, arg.description, arg.risk_level, arg.example_template,
    COALESCE(1.0 / (60.0 + fts.fts_pos), 0.0) + COALESCE(1.0 / (60.0 + vec.vec_pos), 0.0) as rrf_score
FROM arguments arg
LEFT JOIN fts_rank fts ON arg.cmd_path = fts.cmd_path
LEFT JOIN vec_rank vec ON arg.cmd_path = vec.cmd_path
WHERE fts.cmd_path IS NOT NULL OR vec.cmd_path IS NOT NULL
ORDER BY rrf_score DESC
LIMIT :limit_num;
"#;

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
            docker_image: None,
            script_url: None,
            source_url: None,
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

    #[test]
    fn test_db_conversions() {
        let contract = AciCommandContract {
            app_id: "org.github.mtoyoda.sl".to_string(),
            name: "sl".to_string(),
            cmd_path: "sl.-l".to_string(),
            node_type: NodeType::Arg,
            description: "Display a train moving from left to right".to_string(),
            risk_level: RiskLevel::Safe,
            example_template: Some("sl -l".to_string()),
            install_instructions: Some(InstallInstructions {
                brew: Some("brew install sl".to_string()),
                apt: Some("sudo apt install sl".to_string()),
                pacman: None,
                cargo: None,
                scoop: Some("scoop install sl".to_string()),
                ..Default::default()
            }),
            docker_image: Some("docker.io/library/sl:latest".to_string()),
            script_url: Some(
                "https://raw.githubusercontent.com/mtoyoda/sl/master/install.sh".to_string(),
            ),
            source_url: Some("https://github.com/mtoyoda/sl".to_string()),
        };

        // Test node_name extraction
        assert_eq!(contract.node_name(), "-l");

        // Test converting to DB records
        let (db_app, db_arg) = contract.to_db_records().unwrap();
        assert_eq!(db_app.app_id, "org.github.mtoyoda.sl");
        assert_eq!(db_app.name, "sl");
        assert!(db_app
            .install_instructions
            .as_ref()
            .unwrap()
            .contains("brew install sl"));

        assert_eq!(db_arg.cmd_path, "sl.-l");
        assert_eq!(db_arg.app_id, "org.github.mtoyoda.sl");
        assert_eq!(db_arg.node_name, "-l");
        assert_eq!(db_arg.node_type, "arg");
        assert_eq!(db_arg.risk_level, "safe");
        assert_eq!(db_arg.example_template, Some("sl -l".to_string()));
        assert_eq!(
            db_arg.docker_image,
            Some("docker.io/library/sl:latest".to_string())
        );
        assert_eq!(
            db_arg.script_url,
            Some("https://raw.githubusercontent.com/mtoyoda/sl/master/install.sh".to_string())
        );
        assert_eq!(
            db_arg.source_url,
            Some("https://github.com/mtoyoda/sl".to_string())
        );

        // Test reconstruction from DbAciRecord
        let db_record = DbAciRecord {
            app_id: db_app.app_id,
            name: db_app.name,
            cmd_path: db_arg.cmd_path,
            node_type: db_arg.node_type,
            description: db_arg.description,
            risk_level: db_arg.risk_level,
            example_template: db_arg.example_template,
            install_instructions: db_app.install_instructions,
            docker_image: db_arg.docker_image,
            script_url: db_arg.script_url,
            source_url: db_arg.source_url,
        };

        let reconstructed = AciCommandContract::try_from(db_record).unwrap();
        assert_eq!(reconstructed.app_id, contract.app_id);
        assert_eq!(reconstructed.cmd_path, contract.cmd_path);
        assert_eq!(reconstructed.node_type, contract.node_type);
        assert_eq!(reconstructed.risk_level, contract.risk_level);
        assert_eq!(
            reconstructed.install_instructions.as_ref().unwrap().brew,
            Some("brew install sl".to_string())
        );
        assert_eq!(
            reconstructed.install_instructions.as_ref().unwrap().scoop,
            Some("scoop install sl".to_string())
        );
        assert_eq!(
            reconstructed.docker_image,
            Some("docker.io/library/sl:latest".to_string())
        );
        assert_eq!(
            reconstructed.script_url,
            Some("https://raw.githubusercontent.com/mtoyoda/sl/master/install.sh".to_string())
        );
        assert_eq!(
            reconstructed.source_url,
            Some("https://github.com/mtoyoda/sl".to_string())
        );
    }

    #[test]
    fn test_install_instructions_flattened_others() {
        let json_data = r#"{
            "brew": "brew install git",
            "dnf": "dnf install -y git",
            "apk": "apk add git"
        }"#;
        let inst: InstallInstructions = serde_json::from_str(json_data).unwrap();
        assert_eq!(inst.brew.as_deref(), Some("brew install git"));
        assert_eq!(
            inst.get_command("brew").map(|s| s.as_str()),
            Some("brew install git")
        );
        assert_eq!(
            inst.get_command("dnf").map(|s| s.as_str()),
            Some("dnf install -y git")
        );
        assert_eq!(
            inst.get_command("apk").map(|s| s.as_str()),
            Some("apk add git")
        );
        assert_eq!(inst.get_command("pacman"), None);
    }
}

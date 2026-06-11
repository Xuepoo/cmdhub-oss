//! Skill trait and registry.

use cmdhub_shared::AciCommandContract;

/// A skill provides a way to discover and resolve CLI commands.
pub trait Skill: Send + Sync {
    /// Unique identifier for this skill.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Resolve a natural language query to ACI contracts.
    fn resolve(&self, query: &str) -> anyhow::Result<Vec<AciCommandContract>>;
}

/// A registry that holds dynamic `Skill` trait objects and aggregates their resolution results.
pub struct SkillRegistry {
    skills: Vec<Box<dyn Skill>>,
}

impl SkillRegistry {
    /// Creates a new, empty registry.
    pub fn new() -> Self {
        Self { skills: Vec::new() }
    }

    /// Registers a dynamic skill trait object.
    pub fn register(&mut self, skill: Box<dyn Skill>) {
        self.skills.push(skill);
    }

    /// Aggregates results across all registered skills.
    pub fn resolve(&self, query: &str) -> anyhow::Result<Vec<AciCommandContract>> {
        let mut results = Vec::new();
        for skill in &self.skills {
            match skill.resolve(query) {
                Ok(mut skill_results) => {
                    results.append(&mut skill_results);
                }
                Err(err) => {
                    tracing::warn!("Skill {} failed to resolve query: {:?}", skill.id(), err);
                }
            }
        }
        Ok(results)
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in skill that loads command contract records from a local folder of JSON files.
pub struct LocalFileSkill {
    config_dir: std::path::PathBuf,
}

impl LocalFileSkill {
    /// Creates a new `LocalFileSkill` with a target configuration directory.
    pub fn new(config_dir: std::path::PathBuf) -> Self {
        Self { config_dir }
    }

    /// Recursively scans a directory for JSON files.
    fn scan_dir(
        &self,
        dir: &std::path::Path,
        contracts: &mut Vec<AciCommandContract>,
    ) -> anyhow::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!("Failed to read directory entry: {:?}", err);
                    continue;
                }
            };

            let path = entry.path();
            if path.is_dir() {
                if let Err(err) = self.scan_dir(&path, contracts) {
                    tracing::warn!("Failed to scan subdirectory {:?}: {:?}", path, err);
                }
            } else if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                match self.load_json_file(&path) {
                    Ok(mut loaded_contracts) => {
                        contracts.append(&mut loaded_contracts);
                    }
                    Err(err) => {
                        tracing::warn!("Failed to parse JSON file {:?}: {:?}", path, err);
                    }
                }
            }
        }

        Ok(())
    }

    /// Loads and parses an AciCommandContract (single object or list) from a JSON file.
    fn load_json_file(&self, path: &std::path::Path) -> anyhow::Result<Vec<AciCommandContract>> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let value: serde_json::Value = serde_json::from_reader(reader)?;

        if value.is_array() {
            let parsed: Vec<AciCommandContract> = serde_json::from_value(value)?;
            Ok(parsed)
        } else {
            let parsed: AciCommandContract = serde_json::from_value(value)?;
            Ok(vec![parsed])
        }
    }
}

impl Skill for LocalFileSkill {
    fn id(&self) -> &str {
        "local_file"
    }

    fn name(&self) -> &str {
        "Local File Shortcut Skill"
    }

    fn resolve(&self, query: &str) -> anyhow::Result<Vec<AciCommandContract>> {
        let mut contracts = Vec::new();
        if !self.config_dir.exists() {
            return Ok(contracts);
        }

        self.scan_dir(&self.config_dir, &mut contracts)?;

        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        if query_words.is_empty() {
            return Ok(contracts);
        }

        let filtered = contracts
            .into_iter()
            .filter(|contract| {
                let name_lower = contract.name.to_lowercase();
                let cmd_path_lower = contract.cmd_path.to_lowercase();
                let desc_lower = contract.description.to_lowercase();

                let combined = format!("{} {} {}", name_lower, cmd_path_lower, desc_lower);
                query_words.iter().all(|&word| combined.contains(word))
            })
            .collect();

        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmdhub_shared::{NodeType, RiskLevel};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn get_temp_test_dir() -> PathBuf {
        let counter = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cmdhub_skills_test_{}_{}",
            std::process::id(),
            counter
        ));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    struct DummySkill {
        id: String,
        name: String,
        contracts: Vec<AciCommandContract>,
    }

    impl Skill for DummySkill {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn resolve(&self, query: &str) -> anyhow::Result<Vec<AciCommandContract>> {
            let query_lower = query.to_lowercase();
            let matches = self
                .contracts
                .iter()
                .filter(|c| c.name.to_lowercase().contains(&query_lower))
                .cloned()
                .collect();
            Ok(matches)
        }
    }

    #[test]
    fn test_dummy_skill_registration_and_resolution() {
        let mut registry = SkillRegistry::new();

        let contract1 = AciCommandContract {
            app_id: "org.test.sl".to_string(),
            name: "sl".to_string(),
            cmd_path: "sl.-l".to_string(),
            node_type: NodeType::Arg,
            description: "Display a train".to_string(),
            risk_level: RiskLevel::Safe,
            example_template: Some("sl -l".to_string()),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
        };

        let contract2 = AciCommandContract {
            app_id: "org.test.git".to_string(),
            name: "git".to_string(),
            cmd_path: "git.commit".to_string(),
            node_type: NodeType::Sub,
            description: "Record changes to the repository".to_string(),
            risk_level: RiskLevel::Medium,
            example_template: Some("git commit -m \"message\"".to_string()),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
        };

        let dummy_skill = DummySkill {
            id: "dummy".to_string(),
            name: "Dummy Skill".to_string(),
            contracts: vec![contract1, contract2],
        };

        registry.register(Box::new(dummy_skill));

        let results = registry.resolve("sl").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "sl");

        let results = registry.resolve("git").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "git");

        let results = registry.resolve("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_local_file_skill_resolution() {
        let temp_dir = get_temp_test_dir();

        let contract_sl = AciCommandContract {
            app_id: "org.test.sl".to_string(),
            name: "sl".to_string(),
            cmd_path: "sl.-l".to_string(),
            node_type: NodeType::Arg,
            description: "Display a train moving from left to right".to_string(),
            risk_level: RiskLevel::Safe,
            example_template: Some("sl -l".to_string()),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
        };

        let sl_json_path = temp_dir.join("sl.json");
        let sl_json_content = serde_json::to_string(&contract_sl).unwrap();
        fs::write(&sl_json_path, sl_json_content).unwrap();

        let contract_git = AciCommandContract {
            app_id: "org.test.git".to_string(),
            name: "git".to_string(),
            cmd_path: "git.commit".to_string(),
            node_type: NodeType::Sub,
            description: "Record changes to the repository".to_string(),
            risk_level: RiskLevel::Medium,
            example_template: Some("git commit -m \"msg\"".to_string()),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
        };
        let git_json_path = temp_dir.join("git.json");
        let git_json_content = serde_json::to_string(&vec![contract_git]).unwrap();
        fs::write(&git_json_path, git_json_content).unwrap();

        let sub_dir = temp_dir.join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        let contract_tar = AciCommandContract {
            app_id: "org.test.tar".to_string(),
            name: "tar".to_string(),
            cmd_path: "tar.create".to_string(),
            node_type: NodeType::Sub,
            description: "Create a tar archive".to_string(),
            risk_level: RiskLevel::Medium,
            example_template: Some("tar -cvf archive.tar files".to_string()),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
        };
        let tar_json_path = sub_dir.join("tar.json");
        let tar_json_content = serde_json::to_string(&contract_tar).unwrap();
        fs::write(&tar_json_path, tar_json_content).unwrap();

        let skill = LocalFileSkill::new(temp_dir.clone());

        let results = skill.resolve("train").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "sl");

        let results = skill.resolve("git commit").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "git");

        let results = skill.resolve("archive").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "tar");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}

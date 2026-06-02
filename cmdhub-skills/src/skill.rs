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

//! Skill data models.

use serde::{Deserialize, Serialize};

/// A skill entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub conditions: Option<String>,
    pub version: i32,
    /// Who created the skill: "agent" (saved by the agent from experience)
    /// or "thirdparty" (imported from an external source such as GitHub).
    pub origin: String,
    /// Workspace-relative path of the skill's markdown file, when the full
    /// instructions live on disk (third-party skills). Large skills are not
    /// injected into the prompt; the agent reads this file on demand.
    pub source_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Skill {
    pub const ORIGIN_AGENT: &'static str = "agent";
    pub const ORIGIN_THIRDPARTY: &'static str = "thirdparty";
}

/// Skill condition for auto-loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCondition {
    /// Platform condition (e.g., "macos", "linux").
    pub platform: Option<String>,
    /// Tool availability condition.
    pub tool_available: Option<String>,
}

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
    pub created_at: String,
    pub updated_at: String,
}

/// Skill condition for auto-loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCondition {
    /// Platform condition (e.g., "macos", "linux").
    pub platform: Option<String>,
    /// Tool availability condition.
    pub tool_available: Option<String>,
}

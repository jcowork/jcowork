//! Skill Loader - discover skills from user workspace.

use anyhow::Result;
use std::path::Path;

use crate::models::Skill;

/// Discovers skill files from the user's workspace directory.
///
/// Skill files are markdown files stored under `<workspace>/.jcowork/skills/`.
/// Each file should have YAML frontmatter with name, description, and conditions.
pub struct SkillLoader;

impl SkillLoader {
    /// Discover all skill files in a user's skills directory.
    pub async fn discover(skills_dir: &str) -> Result<Vec<Skill>> {
        let path = Path::new(skills_dir);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let file_path = entry.path();
            if file_path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
                    if let Some(skill) = parse_skill_file(&content, &file_path) {
                        skills.push(skill);
                    }
                }
            }
        }

        Ok(skills)
    }
}

/// Parse a skill markdown file with optional YAML frontmatter.
fn parse_skill_file(content: &str, path: &Path) -> Option<Skill> {
    let name = path.file_stem()?.to_str()?.to_string();
    let now = chrono::Utc::now().naive_utc().to_string();

    // Parse frontmatter
    let (description, conditions, body) = if content.starts_with("---") {
        let end = content.find("\n---")?;
        let frontmatter = &content[3..end];
        let body = content[end + 4..].trim();

        let mut description = None;
        let mut conditions = None;

        for line in frontmatter.lines() {
            if let Some(val) = line.strip_prefix("description:") {
                description = Some(val.trim().trim_matches('"').to_string());
            } else if let Some(val) = line.strip_prefix("conditions:") {
                conditions = Some(val.trim().trim_matches('"').to_string());
            }
        }

        (description, conditions, body.to_string())
    } else {
        (None, None, content.to_string())
    };

    Some(Skill {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: String::new(), // filled by caller
        name,
        description,
        content: body,
        conditions,
        version: 1,
        created_at: now.clone(),
        updated_at: now,
    })
}

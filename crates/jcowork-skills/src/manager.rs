//! Skill Manager - CRUD, patch, search operations.

use anyhow::Result;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::Skill;

/// Manages skills for a user.
///
/// Skills are procedural memory:
/// reusable workflows that the agent creates from experience and can
/// patch during use when they become outdated.
pub struct SkillManager {
    pool: SqlitePool,
}

impl SkillManager {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a new skill.
    pub async fn create(
        &self,
        user_id: &str,
        name: &str,
        description: Option<&str>,
        content: &str,
        conditions: Option<&str>,
    ) -> Result<Skill> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().naive_utc().to_string();

        sqlx::query(
            r#"INSERT INTO skills (id, user_id, name, description, content, conditions, version, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)"#,
        )
        .bind(&id)
        .bind(user_id)
        .bind(name)
        .bind(description)
        .bind(content)
        .bind(conditions)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(Skill {
            id,
            user_id: user_id.to_string(),
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            content: content.to_string(),
            conditions: conditions.map(|s| s.to_string()),
            version: 1,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Get a skill by name.
    pub async fn get_by_name(&self, user_id: &str, name: &str) -> Result<Option<Skill>> {
        let row = sqlx::query_as::<_, (String, String, String, Option<String>, String, Option<String>, i32, String, String)>(
            r#"SELECT id, user_id, name, description, content, conditions, version, created_at, updated_at
               FROM skills WHERE user_id = ? AND name = ?"#,
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, user_id, name, description, content, conditions, version, created_at, updated_at)| Skill {
            id, user_id, name, description, content, conditions, version, created_at, updated_at,
        }))
    }

    /// List all skills for a user.
    pub async fn list(&self, user_id: &str) -> Result<Vec<Skill>> {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>, String, Option<String>, i32, String, String)>(
            r#"SELECT id, user_id, name, description, content, conditions, version, created_at, updated_at
               FROM skills WHERE user_id = ? ORDER BY name"#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|(id, user_id, name, description, content, conditions, version, created_at, updated_at)| Skill {
            id, user_id, name, description, content, conditions, version, created_at, updated_at,
        }).collect())
    }

    /// Patch a skill (increment version, update content).
    pub async fn patch(&self, user_id: &str, name: &str, patch_instructions: &str) -> Result<Skill> {
        let now = chrono::Utc::now().naive_utc().to_string();

        // Get current skill
        let current = self.get_by_name(user_id, name).await?
            .ok_or_else(|| anyhow::anyhow!("Skill not found: {}", name))?;

        // Append patch instructions to content
        let new_content = format!(
            "{}\n\n--- Patch v{} ---\n{}",
            current.content,
            current.version + 1,
            patch_instructions
        );

        sqlx::query(
            r#"UPDATE skills SET content = ?, version = version + 1, updated_at = ?
               WHERE user_id = ? AND name = ?"#,
        )
        .bind(&new_content)
        .bind(&now)
        .bind(user_id)
        .bind(name)
        .execute(&self.pool)
        .await?;

        self.get_by_name(user_id, name).await.map(|s| s.unwrap())
    }

    /// Delete a skill.
    pub async fn delete(&self, user_id: &str, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM skills WHERE user_id = ? AND name = ?")
            .bind(user_id)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Build the skill index for system prompt injection.
    pub async fn build_skill_index(&self, user_id: &str) -> String {
        match self.list(user_id).await {
            Ok(skills) if skills.is_empty() => String::new(),
            Ok(skills) => {
                let entries = skills
                    .iter()
                    .map(|s| {
                        let desc = s.description.as_deref().unwrap_or("");
                        format!("- {} (v{}): {}", s.name, s.version, desc)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("Available skills:\n{}", entries)
            }
            Err(e) => {
                tracing::warn!("Failed to build skill index: {}", e);
                String::new()
            }
        }
    }
}

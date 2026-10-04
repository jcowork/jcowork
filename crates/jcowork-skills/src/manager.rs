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

/// Column tuple shared by the SELECT queries: id, user_id, name, description,
/// content, conditions, version, origin, source_path, created_at, updated_at.
type SkillRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    i32,
    String,
    Option<String>,
    String,
    String,
);

fn row_to_skill(r: SkillRow) -> Skill {
    Skill {
        id: r.0,
        user_id: r.1,
        name: r.2,
        description: r.3,
        content: r.4,
        conditions: r.5,
        version: r.6,
        origin: r.7,
        source_path: r.8,
        created_at: r.9,
        updated_at: r.10,
    }
}

const SKILL_COLUMNS: &str =
    "id, user_id, name, description, content, conditions, version, origin, source_path, created_at, updated_at";

impl SkillManager {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a new agent-authored skill.
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
            r#"INSERT INTO skills (id, user_id, name, description, content, conditions, version, origin, source_path, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, 1, 'agent', NULL, ?, ?)"#,
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
            origin: Skill::ORIGIN_AGENT.to_string(),
            source_path: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Import (or re-import) a third-party skill, keyed by name.
    ///
    /// - Existing third-party skill with the same name: content, description
    ///   and source_path are refreshed and the version is bumped.
    /// - No existing skill: a new row with origin = "thirdparty" is created.
    /// - An existing *agent* skill with the same name is left untouched and
    ///   reported as a conflict so callers never silently overwrite it.
    ///
    /// Returns `(skill, created)`.
    pub async fn import_thirdparty(
        &self,
        user_id: &str,
        name: &str,
        description: Option<&str>,
        content: &str,
        source_path: Option<&str>,
    ) -> Result<(Skill, bool)> {
        let now = chrono::Utc::now().naive_utc().to_string();

        if let Some(existing) = self.get_by_name(user_id, name).await? {
            if existing.origin != Skill::ORIGIN_THIRDPARTY {
                anyhow::bail!(
                    "已存在同名的自建技能 '{}'，请换一个技能名称或先删除该技能",
                    name
                );
            }

            sqlx::query(
                r#"UPDATE skills
                   SET description = ?, content = ?, source_path = ?, version = version + 1, updated_at = ?
                   WHERE user_id = ? AND name = ?"#,
            )
            .bind(description)
            .bind(content)
            .bind(source_path)
            .bind(&now)
            .bind(user_id)
            .bind(name)
            .execute(&self.pool)
            .await?;

            let updated = self
                .get_by_name(user_id, name)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Skill vanished during update: {}", name))?;
            return Ok((updated, false));
        }

        let id = Uuid::new_v4().to_string();
        sqlx::query(
            r#"INSERT INTO skills (id, user_id, name, description, content, conditions, version, origin, source_path, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, NULL, 1, 'thirdparty', ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(user_id)
        .bind(name)
        .bind(description)
        .bind(content)
        .bind(source_path)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok((
            Skill {
                id,
                user_id: user_id.to_string(),
                name: name.to_string(),
                description: description.map(|s| s.to_string()),
                content: content.to_string(),
                conditions: None,
                version: 1,
                origin: Skill::ORIGIN_THIRDPARTY.to_string(),
                source_path: source_path.map(|s| s.to_string()),
                created_at: now.clone(),
                updated_at: now,
            },
            true,
        ))
    }

    /// Get a skill by its ID.
    pub async fn get_by_id(&self, user_id: &str, id: &str) -> Result<Option<Skill>> {
        let row = sqlx::query_as::<_, SkillRow>(&format!(
            "SELECT {} FROM skills WHERE user_id = ? AND id = ?",
            SKILL_COLUMNS
        ))
        .bind(user_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(row_to_skill))
    }

    /// Get a skill by name.
    pub async fn get_by_name(&self, user_id: &str, name: &str) -> Result<Option<Skill>> {
        let row = sqlx::query_as::<_, SkillRow>(&format!(
            "SELECT {} FROM skills WHERE user_id = ? AND name = ?",
            SKILL_COLUMNS
        ))
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(row_to_skill))
    }

    /// List all skills for a user.
    pub async fn list(&self, user_id: &str) -> Result<Vec<Skill>> {
        let rows = sqlx::query_as::<_, SkillRow>(&format!(
            "SELECT {} FROM skills WHERE user_id = ? ORDER BY name",
            SKILL_COLUMNS
        ))
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(row_to_skill).collect())
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

    /// Delete a skill by ID, returning the removed row when it existed.
    pub async fn delete_by_id(&self, user_id: &str, id: &str) -> Result<Option<Skill>> {
        let Some(skill) = self.get_by_id(user_id, id).await? else {
            return Ok(None);
        };
        sqlx::query("DELETE FROM skills WHERE user_id = ? AND id = ?")
            .bind(user_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(Some(skill))
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

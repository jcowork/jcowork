//! Skill listing, toggling and third-party import endpoints.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{AppState, AuthUser, SkillInfo};
use jcowork_skills::builtin_skills;
use jcowork_skills::models::Skill;

pub(crate) async fn list_skills(
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    let skills: Vec<SkillInfo> = Vec::new();
    (StatusCode::OK, Json(skills))
}

pub(crate) async fn create_skill(
    _auth: axum::Extension<AuthUser>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"status": "created"})))
}

/// Skill list entries embed at most this much instruction text; longer skills
/// are shown as a short preview (the full document stays in the workspace file
/// for third-party skills and is read by the agent on demand).
const CONTENT_INLINE_LIMIT: usize = 64 * 1024;
const CONTENT_PREVIEW_BYTES: usize = 8 * 1024;

/// Unified skill entry returned by /api/skills/all
#[derive(Debug, Serialize)]
pub(crate) struct SkillEntry {
    id: String,
    name: String,
    description: String,
    content: String,
    source: String, // "builtin" or "user"
    /// "builtin", "agent" (saved by the agent) or "thirdparty" (imported).
    origin: String,
    version: i32,
    enabled: bool,
    /// Workspace-relative path of the on-demand instruction file, when the
    /// skill's full document lives in the user's workspace (third-party).
    source_path: Option<String>,
}

/// Clip oversized content into a transport/UI-friendly preview.
fn preview_content(content: &str, source_path: Option<&str>) -> String {
    if content.len() <= CONTENT_INLINE_LIMIT {
        return content.to_string();
    }
    let head = jcowork_tools::base::truncate_str(content, CONTENT_PREVIEW_BYTES);
    match source_path {
        Some(p) => format!(
            "{}\n\n... (技能内容过长，此处仅显示开头预览；完整内容见工作区文件 {})",
            head, p
        ),
        None => format!("{}\n\n... (技能内容过长，此处仅显示开头预览)", head),
    }
}

fn user_skill_entry(s: &Skill, enabled: bool) -> SkillEntry {
    SkillEntry {
        id: s.id.clone(),
        name: s.name.clone(),
        description: s.description.clone().unwrap_or_default(),
        content: preview_content(&s.content, s.source_path.as_deref()),
        source: "user".to_string(),
        origin: s.origin.clone(),
        version: s.version,
        enabled,
        source_path: s.source_path.clone(),
    }
}

pub(crate) async fn list_all_skills(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
) -> impl IntoResponse {
    // Load enabled skill IDs from memory (category = "skill_enabled", content = skill id)
    let enabled_ids: std::collections::HashSet<String> = state
        .memory_manager
        .recall_all(&auth_user.user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.category == "skill_enabled")
        .map(|e| e.content)
        .collect();

    let mut entries: Vec<SkillEntry> = Vec::new();

    // Built-in skills (hidden ones stay functional but are not shown in the UI)
    for s in builtin_skills().iter().filter(|s| !s.hidden) {
        entries.push(SkillEntry {
            id: s.id.to_string(),
            name: s.name.to_string(),
            description: s.description.to_string(),
            content: s.content.to_string(),
            source: "builtin".to_string(),
            origin: "builtin".to_string(),
            version: 1,
            enabled: enabled_ids.contains(s.id),
            source_path: None,
        });
    }

    // User skills (agent-authored + third-party imports)
    if let Ok(user_skills) = state.skill_manager.list(&auth_user.user_id).await {
        for s in user_skills {
            let enabled = enabled_ids.contains(&s.id);
            entries.push(user_skill_entry(&s, enabled));
        }
    }

    (StatusCode::OK, Json(entries))
}

/// Replace the enabled state of a skill, storing it as a `skill_enabled`
/// memory entry (single source of truth shared with the agent).
async fn set_skill_enabled(
    state: &AppState,
    user_id: &str,
    skill_id: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    if let Ok(entries) = state.memory_manager.recall_all(user_id).await {
        for entry in entries
            .into_iter()
            .filter(|e| e.category == "skill_enabled" && e.content == skill_id)
        {
            let _ = state.memory_manager.delete(user_id, &entry.id).await;
        }
    }
    if enabled {
        state
            .memory_manager
            .save(user_id, skill_id, "skill_enabled")
            .await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToggleSkillRequest {
    enabled: bool,
}

pub(crate) async fn toggle_skill(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(skill_id): Path<String>,
    Json(req): Json<ToggleSkillRequest>,
) -> impl IntoResponse {
    match set_skill_enabled(&state, &auth_user.user_id, &skill_id, req.enabled).await {
        Ok(_) => {
            let status = if req.enabled { "enabled" } else { "disabled" };
            (StatusCode::OK, Json(serde_json::json!({ "status": status })))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

// ── Third-party skill import ──

fn err_json(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportSkillRequest {
    /// GitHub repository / file URL to import from.
    pub url: Option<String>,
    /// Raw SKILL.md content (alternative to `url`).
    pub content: Option<String>,
    /// Optional name override (mainly useful with `content`).
    pub name: Option<String>,
}

/// Write imported files into the skill folder (paths are validated).
async fn write_skill_files(dir: &std::path::Path, files: &[(String, String)]) -> anyhow::Result<()> {
    for (rel, content) in files {
        if rel.starts_with('/')
            || rel.contains('\\')
            || rel.split('/').any(|seg| seg.is_empty() || seg == ".." || seg == ".")
        {
            anyhow::bail!("非法的文件路径: {}", rel);
        }
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&abs, content).await?;
    }
    Ok(())
}

/// POST /api/skills/import — import a third-party skill from GitHub or from
/// pasted SKILL.md content. Files are mirrored into
/// `<workspace>/skills/<name>/` and only metadata is injected into the prompt.
pub(crate) async fn import_skill(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Json(req): Json<ImportSkillRequest>,
) -> Response {
    const MAX_PASTED_BYTES: usize = 4 * 1024 * 1024;

    let url = req.url.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let pasted = req.content.as_deref().filter(|s| !s.trim().is_empty());

    let imported: jcowork_skills::ImportedSkill = if let Some(url) = url {
        match jcowork_skills::fetch_github_skill(url).await {
            Ok(s) => s,
            Err(e) => return err_json(StatusCode::BAD_REQUEST, format!("导入失败: {:#}", e)),
        }
    } else if let Some(content) = pasted {
        if content.len() > MAX_PASTED_BYTES {
            return err_json(StatusCode::BAD_REQUEST, "技能内容过大（上限 4MB）");
        }
        let fallback = req
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("imported-skill");
        let doc = jcowork_skills::parse_skill_document(content, fallback);
        let name = req
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| jcowork_skills::import::sanitize_skill_name(n, fallback))
            .unwrap_or(doc.name);
        jcowork_skills::ImportedSkill {
            name,
            description: doc.description,
            repo_url: String::new(),
            skill_file: "SKILL.md".to_string(),
            files: vec![("SKILL.md".to_string(), content.to_string())],
        }
    } else {
        return err_json(
            StatusCode::BAD_REQUEST,
            "请提供 GitHub 链接（url）或技能内容（content）",
        );
    };

    let entry_content = imported
        .files
        .iter()
        .find(|(p, _)| p == &imported.skill_file)
        .map(|(_, c)| c.clone())
        .unwrap_or_default();
    if entry_content.trim().is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "技能入口文件 SKILL.md 内容为空");
    }

    // Name conflicts: never overwrite a skill of another origin.
    let existing = match state
        .skill_manager
        .get_by_name(&auth_user.user_id, &imported.name)
        .await
    {
        Ok(s) => s,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if let Some(s) = &existing {
        if s.origin != Skill::ORIGIN_THIRDPARTY {
            return err_json(
                StatusCode::CONFLICT,
                format!("已存在同名的自建技能「{}」，请先删除它或换一个技能名称", s.name),
            );
        }
    }

    // Mirror the files into the user's workspace so the agent can read them
    // on demand (file_read/file_search are workspace-scoped).
    let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
    let skill_dir: PathBuf = PathBuf::from(&workspace_root)
        .join("skills")
        .join(&imported.name);
    let dir_preexisted = skill_dir.exists();
    if existing.is_none() && dir_preexisted {
        return err_json(
            StatusCode::CONFLICT,
            format!(
                "工作区已存在目录 skills/{}，请先移走或删除该目录再导入",
                imported.name
            ),
        );
    }

    if let Err(e) = write_skill_files(&skill_dir, &imported.files).await {
        return err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入技能文件失败: {:#}", e),
        );
    }

    let source_path = format!("skills/{}/{}", imported.name, imported.skill_file);
    let (skill, created) = match state
        .skill_manager
        .import_thirdparty(
            &auth_user.user_id,
            &imported.name,
            imported.description.as_deref(),
            &entry_content,
            Some(&source_path),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            // Best-effort rollback when we created the folder ourselves.
            if !dir_preexisted {
                let _ = tokio::fs::remove_dir_all(&skill_dir).await;
            }
            return err_json(StatusCode::BAD_REQUEST, format!("保存技能失败: {:#}", e));
        }
    };

    // Freshly imported skills start enabled, ready to use.
    let _ = set_skill_enabled(&state, &auth_user.user_id, &skill.id, true).await;

    tracing::info!(
        user = %auth_user.username,
        skill = %skill.name,
        files = imported.files.len(),
        created,
        "Third-party skill imported"
    );

    let file_list: Vec<&String> = imported.files.iter().map(|(p, _)| p).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": if created { "created" } else { "updated" },
            "name": skill.name,
            "version": skill.version,
            "files": file_list,
        })),
    )
        .into_response()
}

/// DELETE /api/skills/{id} — delete a user skill (third-party skills also
/// remove their mirrored workspace folder).
pub(crate) async fn delete_skill(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(skill_id): Path<String>,
) -> Response {
    if skill_id.starts_with("builtin:") {
        return err_json(StatusCode::BAD_REQUEST, "内置技能不可删除，只能停用");
    }

    let skill = match state
        .skill_manager
        .delete_by_id(&auth_user.user_id, &skill_id)
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => return err_json(StatusCode::NOT_FOUND, "技能不存在"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    // Drop the enabled-state entry.
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries
            .into_iter()
            .filter(|e| e.category == "skill_enabled" && e.content == skill_id)
        {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }

    // Remove mirrored files for third-party skills (only under skills/).
    if skill.origin == Skill::ORIGIN_THIRDPARTY {
        if let Some(rel) = skill.source_path.as_deref() {
            let segments: Vec<&str> = rel.split('/').collect();
            let safe = segments.first() == Some(&"skills")
                && segments.len() >= 2
                && segments
                    .iter()
                    .all(|s| !s.is_empty() && *s != ".." && *s != ".");
            if safe {
                let workspace_root = format!("{}/{}/workspace", state.data_dir, auth_user.user_id);
                let dir = PathBuf::from(&workspace_root)
                    .join(segments[0])
                    .join(segments[1]);
                if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                    tracing::warn!(error = %e, dir = %dir.display(), "Failed to remove skill folder");
                }
            }
        }
    }

    tracing::info!(user = %auth_user.username, skill = %skill.name, "Skill deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "deleted", "name": skill.name })),
    )
        .into_response()
}

/// Memory category prefix for per-skill configuration entries.
/// One entry per skill: category = "skill_config:{skill_id}", content = config value.
fn skill_config_category(skill_id: &str) -> String {
    format!("skill_config:{}", skill_id)
}

/// GET /api/skills/{id}/config — returns the skill's config (e.g. selected VL model).
pub(crate) async fn get_skill_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(skill_id): Path<String>,
) -> impl IntoResponse {
    let vl_model = state
        .memory_manager
        .recall_all(&auth_user.user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|e| e.category == skill_config_category(&skill_id))
        .map(|e| e.content);
    (StatusCode::OK, Json(serde_json::json!({ "vl_model": vl_model })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct SaveSkillConfigRequest {
    /// "provider:model" string, or null/empty to clear the selection.
    vl_model: Option<String>,
}

/// PUT /api/skills/{id}/config — save or clear the skill's config.
pub(crate) async fn save_skill_config(
    State(state): State<AppState>,
    axum::Extension(auth_user): axum::Extension<AuthUser>,
    Path(skill_id): Path<String>,
    Json(req): Json<SaveSkillConfigRequest>,
) -> impl IntoResponse {
    let category = skill_config_category(&skill_id);

    // Replace any existing config entry for this skill
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries.into_iter().filter(|e| e.category == category) {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }

    match req.vl_model.filter(|m| !m.is_empty()) {
        Some(model) => match state.memory_manager.save(&auth_user.user_id, &model, &category).await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "saved" }))),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))),
        },
        None => (StatusCode::OK, Json(serde_json::json!({ "status": "cleared" }))),
    }
}

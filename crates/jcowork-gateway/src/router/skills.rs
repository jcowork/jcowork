//! Skill listing and toggling endpoints.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use super::{AppState, AuthUser, SkillInfo};
use jcowork_skills::builtin_skills;

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

/// Unified skill entry returned by /api/skills/all
#[derive(Debug, Serialize)]
pub(crate) struct SkillEntry {
    id: String,
    name: String,
    description: String,
    content: String,
    source: String, // "builtin" or "user"
    version: i32,
    enabled: bool,
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
            version: 1,
            enabled: enabled_ids.contains(s.id),
        });
    }

    // User skills
    if let Ok(user_skills) = state.skill_manager.list(&auth_user.user_id).await {
        for s in user_skills {
            let enabled = enabled_ids.contains(&s.id);
            entries.push(SkillEntry {
                id: s.id.clone(),
                name: s.name,
                description: s.description.unwrap_or_default(),
                content: s.content,
                source: "user".to_string(),
                version: s.version,
                enabled,
            });
        }
    }

    (StatusCode::OK, Json(entries))
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
    // Remove existing entry for this skill_id in skill_enabled category
    if let Ok(entries) = state.memory_manager.recall_all(&auth_user.user_id).await {
        for entry in entries.into_iter().filter(|e| e.category == "skill_enabled" && e.content == skill_id) {
            let _ = state.memory_manager.delete(&auth_user.user_id, &entry.id).await;
        }
    }
    if req.enabled {
        match state.memory_manager.save(&auth_user.user_id, &skill_id, "skill_enabled").await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "enabled" }))),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))),
        }
    } else {
        (StatusCode::OK, Json(serde_json::json!({ "status": "disabled" })))
    }
}

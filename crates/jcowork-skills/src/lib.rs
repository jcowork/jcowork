//! Jcowork Skills - Skill system with CRUD, patch, and discovery.

pub mod builtin;
pub mod import;
pub mod loader;
pub mod manager;
pub mod models;

pub use builtin::{builtin_skills, BuiltinSkill};
pub use import::{fetch_github_skill, parse_skill_document, GithubSource, ImportedSkill};
pub use manager::SkillManager;

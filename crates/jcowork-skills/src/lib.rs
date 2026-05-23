//! Jcowork Skills - Skill system with CRUD, patch, and discovery.

pub mod builtin;
pub mod loader;
pub mod manager;
pub mod models;

pub use builtin::{builtin_skills, BuiltinSkill};
pub use manager::SkillManager;

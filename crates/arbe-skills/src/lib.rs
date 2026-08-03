//! Skill manifest loading and scope resolution (harness spec FR-6, overall
//! design §4.3).

pub mod error;
pub mod loader;
pub mod manifest;
pub mod merge;

pub use error::SkillError;
pub use loader::load_dir;
pub use manifest::parse_manifest;
pub use merge::merge_skills;

use serde::{Deserialize, Serialize};

/// Where a skill was resolved from; determines merge precedence
/// (session-local > project-local > global).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    SessionLocal,
    ProjectLocal,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub scope: SkillScope,
    pub instructions: String,
    pub tags: Vec<String>,
}

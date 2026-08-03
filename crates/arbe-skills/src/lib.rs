//! Skill manifest loading and scope resolution (harness spec FR-6, overall
//! design §4.3). Manifest parsing and merge policy land in Phase 5; this
//! crate currently defines only the shared contract.

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

//! Skill manifest loading and scope resolution (harness spec FR-6, overall
//! design §4.3).

pub mod error;
pub mod loader;
pub mod manifest;
pub mod merge;

pub use error::SkillError;
pub use loader::{Loaded, load_dir};
pub use manifest::parse_manifest;
pub use merge::merge_skills;

use serde::{Deserialize, Serialize};

/// Where a skill was resolved from; determines merge precedence
/// (project-local > global).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
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

/// The skills in effect for a session, after merging scopes.
#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    skills: Vec<SkillManifest>,
}

impl SkillSet {
    pub fn new(skills: Vec<SkillManifest>) -> Self {
        Self { skills }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn get(&self, name: &str) -> Option<&SkillManifest> {
        self.skills.iter().find(|s| s.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// Every skill's full instructions (for "always" mode).
    pub fn instructions(&self) -> Vec<String> {
        self.skills.iter().map(|s| s.instructions.clone()).collect()
    }

    /// A short listing for the prompt ("on demand" mode): one line per
    /// skill with its name and description, plus how to load one.
    pub fn index(&self, load_tool: &str) -> String {
        let mut text = format!(
            "Skills available — call `{load_tool}` with a skill's name to read its full instructions before doing work it covers:\n"
        );
        for skill in &self.skills {
            text.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str) -> SkillManifest {
        SkillManifest {
            name: name.into(),
            description: description.into(),
            scope: SkillScope::Global,
            instructions: format!("{name} body"),
            tags: vec![],
        }
    }

    #[test]
    fn the_index_lists_names_and_descriptions_but_not_bodies() {
        let set = SkillSet::new(vec![
            skill("rust-style", "House style"),
            skill("deploy", "How to ship"),
        ]);
        let index = set.index("load_skill");
        assert!(index.contains("`load_skill`"));
        assert!(index.contains("- rust-style: House style"));
        assert!(index.contains("- deploy: How to ship"));
        assert!(!index.contains("body"));
        assert_eq!(set.get("deploy").unwrap().instructions, "deploy body");
        assert!(set.get("nope").is_none());
    }
}

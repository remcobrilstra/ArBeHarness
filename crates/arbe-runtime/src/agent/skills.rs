//! Skills for a session: loading the global and project scopes, and the
//! `load_skill` tool that fetches one skill's instructions on demand.

use std::path::Path;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use arbe_skills::{SkillScope, SkillSet};
use arbe_tools::{ToolContext, ToolDescription, ToolExecutor, schemars};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

/// Registry name of the on-demand loader.
pub(super) const LOAD_SKILL_TOOL: &str = "load_skill";

/// Global skills (`~/.arbe/skills/`) and project skills
/// (`<project>/.arbe/skills/`, then `<project>/.agents/skills/` — the
/// folder other agents share); a project skill replaces a global one of
/// the same name, and `.arbe` wins over `.agents`. Returns the merged set
/// and a readable problem for each file that couldn't be loaded.
pub(super) fn load_session_skills(
    global_dir: &Path,
    project_dir: &Path,
) -> (SkillSet, Vec<String>) {
    let global = arbe_skills::load_dir(global_dir, SkillScope::Global);
    let ours = arbe_skills::load_dir(
        &project_dir.join(".arbe").join("skills"),
        SkillScope::ProjectLocal,
    );
    let shared = arbe_skills::load_dir(
        &project_dir.join(".agents").join("skills"),
        SkillScope::ProjectLocal,
    );
    let problems = global
        .problems
        .iter()
        .chain(&ours.problems)
        .chain(&shared.problems)
        .map(|p| format!("skill skipped — {p}"))
        .collect();
    let project = arbe_skills::merge_skills(ours.skills, shared.skills);
    let mut merged = arbe_skills::merge_skills(project, global.skills);
    // Folders as the tools see them: relative to the project when inside it.
    for skill in &mut merged {
        if let Some(dir) = &skill.dir
            && let Ok(relative) = dir.strip_prefix(project_dir)
        {
            skill.dir = Some(relative.to_path_buf());
        }
    }
    (SkillSet::new(merged), problems)
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    /// Name of the skill, exactly as listed.
    name: String,
}

/// Returns one skill's full instructions. Read-only and harmless, so it is
/// low risk and always available when skills are loaded on demand.
pub(super) struct LoadSkillTool {
    skills: SkillSet,
}

impl LoadSkillTool {
    pub(super) fn new(skills: SkillSet) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl ToolExecutor for LoadSkillTool {
    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid load_skill arguments: {e}")))?;
        let (output, is_error) = match self.skills.get(&args.name) {
            Some(skill) => match &skill.dir {
                Some(dir) => (
                    format!(
                        "{}\n\n(This skill's files are in `{}`; paths in it are relative to that folder.)",
                        skill.instructions,
                        dir.display().to_string().replace('\\', "/")
                    ),
                    false,
                ),
                None => (skill.instructions.clone(), false),
            },
            None => (
                format!(
                    "no skill named {:?}; available: {}",
                    args.name,
                    self.skills.names().join(", ")
                ),
                true,
            ),
        };
        Ok(ToolResult {
            id: invocation.id,
            output: Value::String(output),
            is_error,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Read a skill's full instructions. The available skills are listed in the system prompt.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ToolCallId, TurnId};
    use serde_json::json;

    fn write_skill(dir: &Path, file: &str, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(file),
            format!("---\nname: {name}\ndescription: about {name}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn project_skills_override_global_ones_and_problems_are_reported() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global");
        let project = root.path().join("project");
        write_skill(&global, "style.md", "style", "global style");
        write_skill(&global, "deploy.md", "deploy", "how to deploy");
        write_skill(
            &project.join(".arbe/skills"),
            "style.md",
            "style",
            "project style",
        );
        std::fs::write(global.join("broken.md"), "no frontmatter").unwrap();

        let (skills, problems) = load_session_skills(&global, &project);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills.get("style").unwrap().instructions, "project style");
        assert_eq!(skills.get("deploy").unwrap().instructions, "how to deploy");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("broken.md"), "{}", problems[0]);
    }

    #[tokio::test]
    async fn shared_agents_skill_folders_load_and_point_at_their_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let shared = project.join(".agents/skills");
        write_skill(
            &shared.join("pdf"),
            "SKILL.md",
            "pdf",
            "run scripts/fill.py",
        );
        write_skill(&shared, "style.md", "style", "shared style");
        write_skill(
            &project.join(".arbe/skills"),
            "style.md",
            "style",
            "our style",
        );

        let (skills, problems) = load_session_skills(&root.path().join("global"), &project);
        assert!(problems.is_empty(), "{problems:?}");
        // `.arbe/skills` wins over `.agents/skills`.
        assert_eq!(skills.get("style").unwrap().instructions, "our style");

        let tool = LoadSkillTool::new(skills);
        let call = ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: LOAD_SKILL_TOOL.into(),
            arguments: json!({ "name": "pdf" }),
            risk: RiskLevel::Low,
            rationale: None,
        };
        let found = tool
            .execute(call, &ToolContext::for_testing())
            .await
            .unwrap();
        let text = found.output.as_str().unwrap();
        assert!(text.starts_with("run scripts/fill.py"), "{text}");
        // Relative to the project, so the file tools can open them.
        assert!(text.contains("are in `.agents/skills/pdf`"), "{text}");
    }

    #[tokio::test]
    async fn load_skill_returns_the_body_or_lists_what_exists() {
        let root = tempfile::tempdir().unwrap();
        write_skill(root.path(), "deploy.md", "deploy", "run make release");
        let (skills, _) = load_session_skills(root.path(), &root.path().join("none"));
        let tool = LoadSkillTool::new(skills);
        let call = |name: &str| ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: LOAD_SKILL_TOOL.into(),
            arguments: json!({ "name": name }),
            risk: RiskLevel::Low,
            rationale: None,
        };
        let found = tool
            .execute(call("deploy"), &ToolContext::for_testing())
            .await
            .unwrap();
        assert_eq!(found.output, json!("run make release"));
        let missing = tool
            .execute(call("nope"), &ToolContext::for_testing())
            .await
            .unwrap();
        assert!(missing.is_error);
        assert!(
            missing
                .output
                .as_str()
                .unwrap()
                .contains("available: deploy")
        );
    }
}

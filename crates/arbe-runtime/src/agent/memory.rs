//! Persistent memory (v2 plan P5.6): notes that carry over between
//! sessions, in `~/.arbe/memory/global/memory.md` (every project) and
//! `~/.arbe/memory/projects/<project-id>/memory.md` (one project). Both
//! are read into every request; the `remember` tool appends to them.

use std::path::{Path, PathBuf};

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use arbe_tools::{ToolContext, ToolDescription, ToolExecutor, schemars};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

pub(super) const REMEMBER_TOOL: &str = "remember";

/// Longest memory file content put into a request (per file).
const MAX_MEMORY_CHARS: usize = 8_000;

/// A stable, readable id for a project directory: its folder name plus a
/// short hash of the full (canonical) path, so two projects with the same
/// folder name don't share memory. FNV-1a rather than std's hasher, whose
/// output isn't guaranteed stable across Rust versions.
pub(super) fn project_id(project_dir: &Path) -> String {
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let text = canonical.to_string_lossy().to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name: String = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!("{name}-{:08x}", hash as u32)
}

fn memory_file(home: &Path, scope: Scope, project_dir: &Path) -> PathBuf {
    match scope {
        Scope::Global => home.join("memory").join("global").join("memory.md"),
        Scope::Project => home
            .join("memory")
            .join("projects")
            .join(project_id(project_dir))
            .join("memory.md"),
    }
}

/// The memory notes for a request: global then project, each framed so
/// the model knows what they are, capped, skipped when empty or unreadable.
pub(super) fn load_notes(home: &Path, project_dir: &Path) -> Vec<String> {
    [
        (Scope::Global, "saved for every project"),
        (Scope::Project, "saved for this project"),
    ]
    .into_iter()
    .filter_map(|(scope, label)| {
        let text = std::fs::read_to_string(memory_file(home, scope, project_dir)).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| {
            let capped: String = text.chars().take(MAX_MEMORY_CHARS).collect();
            format!("Memory — notes from earlier sessions, {label}:\n{capped}")
        })
    })
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Project,
    Global,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    /// A short fact worth knowing in future sessions (a preference, a
    /// convention, where something lives). One line.
    note: String,
    /// `project` (default) for this project only, `global` for every
    /// project.
    #[serde(default)]
    scope: Option<Scope>,
}

/// Appends a note to a memory file. Medium risk: it writes something that
/// shapes every future session, so it goes through approval like any
/// other write.
pub(super) struct RememberTool {
    home: PathBuf,
    project_dir: PathBuf,
}

impl RememberTool {
    pub(super) fn new(home: PathBuf, project_dir: PathBuf) -> Self {
        Self { home, project_dir }
    }
}

#[async_trait]
impl ToolExecutor for RememberTool {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid remember arguments: {e}")))?;
        let note = args.note.trim().replace(['\n', '\r'], " ");
        if note.is_empty() {
            return Err(ToolError::Validation("note is empty".into()));
        }
        let scope = args.scope.unwrap_or(Scope::Project);
        let path = memory_file(&self.home, scope, &self.project_dir);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("- {note}\n"));
        arbe_storage::atomic::write_atomic(&path, text.as_bytes())
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;
        Ok(ToolResult {
            id: invocation.id,
            output: Value::String(format!(
                "saved to {} memory",
                match scope {
                    Scope::Project => "project",
                    Scope::Global => "global",
                }
            )),
            is_error: false,
        })
    }

    fn parallel_safe(&self) -> bool {
        false
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Save a note about the user or this project to persistent memory, shown to you at the start of future sessions. Use it only when the user asks you to remember something, or states a lasting preference or convention (e.g. \"always use tabs\"). Never for general knowledge, answers to questions, or task progress.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ToolCallId, TurnId};
    use serde_json::json;

    fn call(args: Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: REMEMBER_TOOL.into(),
            arguments: args,
            risk: RiskLevel::Medium,
            rationale: None,
        }
    }

    #[test]
    fn project_ids_are_stable_readable_and_distinct() {
        let a = Path::new("/work/one/app");
        let b = Path::new("/work/two/app");
        assert_eq!(project_id(a), project_id(a));
        assert_ne!(project_id(a), project_id(b));
        assert!(project_id(a).starts_with("app-"));
    }

    #[tokio::test]
    async fn remembered_notes_come_back_as_memory_notes_per_scope() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let tool = RememberTool::new(home.path().into(), project.path().into());
        assert!(load_notes(home.path(), project.path()).is_empty());

        tool.execute(
            call(json!({"note": "uses tabs"})),
            &ToolContext::for_testing(),
        )
        .await
        .unwrap();
        tool.execute(
            call(json!({"note": "prefers short answers\nreally", "scope": "global"})),
            &ToolContext::for_testing(),
        )
        .await
        .unwrap();

        let notes = load_notes(home.path(), project.path());
        assert_eq!(notes.len(), 2);
        assert!(
            notes[0].contains("every project")
                && notes[0].contains("- prefers short answers really")
        );
        assert!(notes[1].contains("this project") && notes[1].contains("- uses tabs"));
        // Another project doesn't see this project's notes.
        let other = tempfile::tempdir().unwrap();
        assert_eq!(load_notes(home.path(), other.path()).len(), 1);
    }

    #[tokio::test]
    async fn an_empty_note_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let tool = RememberTool::new(home.path().into(), home.path().into());
        assert!(
            tool.execute(call(json!({"note": "  "})), &ToolContext::for_testing())
                .await
                .is_err()
        );
    }
}

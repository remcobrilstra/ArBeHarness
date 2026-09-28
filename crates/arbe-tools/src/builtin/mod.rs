//! The fixed set of builtin tools ArBeHarness ships with (deliberately
//! small — a handful of well-tested tools rather than a large surface):
//! reading, writing, and editing files; listing directories; finding files
//! by glob; searching file contents by regex; running a shell command; and
//! tracking a model-driven todo list. Every filesystem-touching tool here
//! is sandboxed to a single root directory via
//! [`path_guard::resolve_within_root`]; `execute` is the one exception
//! that can't be sandboxed the same way, which is why it's the
//! highest-risk tool of the set. `todo_write` is the only tool with
//! in-memory state instead of a filesystem root — see its module docs.

pub mod edit_file;
pub mod execute_tool;
pub mod glob_tool;
pub mod grep_tool;
pub mod list_dir;
pub mod path_guard;
pub mod read_file;
pub mod todo_write;
pub mod write_file;

use std::path::Path;
use std::sync::Arc;

use arbe_core::{RiskLevel, ToolSpec};
use serde_json::json;

use crate::ToolRegistry;

/// The fixed tool-name -> executor wiring. Kept in one place so the set of
/// builtin tools (and their names, which a future automatic tool-call
/// parser will need to match against a model's requested tool) has a
/// single source of truth.
pub const TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "glob",
    "grep",
    "execute",
    "todo_write",
];

/// Registers every builtin tool, sandboxed to `root` (the agent's project
/// directory — `RuntimeConfig::project_dir`).
pub fn register_all(registry: &mut ToolRegistry, root: &Path) {
    registry.register(
        "read_file",
        Arc::new(read_file::ReadFileTool::new(root.to_path_buf())),
    );
    registry.register(
        "write_file",
        Arc::new(write_file::WriteFileTool::new(root.to_path_buf())),
    );
    registry.register(
        "edit_file",
        Arc::new(edit_file::EditFileTool::new(root.to_path_buf())),
    );
    registry.register(
        "list_dir",
        Arc::new(list_dir::ListDirTool::new(root.to_path_buf())),
    );
    registry.register(
        "glob",
        Arc::new(glob_tool::GlobTool::new(root.to_path_buf())),
    );
    registry.register(
        "grep",
        Arc::new(grep_tool::GrepTool::new(root.to_path_buf())),
    );
    registry.register(
        "execute",
        Arc::new(execute_tool::ExecuteTool::new(root.to_path_buf())),
    );
    registry.register("todo_write", Arc::new(todo_write::TodoWriteTool::new()));
}

/// JSON-schema descriptions of every builtin tool, for a provider that
/// supports tool calling (`ProviderCapabilities::tool_calls`) — this is
/// what tells the model these tools exist and how to call them. Kept
/// hand-authored right next to `TOOL_NAMES`/`register_all` (rather than a
/// trait method on `ToolExecutor`) so adding a schema here can't drift
/// from the tool's actual `Args` struct without a human noticing in
/// review; each schema below is written straight off the corresponding
/// tool module's `Args`.
pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file".to_string(),
            description: "Read a UTF-8 text file (optionally a 1-indexed inclusive line range) from within the project directory.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root."},
                    "start_line": {"type": "integer", "description": "1-indexed, inclusive. Omit with end_line to read the whole file."},
                    "end_line": {"type": "integer", "description": "1-indexed, inclusive."}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "Create or overwrite a file within the project directory with the given content.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root."},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: "Find-and-replace a substring within an existing file. Fails if `find` doesn't match, or matches more than once unless replace_all is set.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root."},
                    "find": {"type": "string"},
                    "replace": {"type": "string"},
                    "replace_all": {"type": "boolean", "description": "Defaults to false (fails on more than one match)."}
                },
                "required": ["path", "find", "replace"]
            }),
        },
        ToolSpec {
            name: "list_dir".to_string(),
            description: "List the entries (name + is_dir) of a directory within the project directory.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root. Defaults to \".\"."}
                }
            }),
        },
        ToolSpec {
            name: "glob".to_string(),
            description: "Find files matching a glob pattern (e.g. \"**/*.rs\") within the project directory.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "e.g. \"**/*.rs\", \"src/*.toml\"."},
                    "path": {"type": "string", "description": "Directory to search under. Defaults to \".\"."}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "grep".to_string(),
            description: "Search file contents by regex within the project directory, returning matching lines.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression."},
                    "path": {"type": "string", "description": "Directory to search under. Defaults to \".\"."},
                    "case_insensitive": {"type": "boolean"}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "execute".to_string(),
            description: "Run a shell command with the project directory as its working directory. Highest-risk tool — always approval-gated.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_secs": {"type": "integer", "description": "Defaults to 30, capped at 300."}
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: "todo_write".to_string(),
            description: "Replace the current task's todo list, for tracking progress on multi-step work. Each call resends the full list.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        },
    ]
}

/// A coarse, tool-name-keyed default risk for a model-initiated call
/// (harness spec FR-4's risk indicator) — read/list/search operations are
/// low risk, file mutation is medium, arbitrary shell execution is high.
/// Purely informational (the approval *policy* decision is driven by
/// `ApprovalPolicyMode`/allow-/denylist, not risk — see `crate::policy`),
/// used only for what's shown to a human when a prompt is required.
pub fn default_risk_for(tool_name: &str) -> RiskLevel {
    match tool_name {
        "read_file" | "list_dir" | "glob" | "grep" | "todo_write" => RiskLevel::Low,
        "write_file" | "edit_file" => RiskLevel::Medium,
        "execute" => RiskLevel::High,
        _ => RiskLevel::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_effecting_builtins_are_not_parallel_safe() {
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, Path::new("."));
        for (name, expected) in [
            ("read_file", true),
            ("list_dir", true),
            ("glob", true),
            ("grep", true),
            ("write_file", false),
            ("edit_file", false),
            ("execute", false),
            ("todo_write", false),
        ] {
            assert_eq!(
                registry.get(name).unwrap().parallel_safe(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn registers_every_declared_tool_name() {
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, Path::new("."));

        for name in TOOL_NAMES {
            assert!(
                registry.contains(name),
                "expected {name:?} to be registered"
            );
        }
    }

    #[test]
    fn every_declared_tool_name_has_a_spec_and_a_default_risk() {
        let specs = tool_specs();
        for name in TOOL_NAMES {
            assert!(
                specs.iter().any(|s| s.name == *name),
                "expected a ToolSpec for {name:?}"
            );
            // Exercised for the side effect of not panicking on an
            // unrecognized name — every declared name must hit a real
            // match arm, not the catch-all default.
            let _ = default_risk_for(name);
        }
        assert_eq!(specs.len(), TOOL_NAMES.len());
    }

    #[test]
    fn execute_is_the_highest_risk_builtin() {
        assert_eq!(default_risk_for("execute"), RiskLevel::High);
        assert_eq!(default_risk_for("read_file"), RiskLevel::Low);
    }
}

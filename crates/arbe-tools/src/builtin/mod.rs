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
pub mod processes;
pub mod read_file;
pub mod todo_write;
pub mod write_file;

use std::path::Path;
use std::sync::Arc;

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
    "process_output",
    "process_kill",
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
    // One table per registry (= per session): background processes die
    // with the session.
    let processes = Arc::new(processes::ProcessTable::new());
    registry.register(
        "execute",
        Arc::new(execute_tool::ExecuteTool::with_processes(
            root.to_path_buf(),
            processes.clone(),
        )),
    );
    registry.register(
        "process_output",
        Arc::new(processes::ProcessOutputTool::new(processes.clone())),
    );
    registry.register(
        "process_kill",
        Arc::new(processes::ProcessKillTool::new(processes)),
    );
    registry.register("todo_write", Arc::new(todo_write::TodoWriteTool::new()));
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

    fn builtin_specs() -> Vec<arbe_core::ToolSpec> {
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, Path::new("."));
        registry.specs()
    }

    fn required(spec: &arbe_core::ToolSpec) -> Vec<&str> {
        let mut names: Vec<&str> = spec.parameters["required"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn every_builtin_describes_itself_with_an_object_schema() {
        let specs = builtin_specs();
        assert_eq!(specs.len(), TOOL_NAMES.len());
        for spec in &specs {
            assert!(
                !spec.description.is_empty(),
                "{} has no description",
                spec.name
            );
            assert_eq!(spec.parameters["type"], "object", "{}", spec.name);
            let text = spec.parameters.to_string();
            assert!(!text.contains("$ref"), "{} schema uses $ref", spec.name);
            assert!(
                !text.contains("$schema"),
                "{} schema has $schema",
                spec.name
            );
        }
    }

    /// The schema is derived from each tool's real argument type, so
    /// required-ness follows `#[serde(default)]`/`Option` exactly.
    #[test]
    fn required_arguments_follow_the_argument_types() {
        let specs = builtin_specs();
        let spec = |name: &str| specs.iter().find(|s| s.name == name).unwrap().clone();
        assert_eq!(required(&spec("read_file")), vec!["path"]);
        assert_eq!(required(&spec("write_file")), vec!["content", "path"]);
        assert_eq!(
            required(&spec("edit_file")),
            vec!["find", "path", "replace"]
        );
        assert_eq!(required(&spec("list_dir")), Vec::<&str>::new());
        assert_eq!(required(&spec("grep")), vec!["pattern"]);
        assert_eq!(required(&spec("execute")), vec!["command"]);
        assert_eq!(required(&spec("todo_write")), vec!["todos"]);
        // Field doc comments become the argument descriptions.
        assert_eq!(
            spec("read_file").parameters["properties"]["path"]["description"],
            "Path relative to the project root."
        );
        // Nested types are inlined, including the status enum.
        let status =
            &spec("todo_write").parameters["properties"]["todos"]["items"]["properties"]["status"];
        assert_eq!(
            status["enum"],
            serde_json::json!(["pending", "in_progress", "completed"])
        );
    }

    #[test]
    fn rule_subjects_are_the_path_or_the_command() {
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, Path::new("."));
        let subject =
            |tool: &str, args: serde_json::Value| registry.get(tool).unwrap().subject(&args);
        use serde_json::json;
        assert_eq!(
            subject("read_file", json!({"path": "./src\\lib.rs"})).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(subject("list_dir", json!({})).as_deref(), Some("."));
        assert_eq!(
            subject("execute", json!({"command": "  cargo test  "})).as_deref(),
            Some("cargo test")
        );
        assert_eq!(subject("todo_write", json!({"todos": []})), None);
    }

    #[test]
    fn risk_levels_come_from_the_tools() {
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, Path::new("."));
        use arbe_core::RiskLevel;
        assert_eq!(registry.risk_of("execute"), RiskLevel::High);
        assert_eq!(registry.risk_of("write_file"), RiskLevel::Medium);
        assert_eq!(registry.risk_of("read_file"), RiskLevel::Low);
        assert_eq!(registry.risk_of("todo_write"), RiskLevel::Low);
        assert_eq!(registry.risk_of("not-a-tool"), RiskLevel::Medium);
    }
}

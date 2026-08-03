//! The fixed set of builtin tools ArBeHarness ships with (deliberately
//! small — a handful of well-tested tools rather than a large surface):
//! reading, writing, and editing files; listing directories; finding files
//! by glob; searching file contents by regex; and running a shell
//! command. Every filesystem-touching tool here is sandboxed to a single
//! root directory via [`path_guard::resolve_within_root`]; `execute` is
//! the one exception that can't be sandboxed the same way, which is why
//! it's the highest-risk tool of the set.

pub mod edit_file;
pub mod execute_tool;
pub mod glob_tool;
pub mod grep_tool;
pub mod list_dir;
pub mod path_guard;
pub mod read_file;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

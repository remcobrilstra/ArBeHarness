use std::fs;
use std::path::{Path, PathBuf};

use crate::error::StorageError;
use crate::paths;

/// Reads `<arbe_home>/instructions/agent.md` — instructions that apply to
/// every session/project on the machine. Returns `Ok(None)` rather than an
/// error when the file doesn't exist: an absent global instructions file
/// is a normal starting state, not a failure.
pub fn read_global_instructions_at(arbe_home: &Path) -> Result<Option<String>, StorageError> {
    read_optional(&global_instructions_path(arbe_home))
}

/// Reads project-specific instructions from the project root: prefers
/// `<project_dir>/agent.md`, falling back to `<project_dir>/CLAUDE.md` if
/// `agent.md` doesn't exist. Returns `Ok(None)` if neither is present. Does
/// not walk up parent directories.
pub fn read_project_instructions(project_dir: &Path) -> Result<Option<String>, StorageError> {
    let agent_md = project_dir.join("agent.md");
    match read_optional(&agent_md)? {
        Some(contents) => Ok(Some(contents)),
        None => read_optional(&project_dir.join("CLAUDE.md")),
    }
}

fn global_instructions_path(arbe_home: &Path) -> PathBuf {
    paths::instructions_dir_at(arbe_home).join("agent.md")
}

fn read_optional(path: &Path) -> Result<Option<String>, StorageError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StorageError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic::write_atomic;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "arbe-instructions-test-{label}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn missing_global_instructions_is_none_not_an_error() {
        let home = temp_dir("global-missing");
        assert_eq!(read_global_instructions_at(&home).unwrap(), None);
    }

    #[test]
    fn reads_back_a_written_global_instructions_file() {
        let home = temp_dir("global-hit");
        write_atomic(&global_instructions_path(&home), b"be terse").unwrap();

        assert_eq!(
            read_global_instructions_at(&home).unwrap(),
            Some("be terse".to_string())
        );

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn missing_project_instructions_is_none_not_an_error() {
        let project = temp_dir("project-missing");
        assert_eq!(read_project_instructions(&project).unwrap(), None);
    }

    #[test]
    fn prefers_agent_md_over_claude_md() {
        let project = temp_dir("project-prefers-agent");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("agent.md"), "agent version").unwrap();
        fs::write(project.join("CLAUDE.md"), "claude version").unwrap();

        assert_eq!(
            read_project_instructions(&project).unwrap(),
            Some("agent version".to_string())
        );

        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn falls_back_to_claude_md_when_agent_md_absent() {
        let project = temp_dir("project-falls-back");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "claude version").unwrap();

        assert_eq!(
            read_project_instructions(&project).unwrap(),
            Some("claude version".to_string())
        );

        fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn does_not_walk_up_parent_directories() {
        let project = temp_dir("project-no-walk-up");
        let nested = project.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(project.join("agent.md"), "parent version").unwrap();

        assert_eq!(read_project_instructions(&nested).unwrap(), None);

        fs::remove_dir_all(&project).ok();
    }
}

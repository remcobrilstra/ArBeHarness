use std::fs;
use std::path::{Path, PathBuf};

use crate::error::StorageError;
use crate::paths;

/// Reads `<root>/memory/global/memory.md`. Returns `Ok(None)` rather than
/// an error when the file doesn't exist yet — an empty/absent memory file
/// is a normal starting state, not a failure (overall design §5.4).
pub fn read_global_memory_at(arbe_home: &Path) -> Result<Option<String>, StorageError> {
    read_optional(&global_memory_path(arbe_home))
}

/// Reads `<root>/memory/projects/<project_id>/memory.md`.
pub fn read_project_memory_at(
    arbe_home: &Path,
    project_id: &str,
) -> Result<Option<String>, StorageError> {
    read_optional(&project_memory_path(arbe_home, project_id))
}

/// Same as [`read_global_memory_at`] but resolves the root via
/// [`paths::arbe_home`] (honors `ARBE_HOME`).
pub fn read_global_memory() -> Result<Option<String>, StorageError> {
    read_global_memory_at(&paths::arbe_home())
}

/// Same as [`read_project_memory_at`] but resolves the root via
/// [`paths::arbe_home`] (honors `ARBE_HOME`).
pub fn read_project_memory(project_id: &str) -> Result<Option<String>, StorageError> {
    read_project_memory_at(&paths::arbe_home(), project_id)
}

fn global_memory_path(arbe_home: &Path) -> PathBuf {
    arbe_home.join("memory").join("global").join("memory.md")
}

fn project_memory_path(arbe_home: &Path, project_id: &str) -> PathBuf {
    arbe_home
        .join("memory")
        .join("projects")
        .join(project_id)
        .join("memory.md")
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

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("arbe-memfiles-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn missing_memory_files_are_none_not_an_error() {
        let home = temp_home();
        assert_eq!(read_global_memory_at(&home).unwrap(), None);
        assert_eq!(read_project_memory_at(&home, "some-project").unwrap(), None);
    }

    #[test]
    fn reads_back_a_written_global_memory_file() {
        let home = temp_home();
        write_atomic(&global_memory_path(&home), b"# notes\nremembered fact").unwrap();

        assert_eq!(
            read_global_memory_at(&home).unwrap(),
            Some("# notes\nremembered fact".to_string())
        );

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn reads_back_a_written_project_memory_file() {
        let home = temp_home();
        write_atomic(&project_memory_path(&home, "proj-a"), b"proj notes").unwrap();

        assert_eq!(
            read_project_memory_at(&home, "proj-a").unwrap(),
            Some("proj notes".to_string())
        );

        fs::remove_dir_all(&home).ok();
    }
}

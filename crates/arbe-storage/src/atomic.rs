use std::fs;
use std::path::Path;

use crate::error::StorageError;

/// Writes `contents` to `path` via a temp-file-then-rename so a crash mid
/// write can never leave `path` truncated or partially written (overall
/// design §6, harness spec §5).
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let parent = path.parent().expect("path must have a parent directory");
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, contents).map_err(|source| io_err(&tmp_path, source))?;
    fs::rename(&tmp_path, path).map_err(|source| io_err(path, source))?;
    Ok(())
}

/// Appends `line` (plus a trailing newline) to `path`, creating the file
/// and its parent directory if needed. Used for the append-only
/// `turns.jsonl` / `events.jsonl` logs.
pub fn append_line(path: &Path, line: &str) -> Result<(), StorageError> {
    use std::io::Write;

    let parent = path.parent().expect("path must have a parent directory");
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| io_err(path, source))?;
    writeln!(file, "{line}").map_err(|source| io_err(path, source))?;
    Ok(())
}

fn io_err(path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn atomic_write_survives_a_second_write() {
        let dir = std::env::temp_dir().join(format!("arbe-atomic-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("meta.json");

        write_atomic(&path, b"{\"a\":1}").unwrap();
        write_atomic(&path, b"{\"a\":2}").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "{\"a\":2}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_line_creates_file_and_appends() {
        let dir = std::env::temp_dir().join(format!("arbe-append-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("turns.jsonl");

        append_line(&path, "line1").unwrap();
        append_line(&path, "line2").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "line1\nline2\n");

        fs::remove_dir_all(&dir).ok();
    }
}

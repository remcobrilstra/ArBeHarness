use std::fs;
use std::path::Path;

use crate::error::StorageError;

/// Writes `contents` to `path` via a temp-file-then-rename so a crash mid
/// write can never leave `path` truncated or partially written (overall
/// design §6, harness spec §5).
///
/// The temp file's name includes a random suffix (not just `path`'s
/// extension swapped for `.tmp`) so two concurrent `write_atomic` calls
/// targeting the same `path` never share a temp file and race on the same
/// `fs::write` — each writer gets its own temp file and only the final
/// `rename` (atomic on all supported platforms) decides which write wins.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let parent = path.parent().expect("path must have a parent directory");
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_path = parent.join(format!("{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&tmp_path, contents).map_err(|source| io_err(&tmp_path, source))?;
    fs::rename(&tmp_path, path).map_err(|source| io_err(path, source))?;
    Ok(())
}

/// Appends `line` (plus a trailing newline) to `path`, creating the file
/// and its parent directory if needed. Used for the append-only
/// `turns.jsonl` / `events.jsonl` logs.
///
/// Writes the line and its trailing newline as a single `write_all` call
/// (one syscall) rather than two separate writes, so a crash mid-append can
/// only ever leave the file missing the whole line, never half of it —
/// which is what lets `read_jsonl`'s tolerant trailing-line handling work.
pub fn append_line(path: &Path, line: &str) -> Result<(), StorageError> {
    use std::io::Write;

    let parent = path.parent().expect("path must have a parent directory");
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| io_err(path, source))?;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    file.write_all(buf.as_bytes())
        .map_err(|source| io_err(path, source))?;
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

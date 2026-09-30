use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::StorageError;

/// Writes `contents` to `path` via a temp-file-then-rename so a crash mid
/// write can never leave `path` truncated or partially written (overall
/// design §6, harness spec §5). The temp file is flushed to disk before
/// the rename, so after a power loss `path` holds either the old or the
/// new contents, never an empty file.
///
/// The temp file's name includes a random suffix (not just `path`'s
/// extension swapped for `.tmp`) so two concurrent `write_atomic` calls
/// targeting the same `path` never share a temp file — each writer gets its
/// own and only the final `rename` (atomic on all supported platforms)
/// decides which write wins.
///
/// Replacing an existing file keeps its permissions (e.g. a script's
/// executable bit), which a fresh temp file wouldn't have.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let parent = parent_of(path)?;
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_path = parent.join(format!("{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::File::create(&tmp_path).map_err(|source| io_err(&tmp_path, source))?;
        file.write_all(contents)
            .map_err(|source| io_err(&tmp_path, source))?;
        file.sync_all()
            .map_err(|source| io_err(&tmp_path, source))?;
        drop(file);
        if let Ok(existing) = fs::metadata(path) {
            // Best effort: a file system without permissions to copy
            // (or a permission we can't set) shouldn't stop the write.
            let _ = fs::set_permissions(&tmp_path, existing.permissions());
        }
        fs::rename(&tmp_path, path).map_err(|source| io_err(path, source))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Appends `line` (plus a trailing newline) to `path`, creating the file
/// and its parent directory if needed, and flushes it to disk. Used for the
/// append-only `turns.jsonl` / `in_flight.jsonl` / `compactions.jsonl` logs.
///
/// A crash mid-append can leave a torn last line (no trailing newline).
/// Appending after it would glue the new record onto the fragment — one
/// unreadable line, which would first hide the new record and, after the
/// next append, make the whole file unreadable. So a torn tail is cut off
/// first: it was never a complete record.
pub fn append_line(path: &Path, line: &str) -> Result<(), StorageError> {
    let parent = parent_of(path)?;
    fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_err(path, source))?;
    let end = drop_torn_tail(&mut file).map_err(|source| io_err(path, source))?;
    file.seek(SeekFrom::Start(end))
        .map_err(|source| io_err(path, source))?;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    file.write_all(buf.as_bytes())
        .map_err(|source| io_err(path, source))?;
    file.sync_data().map_err(|source| io_err(path, source))?;
    Ok(())
}

/// Truncates `file` after its last newline if it doesn't end in one, and
/// returns the resulting length.
fn drop_torn_tail(file: &mut fs::File) -> std::io::Result<u64> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(0);
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(len);
    }
    // Find the last newline, reading backwards in blocks.
    const BLOCK: u64 = 8 * 1024;
    let mut end = len;
    let mut keep = 0;
    let mut buf = vec![0u8; BLOCK as usize];
    while end > 0 {
        let start = end.saturating_sub(BLOCK);
        let chunk = &mut buf[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        if let Some(i) = chunk.iter().rposition(|b| *b == b'\n') {
            keep = start + i as u64 + 1;
            break;
        }
        end = start;
    }
    tracing::warn!(
        dropped_bytes = len - keep,
        "dropping an incomplete last line left by an interrupted write"
    );
    file.set_len(keep)?;
    Ok(keep)
}

fn parent_of(path: &Path) -> Result<&Path, StorageError> {
    path.parent().ok_or_else(|| StorageError::Io {
        path: path.display().to_string(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ),
    })
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

    fn temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("arbe-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn atomic_write_survives_a_second_write() {
        let dir = temp_dir("atomic-test");
        let path = dir.join("meta.json");

        write_atomic(&path, b"{\"a\":1}").unwrap();
        write_atomic(&path, b"{\"a\":2}").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "{\"a\":2}");
        // No temp files left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_atomic_write_leaves_no_temp_file() {
        let dir = temp_dir("atomic-fail");
        // The target is a directory, so the rename fails.
        let path = dir.join("target");
        fs::create_dir_all(path.join("inner")).unwrap();
        assert!(write_atomic(&path, b"x").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("atomic-perms");
        let path = dir.join("run.sh");
        write_atomic(&path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&path, b"#!/bin/sh\necho hi\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_path_without_a_parent_is_an_error_not_a_panic() {
        assert!(write_atomic(Path::new(""), b"x").is_err());
        assert!(append_line(Path::new(""), "x").is_err());
    }

    #[test]
    fn append_line_creates_file_and_appends() {
        let dir = temp_dir("append-test");
        let path = dir.join("turns.jsonl");

        append_line(&path, "line1").unwrap();
        append_line(&path, "line2").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "line1\nline2\n");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_torn_last_line_is_dropped_before_the_next_append() {
        let dir = temp_dir("append-torn");
        let path = dir.join("turns.jsonl");
        fs::create_dir_all(&dir).unwrap();

        fs::write(&path, "{\"a\":1}\n{\"b\":").unwrap();
        append_line(&path, "{\"c\":3}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n{\"c\":3}\n");

        // Only a fragment, no complete line at all.
        fs::write(&path, "{\"partial").unwrap();
        append_line(&path, "{\"d\":4}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"d\":4}\n");

        // A long fragment spanning several read blocks.
        let long = "x".repeat(20_000);
        fs::write(&path, format!("{{\"e\":5}}\n{long}")).unwrap();
        append_line(&path, "{\"f\":6}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"e\":5}\n{\"f\":6}\n");

        fs::remove_dir_all(&dir).ok();
    }
}

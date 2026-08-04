use std::path::{Component, Path, PathBuf};

use arbe_core::ToolError;

/// Resolves `requested` against `root` and guarantees the result cannot
/// escape `root` — the sandbox boundary every filesystem-touching builtin
/// tool (`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`,
/// `grep`) goes through before touching disk. This is the single
/// highest-value function in this module: every other tool's safety
/// depends on it being correct.
///
/// - A relative `requested` path is joined onto `root`.
/// - An absolute `requested` path is used as-is (still checked against
///   `root` below) — useful when a caller already has an absolute path in
///   hand, but it must still resolve inside `root`.
/// - `.`/`..` components are resolved *lexically* (no filesystem access,
///   so this works for paths that don't exist yet, e.g. a new file being
///   written) via [`normalize_lexically`].
/// - If the normalized result does not start with the normalized `root`,
///   the request is rejected — this is what stops `../../etc/passwd`-style
///   escapes.
pub fn resolve_within_root(root: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    if requested.is_empty() {
        return Err(ToolError::Validation("path must not be empty".to_string()));
    }

    let candidate = Path::new(requested);
    let combined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };

    let normalized_root = normalize_lexically(root);
    let normalized_candidate = normalize_lexically(&combined);

    if !normalized_candidate.starts_with(&normalized_root) {
        return Err(ToolError::Validation(format!(
            "path {requested:?} resolves outside the working directory"
        )));
    }

    Ok(normalized_candidate)
}

/// Resolves `.`/`..` path components without touching the filesystem
/// (unlike [`std::fs::canonicalize`], which requires the path to exist).
/// `..` past the start of the path is simply dropped rather than erroring
/// — [`resolve_within_root`]'s `starts_with` check is what actually
/// catches an escape attempt; this function only normalizes.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Second, filesystem-aware line of defense against a symlink planted
/// inside `root` (e.g. by `execute`, or already present in a checked-out
/// repo) that points somewhere outside it. [`resolve_within_root`] is
/// purely lexical and has no way to see this — a symlink's path
/// *components* stay lexically under `root` even though following it at
/// I/O time lands outside it. Every builtin tool that does filesystem I/O
/// must call this with the path [`resolve_within_root`] returned,
/// immediately before the actual read/write/list call.
///
/// Walks up from `path` to the nearest existing ancestor (the target
/// itself may not exist yet, e.g. a file about to be created), canonicalizes
/// that ancestor (which resolves any symlinks in it), and rejects the
/// request unless the canonicalized ancestor still starts with the
/// canonicalized `root`.
pub async fn verify_no_symlink_escape(root: &Path, path: &Path) -> Result<(), ToolError> {
    let canonical_root = tokio::fs::canonicalize(root).await.map_err(|e| {
        ToolError::RuntimeFailure(format!(
            "could not canonicalize root {}: {e}",
            root.display()
        ))
    })?;

    let mut existing = path;
    loop {
        if tokio::fs::metadata(existing).await.is_ok() {
            break;
        }
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }

    let canonical_existing = tokio::fs::canonicalize(existing).await.map_err(|e| {
        ToolError::RuntimeFailure(format!(
            "could not canonicalize {}: {e}",
            existing.display()
        ))
    })?;

    if !canonical_existing.starts_with(&canonical_root) {
        return Err(ToolError::Validation(format!(
            "path {path:?} escapes the working directory via a symlink"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/workspace/repo")
    }

    #[test]
    fn joins_a_simple_relative_path() {
        let resolved = resolve_within_root(&root(), "src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/repo/src/main.rs"));
    }

    #[test]
    fn resolves_current_dir_components() {
        let resolved = resolve_within_root(&root(), "./src/./main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/repo/src/main.rs"));
    }

    #[test]
    fn resolves_internal_parent_dir_components_that_stay_inside_root() {
        let resolved = resolve_within_root(&root(), "src/../src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/repo/src/main.rs"));
    }

    #[test]
    fn rejects_a_direct_escape_attempt() {
        let err = resolve_within_root(&root(), "../../etc/passwd").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn rejects_an_escape_attempt_disguised_with_internal_components() {
        let err = resolve_within_root(&root(), "src/../../../etc/passwd").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn accepts_an_absolute_path_that_is_inside_root() {
        let resolved = resolve_within_root(&root(), "/workspace/repo/src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/repo/src/main.rs"));
    }

    #[test]
    fn rejects_an_absolute_path_that_is_outside_root() {
        let err = resolve_within_root(&root(), "/etc/passwd").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn rejects_a_sibling_directory_that_merely_shares_a_name_prefix() {
        // "/workspace/repo-evil" starts with the *string* "/workspace/repo"
        // but is not a path component of it — must still be rejected.
        let err = resolve_within_root(&root(), "/workspace/repo-evil/file").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn rejects_an_empty_path() {
        let err = resolve_within_root(&root(), "").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn the_root_itself_resolves_to_the_root() {
        let resolved = resolve_within_root(&root(), ".").unwrap();
        assert_eq!(resolved, root());
    }

    #[tokio::test]
    async fn accepts_a_path_with_no_symlinks_involved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hi").unwrap();
        let resolved = resolve_within_root(dir.path(), "f.txt").unwrap();
        verify_no_symlink_escape(dir.path(), &resolved)
            .await
            .unwrap();
    }

    // Symlink creation on Windows requires elevated privileges or developer
    // mode, so this exercises the actual escape scenario only on Unix,
    // where CI can create symlinks unprivileged.
    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlink_inside_root_that_points_outside_it() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let sandbox = tempfile::tempdir().unwrap();
        symlink(outside.path(), sandbox.path().join("escape")).unwrap();

        let resolved = resolve_within_root(sandbox.path(), "escape/secret.txt").unwrap();
        let err = verify_no_symlink_escape(sandbox.path(), &resolved)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlinked_file_inside_root_pointing_outside_it() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let sandbox = tempfile::tempdir().unwrap();
        symlink(
            outside.path().join("secret.txt"),
            sandbox.path().join("link.txt"),
        )
        .unwrap();

        let resolved = resolve_within_root(sandbox.path(), "link.txt").unwrap();
        let err = verify_no_symlink_escape(sandbox.path(), &resolved)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn allows_a_not_yet_created_file_whose_parent_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_within_root(dir.path(), "new/nested/file.txt").unwrap();
        verify_no_symlink_escape(dir.path(), &resolved)
            .await
            .unwrap();
    }
}

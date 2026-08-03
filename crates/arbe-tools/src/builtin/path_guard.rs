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
}

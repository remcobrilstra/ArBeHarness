//! Just enough git to label a session with its branch — read from the
//! repository's files, no `git` process. Works in linked worktrees, where
//! `.git` is a file pointing at the worktree's own git directory.

use std::path::{Path, PathBuf};

/// The branch checked out for `dir` (or the repository containing it).
/// `None` outside a repository, on a detached HEAD, or if anything can't
/// be read.
pub fn current_branch(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(dir)?.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

/// The git directory for `dir`: the nearest `.git` directory upwards, or
/// the directory a `.git` file (`gitdir: <path>`) points to.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return Some(ancestor.join(target));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_branch_from_a_repository_or_a_linked_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(
            current_branch(&repo.join("src")).as_deref(),
            Some("feature/x")
        );

        // A linked worktree: `.git` is a file with a (relative) gitdir.
        let worktree = root.path().join("wt");
        let wt_git = repo.join(".git/worktrees/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/fix-1\n").unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
        assert_eq!(current_branch(&worktree).as_deref(), Some("fix-1"));

        // Detached HEAD.
        std::fs::write(wt_git.join("HEAD"), "3f9a12c0ffee\n").unwrap();
        assert_eq!(current_branch(&worktree), None);
    }

    #[test]
    fn outside_a_repository_there_is_no_branch() {
        let dir = tempfile::tempdir().unwrap();
        // tempdirs don't live inside a repository.
        assert_eq!(current_branch(dir.path()), None);
    }
}

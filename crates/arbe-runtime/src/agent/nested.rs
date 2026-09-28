//! Nested instruction files (v2 plan P5.5): an `AGENTS.md`, `agent.md` or
//! `CLAUDE.md` in a subdirectory of the project applies to work in that
//! subdirectory. It's shown to the model the first time a tool touches a
//! path inside it — appended to that tool's result — rather than up front,
//! so a large monorepo's instructions only cost context where they apply.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Names checked in each directory, first match wins.
const NAMES: &[&str] = &["AGENTS.md", "agent.md", "CLAUDE.md"];
const MAX_CHARS: usize = 8_000;

/// The directories between the project root (exclusive — its instructions
/// are already in the system prompt) and `subject`'s directory
/// (inclusive), outermost first. Empty if `subject` doesn't exist or lies
/// outside the project.
pub(super) fn candidate_dirs(project_dir: &Path, subject: &str) -> Vec<PathBuf> {
    let Ok(root) = std::fs::canonicalize(project_dir) else {
        return Vec::new();
    };
    let Ok(target) = std::fs::canonicalize(root.join(subject)) else {
        return Vec::new();
    };
    if !target.starts_with(&root) {
        return Vec::new();
    }
    let mut dir = if target.is_dir() {
        target.as_path()
    } else {
        match target.parent() {
            Some(parent) => parent,
            None => return Vec::new(),
        }
    };
    let mut dirs = Vec::new();
    while dir != root && dir.starts_with(&root) {
        dirs.push(dir.to_path_buf());
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    dirs.reverse();
    dirs
}

/// Reads the instruction file in each of `dirs` (if any), labelled with
/// the directory it applies to, relative to the project root.
pub(super) fn read_instructions(project_dir: &Path, dirs: &[PathBuf]) -> Vec<String> {
    let root = std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    dirs.iter()
        .filter_map(|dir| {
            let (name, text) = NAMES
                .iter()
                .find_map(|name| std::fs::read_to_string(dir.join(name)).ok().map(|t| (*name, t)))?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            let relative = dir
                .strip_prefix(&root)
                .unwrap_or(dir)
                .to_string_lossy()
                .replace('\\', "/");
            let capped: String = text.chars().take(MAX_CHARS).collect();
            Some(format!(
                "[Instructions from {relative}/{name} — they apply to work in {relative}/]\n{capped}"
            ))
        })
        .collect()
}

/// Of `dirs`, the ones not shown yet this session — marking them shown.
pub(super) fn claim_unseen(seen: &mut HashSet<PathBuf>, dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    dirs.into_iter()
        .filter(|d| seen.insert(d.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_instructions_between_the_root_and_the_touched_path_once() {
        let root = tempfile::tempdir().unwrap();
        let api = root.path().join("services").join("api");
        std::fs::create_dir_all(api.join("src")).unwrap();
        std::fs::write(
            root.path().join("services").join("AGENTS.md"),
            "services rule",
        )
        .unwrap();
        std::fs::write(api.join("CLAUDE.md"), "api rule").unwrap();
        std::fs::write(api.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(
            root.path().join("AGENTS.md"),
            "root rule (already in the prompt)",
        )
        .unwrap();

        let dirs = candidate_dirs(root.path(), "services/api/src/main.rs");
        assert_eq!(dirs.len(), 3); // services, services/api, services/api/src
        let mut seen = HashSet::new();
        let fresh = claim_unseen(&mut seen, dirs.clone());
        let texts = read_instructions(root.path(), &fresh);
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("services/AGENTS.md") && texts[0].ends_with("services rule"));
        assert!(texts[1].contains("services/api/CLAUDE.md") && texts[1].ends_with("api rule"));
        assert!(!texts.iter().any(|t| t.contains("root rule")));

        // Second touch: nothing new.
        assert!(claim_unseen(&mut seen, dirs).is_empty());
    }

    #[test]
    fn missing_or_outside_paths_have_no_candidates() {
        let root = tempfile::tempdir().unwrap();
        assert!(candidate_dirs(root.path(), "does/not/exist.rs").is_empty());
        assert!(candidate_dirs(root.path(), "..").is_empty());
        // A command line isn't a path.
        assert!(candidate_dirs(root.path(), "cargo test").is_empty());
    }
}

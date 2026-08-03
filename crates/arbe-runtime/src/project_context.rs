use std::path::Path;

/// How much of a README to fold into the system prompt. Large enough to
/// capture a typical project summary, small enough that it can't crowd
/// out conversation history — `ContextPipeline` treats system
/// instructions as a fixed cost subtracted from the budget before history
/// ever sees it, so a huge README would otherwise shrink history for
/// every single turn.
const README_EXCERPT_CHARS: usize = 1_500;
/// Caps how many top-level directory entries get listed, for the same
/// reason.
const MAX_LISTED_ENTRIES: usize = 50;

const README_CANDIDATES: &[&str] = &["README.md", "README", "Readme.md", "readme.md"];

/// Ecosystem marker files/dirs, each paired with the human-readable label
/// folded into the description when present.
const ECOSYSTEM_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust (Cargo)"),
    ("package.json", "Node.js"),
    ("pyproject.toml", "Python (pyproject)"),
    ("requirements.txt", "Python (pip)"),
    ("go.mod", "Go"),
    (".git", "git repository"),
];

/// Builds a short, deterministic description of `root` — the directory
/// the agent is actually working in (`RuntimeConfig::project_dir`) — so a
/// question like "tell me about our current project" doesn't require the
/// model to already know something it has no way of knowing yet (there's
/// no automatic tool-call parsing from model output — see
/// `docs/v1-status.md` — so the agent can't just decide to go look).
///
/// This is deliberately a point-in-time snapshot taken once when the
/// `Agent` is constructed, not re-scanned every turn: cheap, and a stale
/// top-level listing mid-session is a minor cost compared to re-reading
/// the filesystem on every single turn.
pub fn describe_project(root: &Path) -> String {
    let mut sections = vec![format!("Working directory: {}", root.display())];

    let markers: Vec<&str> = ECOSYSTEM_MARKERS
        .iter()
        .filter(|(marker, _)| root.join(marker).exists())
        .map(|(_, label)| *label)
        .collect();
    if !markers.is_empty() {
        sections.push(format!("Detected: {}", markers.join(", ")));
    }

    if let Some(listing) = list_top_level(root) {
        sections.push(listing);
    }

    if let Some(readme) = read_readme_excerpt(root) {
        sections.push(readme);
    }

    sections.join("\n\n")
}

fn list_top_level(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    let truncated = names.len() > MAX_LISTED_ENTRIES;
    names.truncate(MAX_LISTED_ENTRIES);

    let suffix = if truncated { ", ..." } else { "" };
    Some(format!("Top-level entries: {}{suffix}", names.join(", ")))
}

fn read_readme_excerpt(root: &Path) -> Option<String> {
    for candidate in README_CANDIDATES {
        if let Ok(contents) = std::fs::read_to_string(root.join(candidate)) {
            let excerpt: String = contents.chars().take(README_EXCERPT_CHARS).collect();
            let truncated_marker = if contents.chars().count() > README_EXCERPT_CHARS {
                "\n[...truncated]"
            } else {
                ""
            };
            return Some(format!("{candidate} excerpt:\n{excerpt}{truncated_marker}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn always_includes_the_working_directory_path() {
        let dir = tempdir().unwrap();
        let description = describe_project(dir.path());
        assert!(description.contains(&dir.path().display().to_string()));
    }

    #[test]
    fn detects_a_rust_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let description = describe_project(dir.path());
        assert!(description.contains("Rust (Cargo)"));
    }

    #[test]
    fn detects_multiple_markers_at_once() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        let description = describe_project(dir.path());
        assert!(description.contains("Rust (Cargo)"));
        assert!(description.contains("git repository"));
    }

    #[test]
    fn lists_top_level_entries_sorted() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();

        let description = describe_project(dir.path());
        let listing_line = description
            .lines()
            .find(|l| l.starts_with("Top-level entries"))
            .unwrap();
        assert!(listing_line.find("a.txt").unwrap() < listing_line.find("b.txt").unwrap());
    }

    #[test]
    fn caps_the_top_level_listing_and_marks_it_truncated() {
        let dir = tempdir().unwrap();
        for i in 0..(MAX_LISTED_ENTRIES + 10) {
            std::fs::write(dir.path().join(format!("f{i:03}.txt")), "").unwrap();
        }

        let description = describe_project(dir.path());
        let listing_line = description
            .lines()
            .find(|l| l.starts_with("Top-level entries"))
            .unwrap();
        assert!(listing_line.ends_with(", ..."));
        assert_eq!(listing_line.matches(", f").count(), MAX_LISTED_ENTRIES - 1);
    }

    #[test]
    fn includes_a_readme_excerpt_when_present() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# My Project\nIt does things.",
        )
        .unwrap();

        let description = describe_project(dir.path());
        assert!(description.contains("README.md excerpt"));
        assert!(description.contains("It does things."));
    }

    #[test]
    fn truncates_a_long_readme_and_marks_it() {
        let dir = tempdir().unwrap();
        let long_readme = "x".repeat(README_EXCERPT_CHARS + 500);
        std::fs::write(dir.path().join("README.md"), &long_readme).unwrap();

        let description = describe_project(dir.path());
        assert!(description.contains("[...truncated]"));
        // The excerpt itself should be capped, not the full file.
        assert!(description.len() < long_readme.len());
    }

    #[test]
    fn no_readme_means_no_readme_section() {
        let dir = tempdir().unwrap();
        let description = describe_project(dir.path());
        assert!(!description.contains("excerpt"));
    }

    #[test]
    fn a_missing_directory_does_not_panic_and_still_names_itself() {
        let missing = std::path::PathBuf::from("/definitely/does/not/exist/anywhere");
        let description = describe_project(&missing);
        assert!(description.contains(&missing.display().to_string()));
        assert!(!description.contains("Top-level entries"));
    }
}

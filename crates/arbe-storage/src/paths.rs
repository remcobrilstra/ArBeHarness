use std::path::{Path, PathBuf};

/// Resolves the ArBeHarness root directory (`~/.arbe/`, overall design §6):
/// where the harness's *own* persistent state lives (sessions, skills,
/// memory, mcp config, logs).
///
/// Only the entry points call this (the binary, and `RuntimeConfig`'s
/// defaults); everything else is handed the resolved home explicitly
/// (`RuntimeConfig::home`, `SessionStore::with_root`, the `*_at`
/// functions), so tests and embedders never depend on the process
/// environment.
///
/// Honors `ARBE_HOME` as an override. This is a **dev/test-only** knob —
/// it relocates the harness's entire storage root, not just one session —
/// used so tests/CI never write into a real home directory. It is *not*
/// the same thing as the project/repo directory the agent operates on;
/// that's `RuntimeConfig::project_dir` (`ARBE_WORKDIR`/`--workdir`) in
/// `arbe-runtime`.
pub fn arbe_home() -> PathBuf {
    resolve_arbe_home(std::env::var("ARBE_HOME").ok(), dirs_home())
}

/// Pure resolution logic, isolated from `arbe_home()` so it's testable
/// without mutating the process-global `ARBE_HOME` env var — mutating it
/// directly in a test (as this crate's other tests are careful to avoid,
/// via `with_root`/`*_at`) would race against every other test running in
/// parallel in the same process.
fn resolve_arbe_home(override_home: Option<String>, home: PathBuf) -> PathBuf {
    match override_home {
        Some(path) => PathBuf::from(path),
        None => home.join(".arbe"),
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "arbe-storage: could not determine the user home directory — set HOME (Unix) \
                 or USERPROFILE (Windows) in the environment"
            )
        })
}

/// `<arbe_home>/instructions`.
pub fn instructions_dir_at(arbe_home: &Path) -> PathBuf {
    arbe_home.join("instructions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_arbe_home_uses_the_override_when_set() {
        let resolved = resolve_arbe_home(
            Some("/tmp/arbe-test-home".to_string()),
            PathBuf::from("/home/whoever"),
        );
        assert_eq!(resolved, PathBuf::from("/tmp/arbe-test-home"));
    }

    #[test]
    fn resolve_arbe_home_joins_dot_arbe_onto_home_without_an_override() {
        let resolved = resolve_arbe_home(None, PathBuf::from("/home/whoever"));
        assert_eq!(resolved, PathBuf::from("/home/whoever/.arbe"));
    }
}

use arbe_core::SessionId;
use std::path::PathBuf;

/// Resolves the ArBeHarness root directory (`~/.arbe/`, overall design §6):
/// where the harness's *own* persistent state lives (sessions, skills,
/// memory, mcp config, logs).
///
/// Honors `ARBE_HOME` as an override. This is a **dev/test-only** knob —
/// it relocates the harness's entire storage root, not just one session —
/// used so tests/CI never write into a real home directory. It is *not*
/// the same thing as the project/repo directory the agent operates on;
/// that's `RuntimeConfig::project_dir` (`ARBE_WORKDIR`/`--workdir`) in
/// `arbe-runtime`.
pub fn arbe_home() -> PathBuf {
    if let Ok(override_home) = std::env::var("ARBE_HOME") {
        return PathBuf::from(override_home);
    }
    dirs_home().join(".arbe")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("neither HOME nor USERPROFILE is set")
}

pub fn config_dir() -> PathBuf {
    arbe_home().join("config")
}

pub fn sessions_dir() -> PathBuf {
    arbe_home().join("sessions")
}

pub fn session_dir(session_id: SessionId) -> PathBuf {
    sessions_dir().join(session_id.to_string())
}

pub fn skills_dir() -> PathBuf {
    arbe_home().join("skills")
}

pub fn instructions_dir() -> PathBuf {
    arbe_home().join("instructions")
}

pub fn memory_dir() -> PathBuf {
    arbe_home().join("memory")
}

pub fn mcp_dir() -> PathBuf {
    arbe_home().join("mcp")
}

pub fn logs_dir() -> PathBuf {
    arbe_home().join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_arbe_home_override() {
        unsafe {
            std::env::set_var("ARBE_HOME", "/tmp/arbe-test-home");
        }
        assert_eq!(arbe_home(), PathBuf::from("/tmp/arbe-test-home"));
        assert_eq!(
            sessions_dir(),
            PathBuf::from("/tmp/arbe-test-home/sessions")
        );
        unsafe {
            std::env::remove_var("ARBE_HOME");
        }
    }
}

use std::fs;
use std::path::PathBuf;

use arbe_core::{RuntimeEvent, SessionId, SessionMeta, SessionStatus, Turn};

use crate::atomic::{append_line, write_atomic};
use crate::error::StorageError;
use crate::paths;

/// Filesystem-backed session persistence rooted at `~/.arbe/sessions/`
/// (harness spec FR-1, §5). `meta.json` is written atomically on every
/// status change; `turns.jsonl` / `events.jsonl` are append-only.
///
/// The sessions root is captured at construction (rather than re-resolved
/// globally on every call) so tests can point multiple independent stores
/// at their own temp directories without racing on process env vars.
#[derive(Debug, Clone)]
pub struct SessionStore {
    sessions_root: PathBuf,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions_root: paths::sessions_dir(),
        }
    }

    pub fn with_root(sessions_root: PathBuf) -> Self {
        Self { sessions_root }
    }

    fn session_dir(&self, id: SessionId) -> PathBuf {
        self.sessions_root.join(id.to_string())
    }

    fn meta_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("meta.json")
    }

    fn turns_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("turns.jsonl")
    }

    fn events_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("events.jsonl")
    }

    /// Creates a new session directory and persists its initial metadata.
    pub fn create_session(
        &self,
        profile: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<SessionMeta, StorageError> {
        let meta = SessionMeta::new(profile, provider, model);
        self.save_meta(&meta)?;
        Ok(meta)
    }

    pub fn save_meta(&self, meta: &SessionMeta) -> Result<(), StorageError> {
        let path = self.meta_path(meta.id);
        let json = serde_json::to_vec_pretty(meta).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })?;
        write_atomic(&path, &json)
    }

    pub fn load_meta(&self, id: SessionId) -> Result<SessionMeta, StorageError> {
        let path = self.meta_path(id);
        let bytes = fs::read(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                StorageError::SessionNotFound(id.to_string())
            } else {
                StorageError::Io {
                    path: path.display().to_string(),
                    source,
                }
            }
        })?;
        serde_json::from_slice(&bytes).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })
    }

    /// Marks a session resumable/active again. Because `meta.json` is only
    /// ever updated via atomic writes, a session left in `Active` status
    /// after a forced interruption is safe to resume as-is (FR-1: interrupted
    /// session recovery).
    pub fn resume_session(&self, id: SessionId) -> Result<SessionMeta, StorageError> {
        let mut meta = self.load_meta(id)?;
        meta.touch(SessionStatus::Active);
        self.save_meta(&meta)?;
        Ok(meta)
    }

    pub fn close_session(&self, id: SessionId) -> Result<(), StorageError> {
        let mut meta = self.load_meta(id)?;
        meta.touch(SessionStatus::Closed);
        self.save_meta(&meta)
    }

    pub fn append_turn(&self, turn: &Turn) -> Result<(), StorageError> {
        let path = self.turns_path(turn.session_id);
        let line = serde_json::to_string(turn).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })?;
        append_line(&path, &line)
    }

    pub fn list_turns(&self, id: SessionId) -> Result<Vec<Turn>, StorageError> {
        let path = self.turns_path(id);
        read_jsonl(&path)
    }

    pub fn append_event(&self, id: SessionId, event: &RuntimeEvent) -> Result<(), StorageError> {
        let path = self.events_path(id);
        let line = serde_json::to_string(event).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })?;
        append_line(&path, &line)
    }

    pub fn list_events(&self, id: SessionId) -> Result<Vec<RuntimeEvent>, StorageError> {
        let path = self.events_path(id);
        read_jsonl(&path)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>, StorageError> {
        let dir = &self.sessions_root;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in fs::read_dir(dir).map_err(|source| StorageError::Io {
            path: dir.display().to_string(),
            source,
        })? {
            let entry = entry.map_err(|source| StorageError::Io {
                path: dir.display().to_string(),
                source,
            })?;
            let meta_path = entry.path().join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let bytes = fs::read(&meta_path).map_err(|source| StorageError::Io {
                path: meta_path.display().to_string(),
                source,
            })?;
            let meta: SessionMeta =
                serde_json::from_slice(&bytes).map_err(|source| StorageError::Serde {
                    path: meta_path.display().to_string(),
                    source,
                })?;
            sessions.push(meta);
        }
        Ok(sessions)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn read_jsonl<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
) -> Result<Vec<T>, StorageError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path).map_err(|source| StorageError::Io {
        path: path.display().to_string(),
        source,
    })?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|source| StorageError::Serde {
                path: path.display().to_string(),
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::Role;

    fn temp_store() -> (SessionStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("arbe-sessions-test-{}", uuid::Uuid::new_v4()));
        (SessionStore::with_root(dir.clone()), dir)
    }

    #[test]
    fn create_load_and_resume_roundtrip() {
        let (store, dir) = temp_store();
        let created = store.create_session("default", "openai", "gpt-5").unwrap();
        assert_eq!(created.status, SessionStatus::Created);

        let loaded = store.load_meta(created.id).unwrap();
        assert_eq!(loaded.id, created.id);

        let resumed = store.resume_session(created.id).unwrap();
        assert_eq!(resumed.status, SessionStatus::Active);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_and_list_turns_survives_recovery() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "openai", "gpt-5").unwrap();

        let mut turn = Turn::new(meta.id, 0);
        turn.user_message = Some(arbe_core::Message::new(Role::User, "hello"));
        store.append_turn(&turn).unwrap();

        // Simulate a forced interruption: no explicit close happened,
        // meta is still whatever status it was left in.
        let recovered_meta = store.load_meta(meta.id).unwrap();
        let turns = store.list_turns(meta.id).unwrap();

        assert_eq!(recovered_meta.id, meta.id);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_ref().unwrap().content, "hello");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_sessions_returns_all_created_sessions() {
        let (store, dir) = temp_store();
        store.create_session("default", "openai", "gpt-5").unwrap();
        store.create_session("default", "ollama", "llama3").unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_session_is_not_found() {
        let (store, dir) = temp_store();
        let err = store.load_meta(SessionId::new()).unwrap_err();
        assert!(matches!(err, StorageError::SessionNotFound(_)));

        fs::remove_dir_all(&dir).ok();
    }
}

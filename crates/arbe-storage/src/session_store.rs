use std::fs;
use std::path::PathBuf;

use arbe_core::{
    Compaction, Message, RuntimeEvent, SessionId, SessionMeta, SessionStatus, Turn, TurnId,
};
use serde::{Deserialize, Serialize};

use crate::atomic::{append_line, write_atomic};
use crate::error::StorageError;
use crate::paths;

/// One message of an in-progress turn, as written to `in_flight.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightMessage {
    pub turn_id: TurnId,
    pub turn_index: u64,
    pub message: Message,
}

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

    fn compactions_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("compactions.jsonl")
    }

    fn in_flight_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("in_flight.jsonl")
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

    /// Write-ahead log for the turn in progress: each message is appended
    /// here as soon as it exists, so a crash mid-turn loses at most the
    /// message being produced. When the turn completes, its `Turn` record
    /// goes to `turns.jsonl` and this log is cleared
    /// ([`clear_in_flight`](Self::clear_in_flight)) — so it only ever holds
    /// one turn's messages, and leftovers on startup mean a turn was
    /// interrupted.
    pub fn append_in_flight(
        &self,
        id: SessionId,
        entry: &InFlightMessage,
    ) -> Result<(), StorageError> {
        let path = self.in_flight_path(id);
        let line = serde_json::to_string(entry).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })?;
        append_line(&path, &line)
    }

    pub fn read_in_flight(&self, id: SessionId) -> Result<Vec<InFlightMessage>, StorageError> {
        read_jsonl(&self.in_flight_path(id))
    }

    pub fn clear_in_flight(&self, id: SessionId) -> Result<(), StorageError> {
        let path = self.in_flight_path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StorageError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn append_compaction(
        &self,
        id: SessionId,
        compaction: &Compaction,
    ) -> Result<(), StorageError> {
        let path = self.compactions_path(id);
        let line = serde_json::to_string(compaction).map_err(|source| StorageError::Serde {
            path: path.display().to_string(),
            source,
        })?;
        append_line(&path, &line)
    }

    /// The most recent compaction, which supersedes all earlier ones.
    pub fn latest_compaction(&self, id: SessionId) -> Result<Option<Compaction>, StorageError> {
        Ok(read_jsonl::<Compaction>(&self.compactions_path(id))?.pop())
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

    /// Lists every session under the sessions root. A single damaged
    /// `meta.json` (unreadable or unparseable) is skipped rather than
    /// failing the entire listing — one corrupt session directory
    /// shouldn't hide every other, healthy session.
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
            let Ok(entry) = entry else { continue };
            let meta_path = entry.path().join("meta.json");
            let Ok(bytes) = fs::read(&meta_path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_slice::<SessionMeta>(&bytes) else {
                continue;
            };
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

/// Parses each non-blank line as `T`. A crash mid-`append_line` can only
/// ever tear the *last* line in the file (all earlier lines were fully
/// written and fsynced by prior calls), so only the last line is allowed to
/// fail parsing — it's dropped rather than failing the whole load. A
/// malformed line anywhere else indicates real corruption and is still a
/// hard error.
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
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();

    let mut result = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        match serde_json::from_str(line) {
            Ok(value) => result.push(value),
            Err(source) => {
                if idx == lines.len() - 1 {
                    break;
                }
                return Err(StorageError::Serde {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    Ok(result)
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

    /// A session written by v1 of the harness (plain-string message content,
    /// separate `user_message`/`assistant_message`, no `schema_version`)
    /// still loads, and a v2 turn appended after it coexists in the log.
    #[test]
    fn a_v1_turns_log_still_loads_and_accepts_v2_appends() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "ollama", "llama3").unwrap();
        let v1_line = format!(
            r#"{{"id":"6f1c7e1a-5a8a-4e53-9d7f-1f6f2d6a9b10","session_id":"{}","index":0,"user_message":{{"role":"user","content":"hello","timestamp":"2026-08-01T00:00:00Z"}},"assistant_message":{{"role":"assistant","content":"hi there","timestamp":"2026-08-01T00:00:01Z"}},"tool_calls":[],"tool_results":[],"created_at":"2026-08-01T00:00:00Z"}}"#,
            meta.id
        );
        fs::write(store.turns_path(meta.id), format!("{v1_line}\n")).unwrap();

        let mut next = Turn::new(meta.id, 1);
        next.messages
            .push(arbe_core::Message::new(Role::User, "again"));
        store.append_turn(&next).unwrap();

        let turns = store.list_turns(meta.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].schema_version, 1);
        assert_eq!(turns[0].user_message().unwrap().text(), "hello");
        assert_eq!(
            turns[0].final_assistant_message().unwrap().text(),
            "hi there"
        );
        assert_eq!(turns[1].schema_version, arbe_core::TURN_SCHEMA_VERSION);
        assert_eq!(turns[1].user_message().unwrap().text(), "again");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_latest_compaction_wins() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "ollama", "m").unwrap();
        assert!(store.latest_compaction(meta.id).unwrap().is_none());
        for (through, summary) in [(3, "first"), (7, "second")] {
            store
                .append_compaction(
                    meta.id,
                    &Compaction {
                        through_turn_index: through,
                        summary: summary.into(),
                        usage: Default::default(),
                        created_at: chrono::Utc::now(),
                    },
                )
                .unwrap();
        }
        let latest = store.latest_compaction(meta.id).unwrap().unwrap();
        assert_eq!(latest.through_turn_index, 7);
        assert_eq!(latest.summary, "second");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn in_flight_log_appends_reads_back_and_clears() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "ollama", "m").unwrap();
        assert!(store.read_in_flight(meta.id).unwrap().is_empty());
        // Clearing a log that doesn't exist is fine.
        store.clear_in_flight(meta.id).unwrap();

        let turn_id = TurnId::new();
        for text in ["question", "answer"] {
            store
                .append_in_flight(
                    meta.id,
                    &InFlightMessage {
                        turn_id,
                        turn_index: 4,
                        message: arbe_core::Message::new(Role::User, text),
                    },
                )
                .unwrap();
        }
        let entries = store.read_in_flight(meta.id).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].message.text(), "answer");
        assert_eq!(entries[0].turn_index, 4);

        store.clear_in_flight(meta.id).unwrap();
        assert!(store.read_in_flight(meta.id).unwrap().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_and_list_turns_survives_recovery() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "openai", "gpt-5").unwrap();

        let mut turn = Turn::new(meta.id, 0);
        turn.messages
            .push(arbe_core::Message::new(Role::User, "hello"));
        store.append_turn(&turn).unwrap();

        // Simulate a forced interruption: no explicit close happened,
        // meta is still whatever status it was left in.
        let recovered_meta = store.load_meta(meta.id).unwrap();
        let turns = store.list_turns(meta.id).unwrap();

        assert_eq!(recovered_meta.id, meta.id);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message().unwrap().text(), "hello");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_turns_drops_a_torn_trailing_line_but_keeps_earlier_ones() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "openai", "gpt-5").unwrap();

        let mut turn = Turn::new(meta.id, 0);
        turn.messages
            .push(arbe_core::Message::new(Role::User, "hello"));
        store.append_turn(&turn).unwrap();

        // Simulate a crash mid-append: a second, torn line with no closing
        // brace appended directly to the file (append_line always writes a
        // complete line, so this models the file state a crash mid-write
        // would leave behind).
        let path = store.turns_path(meta.id);
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"partial\": tru").unwrap();
        drop(file);

        let turns = store.list_turns(meta.id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message().unwrap().text(), "hello");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_turns_still_errors_on_a_malformed_line_that_is_not_the_last() {
        let (store, dir) = temp_store();
        let meta = store.create_session("default", "openai", "gpt-5").unwrap();
        let path = store.turns_path(meta.id);

        append_line(&path, "not valid json").unwrap();
        let mut turn = Turn::new(meta.id, 0);
        turn.messages
            .push(arbe_core::Message::new(Role::User, "hello"));
        store.append_turn(&turn).unwrap();

        let err = store.list_turns(meta.id).unwrap_err();
        assert!(matches!(err, StorageError::Serde { .. }));

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

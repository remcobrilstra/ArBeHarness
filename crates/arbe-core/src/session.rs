use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;
use crate::usage::Usage;

/// Lifecycle status of a session, per harness spec FR-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Active,
    Closed,
    Failed,
}

/// What a live session is doing right now, kept current in `meta.json`
/// so another program can show it without subscribing to events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivity {
    /// Open, waiting for the user's next message.
    Idle,
    /// A turn (or manual tool call) is in progress.
    Running,
    /// A turn is paused on a tool-approval decision.
    AwaitingApproval,
    /// A turn is paused on a question the model asked the user.
    AwaitingAnswer,
}

/// Session-level metadata persisted as `~/.arbe/sessions/<session-id>/meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    pub status: SessionStatus,
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Short human-readable label (e.g. for a session picker). `None`
    /// until something sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Provider-reported token usage summed over every turn.
    #[serde(default)]
    pub usage: Usage,
    /// Absolute path of the project directory the session works in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    /// The project's git branch when the session was last opened (`None`
    /// outside a repository or on a detached HEAD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What the session is doing, while a process has it open; `None` once
    /// closed. See [`SessionActivity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivity>,
    /// The process that has the session open; `None` once closed. If it's
    /// set but that process is gone, the process ended without closing
    /// the session (e.g. it crashed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// For a subagent's session: the session whose `task` call started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
}

impl SessionMeta {
    pub fn new(
        profile: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            status: SessionStatus::Created,
            profile: profile.into(),
            provider: provider.into(),
            model: model.into(),
            created_at: now,
            updated_at: now,
            title: None,
            usage: Usage::default(),
            workdir: None,
            branch: None,
            activity: None,
            pid: None,
            parent: None,
        }
    }

    pub fn touch(&mut self, status: SessionStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }
}

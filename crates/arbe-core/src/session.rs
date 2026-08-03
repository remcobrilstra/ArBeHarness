use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Lifecycle status of a session, per harness spec FR-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Active,
    Closed,
    Failed,
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
        }
    }

    pub fn touch(&mut self, status: SessionStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }
}

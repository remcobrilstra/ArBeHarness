use arbe_runtime::arbe_core::{Role, SessionId, ToolCallId};

/// One line in the transcript pane (TUI-FR-1: role distinction).
#[derive(Debug, Clone)]
pub struct TranscriptLine {
    pub role: Role,
    pub content: String,
}

/// A tool call awaiting a human decision (TUI-FR-2).
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: ToolCallId,
    pub tool_name: String,
    pub arguments_pretty: String,
}

/// All UI-local state. This is presentation state only (current input
/// text, scroll position, whether a request is in flight) — it holds no
/// agent decision logic, per TUI spec §2.
pub struct App {
    pub session_id: SessionId,
    pub profile: String,
    pub provider_name: String,
    pub model: String,
    /// The repo/project directory the agent is working on (distinct from
    /// `~/.arbe/`, which is the harness's own storage root).
    pub project_dir: String,
    pub transcript: Vec<TranscriptLine>,
    pub input: String,
    pub status_message: Option<String>,
    pub working: bool,
    pub pending_approval: Option<PendingApproval>,
    pub should_quit: bool,
    pub last_estimated_tokens: u64,
    pub scroll: u16,
}

impl App {
    pub fn new(
        session_id: SessionId,
        profile: String,
        provider_name: String,
        model: String,
        project_dir: String,
    ) -> Self {
        Self {
            session_id,
            profile,
            provider_name,
            model,
            project_dir,
            transcript: Vec::new(),
            input: String::new(),
            status_message: None,
            working: false,
            pending_approval: None,
            should_quit: false,
            last_estimated_tokens: 0,
            scroll: 0,
        }
    }

    pub fn push_line(&mut self, role: Role, content: String) {
        self.transcript.push(TranscriptLine { role, content });
    }

    /// Appends a streamed delta to the in-progress assistant line, starting
    /// a new one if the previous line wasn't an assistant line (TUI-FR-1:
    /// "streaming assistant output rendering token-by-token").
    pub fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(last) = self.transcript.last_mut()
            && last.role == Role::Assistant
            && self.working
        {
            last.content.push_str(delta);
            return;
        }
        self.transcript.push(TranscriptLine {
            role: Role::Assistant,
            content: delta.to_string(),
        });
    }
}

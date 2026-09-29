use arbe_runtime::arbe_core::{RiskLevel, Role, SessionId, SessionMeta, ToolCallId, TurnId};

/// One line in the transcript pane (TUI-FR-1: role distinction).
#[derive(Debug, Clone)]
pub struct TranscriptLine {
    pub role: Role,
    pub content: String,
    /// The model's reasoning rather than its answer: shown collapsed to
    /// one line unless `App::show_thinking` (Ctrl+T).
    pub thinking: bool,
}

/// A tool call awaiting a human decision (TUI-FR-2). Populated from the
/// `ToolCallProposed`/`ToolApprovalRequested` event pair rather than
/// constructed locally, so risk + source-turn always reflect what the
/// runtime actually proposed.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: ToolCallId,
    pub tool_name: String,
    pub arguments_pretty: String,
    pub risk: RiskLevel,
    pub source_turn: TurnId,
    /// Wall-clock deadline (ticks remaining, decremented once per render
    /// loop iteration) after which the approval auto-resolves to deny per
    /// TUI spec §9 ("if approval prompt times out: default action from
    /// policy, recommended deny").
    pub ticks_remaining: u32,
}

/// Metadata captured from `ToolCallProposed`, held until the matching
/// `ToolApprovalRequested` arrives (mirrors the two-event handshake the
/// runtime actually emits — see `arbe_core::RuntimeEvent`).
#[derive(Debug, Clone)]
pub struct ProposedToolCall {
    pub tool_name: String,
    pub arguments_pretty: String,
    pub risk: RiskLevel,
    pub source_turn: TurnId,
}

/// Session-picker overlay state (TUI-FR-3: resume a previous session).
#[derive(Debug, Clone)]
pub struct SessionPicker {
    pub sessions: Vec<SessionMeta>,
    pub selected: usize,
}

impl SessionPicker {
    pub fn new(mut sessions: Vec<SessionMeta>) -> Self {
        sessions.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        Self {
            sessions,
            selected: 0,
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.sessions.len() {
            self.selected += 1;
        }
    }

    pub fn selected_session(&self) -> Option<SessionId> {
        self.sessions.get(self.selected).map(|m| m.id)
    }
}

/// A question the model asked (`ask_user`), waiting for the user's answer.
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub id: ToolCallId,
    pub question: String,
    pub options: Vec<String>,
    pub allow_free_text: bool,
    /// The highlighted option.
    pub selected: usize,
}

impl PendingQuestion {
    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    /// The answer to send for what the user did: text typed in the input
    /// bar (if free text is allowed), else the highlighted option. `None`
    /// when there's nothing to send yet.
    pub fn answer(&self, typed: &str) -> Option<String> {
        let typed = typed.trim();
        if !typed.is_empty() && self.allow_free_text {
            return Some(typed.to_string());
        }
        self.options.get(self.selected).cloned()
    }
}

/// Profile-picker overlay state: switch the session to another configured
/// profile (and with it, usually, another provider/model).
#[derive(Debug, Clone)]
pub struct ProfilePicker {
    pub profiles: Vec<arbe_runtime::ProfileInfo>,
    pub selected: usize,
    /// The active profile's name, marked in the list.
    pub current: String,
}

impl ProfilePicker {
    /// Starts with the active profile selected.
    pub fn new(profiles: Vec<arbe_runtime::ProfileInfo>, current: &str) -> Self {
        let selected = profiles.iter().position(|p| p.name == current).unwrap_or(0);
        Self {
            profiles,
            selected,
            current: current.to_string(),
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.profiles.len() {
            self.selected += 1;
        }
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.profiles.get(self.selected).map(|p| p.name.as_str())
    }
}

/// How many render-loop ticks (`event::poll` iterations, ~80ms each — see
/// `lib.rs::event_loop`) a pending approval waits before auto-denying.
pub const APPROVAL_TIMEOUT_TICKS: u32 = (30_000 / 80) as u32;

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
    /// Rendered-markdown cache for every transcript entry except the last
    /// (see `ui::transcript_lines`). Only the last entry ever mutates in
    /// place (streaming deltas via `append_assistant_delta`), so every
    /// earlier entry's rendering is stable once computed — re-parsing the
    /// whole transcript as markdown on every ~80ms render tick would be
    /// wasted, unbounded-growing work for a long session.
    pub(crate) rendered_cache: Vec<ratatui::text::Line<'static>>,
    /// How many leading `transcript` entries `rendered_cache` currently
    /// reflects.
    pub(crate) rendered_cache_entry_count: usize,
    /// Sum of `content_line_count`'s per-entry line count for every
    /// transcript entry except the current last one, maintained
    /// incrementally by `push_line`/`append_assistant_delta` — same
    /// settled/last split as `rendered_cache`, so `content_line_count`
    /// (called every render tick via `max_scroll`) doesn't rescan the
    /// whole transcript's text just to count newlines.
    settled_content_lines: u32,
    /// Current input buffer; may contain embedded newlines (multi-line
    /// input, TUI spec §6 "Shift+Enter: newline").
    pub input: String,
    /// Byte offset of the cursor within `input`. Always on a char
    /// boundary.
    pub input_cursor: usize,
    /// An actual error (provider/tool failure, timeout) — rendered with an
    /// `[error]` tag. Distinct from `notice` so a success message (e.g.
    /// "resumed session ...") never gets mislabeled as a failure.
    pub status_message: Option<String>,
    /// A non-error informational message (session started/resumed, etc.).
    pub notice: Option<String>,
    pub working: bool,
    /// A short "what's happening right now" label shown next to the
    /// header's spinner while `working` — the fix for a turn that's stuck
    /// waiting on a network call otherwise looking indistinguishable from
    /// a frozen app. Updated as runtime events arrive; cleared once the
    /// final answer starts streaming (the transcript itself is the
    /// indicator from that point on).
    pub activity: Option<String>,
    /// Advances by one on every render tick (~80ms — see
    /// `lib.rs::event_loop`); used only to pick a spinner glyph, so it
    /// never needs to be reset.
    pub spinner_frame: usize,
    pub pending_approval: Option<PendingApproval>,
    pub pending_question: Option<PendingQuestion>,
    pub proposed_tool_calls: std::collections::HashMap<ToolCallId, ProposedToolCall>,
    pub session_picker: Option<SessionPicker>,
    pub profile_picker: Option<ProfilePicker>,
    pub should_quit: bool,
    pub last_estimated_tokens: u64,
    /// What the latest model call's context was made of (`/context`).
    pub context: Option<arbe_runtime::arbe_core::ContextUsage>,
    /// Provider-reported tokens used by the whole session so far.
    pub session_tokens: u64,
    /// The session's cost so far, when the model's prices are configured.
    pub session_cost_usd: Option<f64>,
    /// Top-line offset into the (unwrapped) transcript content.
    pub scroll: u16,
    /// When true, the transcript view stays pinned to the newest content
    /// (the default) — cleared as soon as the user scrolls up manually,
    /// re-armed once they scroll back down to the bottom.
    pub follow_tail: bool,
    /// Height of the transcript viewport from the most recent render,
    /// used to clamp scroll on the next key press (there is no viewport
    /// size available outside of `ui::draw`).
    pub last_viewport_height: u16,
    /// Whether thinking entries are expanded (Ctrl+T toggles).
    pub show_thinking: bool,
    /// Tool calls whose arguments are still streaming in, by the
    /// provider's call id: `(tool name, arguments so far)`. Previewed in
    /// the activity line until the call is complete.
    pub streaming_tool_args: std::collections::HashMap<String, (String, String)>,
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
            rendered_cache: Vec::new(),
            rendered_cache_entry_count: 0,
            settled_content_lines: 0,
            input: String::new(),
            input_cursor: 0,
            status_message: None,
            notice: None,
            working: false,
            activity: None,
            spinner_frame: 0,
            pending_approval: None,
            pending_question: None,
            proposed_tool_calls: std::collections::HashMap::new(),
            session_picker: None,
            profile_picker: None,
            should_quit: false,
            last_estimated_tokens: 0,
            context: None,
            session_tokens: 0,
            session_cost_usd: None,
            scroll: 0,
            follow_tail: true,
            last_viewport_height: 0,
            show_thinking: false,
            streaming_tool_args: std::collections::HashMap::new(),
        }
    }

    pub fn push_line(&mut self, role: Role, content: String) {
        self.settle_last_entry();
        self.transcript.push(TranscriptLine {
            role,
            content,
            thinking: false,
        });
    }

    /// Clears the transcript and its rendering/line-count caches together —
    /// clearing only `transcript` would leave them stale, pointing past the
    /// end of the (now-shorter) transcript.
    pub fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.rendered_cache.clear();
        self.rendered_cache_entry_count = 0;
        self.settled_content_lines = 0;
    }

    fn line_count(content: &str) -> u32 {
        content.split('\n').count().max(1) as u32
    }

    /// Display lines for one entry: a collapsed thinking entry is one line.
    fn entry_line_count(&self, entry: &TranscriptLine) -> u32 {
        if entry.thinking && !self.show_thinking {
            1
        } else {
            Self::line_count(&entry.content)
        }
    }

    /// Expands or collapses every thinking entry. Their rendering and line
    /// counts change, so the caches are rebuilt from scratch.
    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.rendered_cache.clear();
        self.rendered_cache_entry_count = 0;
        let settled = self.transcript.len().saturating_sub(1);
        self.settled_content_lines = self.transcript[..settled]
            .iter()
            .map(|e| self.entry_line_count(e))
            .sum();
    }

    /// Appends streamed reasoning to the in-progress thinking entry,
    /// starting one if needed.
    pub fn append_thinking_delta(&mut self, delta: &str) {
        if let Some(last) = self.transcript.last_mut()
            && last.thinking
            && self.working
        {
            last.content.push_str(delta);
            return;
        }
        self.settle_last_entry();
        self.transcript.push(TranscriptLine {
            role: Role::Assistant,
            content: delta.to_string(),
            thinking: true,
        });
    }

    /// Folds the current last entry's line count into `settled_content_lines`
    /// — called just before a new entry becomes the last one, since that's
    /// the moment the previous last entry stops being mutable.
    fn settle_last_entry(&mut self) {
        if let Some(prev) = self.transcript.last() {
            self.settled_content_lines += self.entry_line_count(prev);
        }
    }

    const SPINNER_FRAMES: &'static [char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

    pub fn tick_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    pub fn spinner_glyph(&self) -> char {
        Self::SPINNER_FRAMES[self.spinner_frame % Self::SPINNER_FRAMES.len()]
    }

    /// Appends a streamed delta to the in-progress assistant line, starting
    /// a new one if the previous line wasn't an assistant line (TUI-FR-1:
    /// "streaming assistant output rendering token-by-token").
    pub fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(last) = self.transcript.last_mut()
            && last.role == Role::Assistant
            && !last.thinking
            && self.working
        {
            last.content.push_str(delta);
            return;
        }
        self.settle_last_entry();
        self.transcript.push(TranscriptLine {
            role: Role::Assistant,
            content: delta.to_string(),
            thinking: false,
        });
    }

    /// Total unwrapped display lines in the transcript, including the
    /// trailing error/notice banner if present — the basis for scroll
    /// clamping.
    pub fn content_line_count(&self) -> u16 {
        let mut lines = self.settled_content_lines;
        if let Some(last) = self.transcript.last() {
            lines += self.entry_line_count(last);
        }
        if self.status_message.is_some() {
            lines += 1;
        }
        if self.notice.is_some() {
            lines += 1;
        }
        lines.min(u16::MAX as u32) as u16
    }

    pub fn max_scroll(&self) -> u16 {
        self.content_line_count()
            .saturating_sub(self.last_viewport_height)
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        let max = self.max_scroll();
        self.scroll = self.scroll.saturating_add(amount).min(max);
        if self.scroll >= max {
            self.follow_tail = true;
        }
    }

    // -- Input editing -----------------------------------------------

    pub fn input_insert_char(&mut self, c: char) {
        self.input.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
    }

    pub fn input_insert_newline(&mut self) {
        self.input_insert_char('\n');
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let prev = self.prev_char_boundary(self.input_cursor);
        self.input.replace_range(prev..self.input_cursor, "");
        self.input_cursor = prev;
    }

    pub fn input_delete_forward(&mut self) {
        if self.input_cursor >= self.input.len() {
            return;
        }
        let next = self.next_char_boundary(self.input_cursor);
        self.input.replace_range(self.input_cursor..next, "");
    }

    pub fn input_move_left(&mut self) {
        self.input_cursor = self.prev_char_boundary(self.input_cursor);
    }

    pub fn input_move_right(&mut self) {
        self.input_cursor = self.next_char_boundary(self.input_cursor);
    }

    pub fn input_move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_move_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    fn prev_char_boundary(&self, from: usize) -> usize {
        self.input[..from]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_char_boundary(&self, from: usize) -> usize {
        self.input[from..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| from + i)
            .unwrap_or(self.input.len())
    }

    pub fn take_input(&mut self) -> String {
        self.input_cursor = 0;
        std::mem::take(&mut self.input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(
            SessionId::new(),
            "default".to_string(),
            "ollama".to_string(),
            "llama3".to_string(),
            ".".to_string(),
        )
    }

    #[test]
    fn content_line_count_counts_wrapped_and_multiline_entries() {
        let mut a = app();
        a.push_line(Role::User, "one line".to_string());
        a.push_line(
            Role::Assistant,
            "line one\nline two\nline three".to_string(),
        );
        assert_eq!(a.content_line_count(), 4);

        a.status_message = Some("oops".to_string());
        assert_eq!(a.content_line_count(), 5);
    }

    #[test]
    fn thinking_streams_into_its_own_entry_and_counts_one_line_until_expanded() {
        let mut a = app();
        a.working = true;
        a.push_line(Role::User, "question".to_string());
        a.append_thinking_delta("step one\n");
        a.append_thinking_delta("step two\nstep three");
        a.append_assistant_delta("the answer");
        assert_eq!(a.transcript.len(), 3);
        assert!(a.transcript[1].thinking);
        assert_eq!(a.transcript[1].content, "step one\nstep two\nstep three");
        assert!(!a.transcript[2].thinking);
        // Collapsed: question + one thinking line + answer.
        assert_eq!(a.content_line_count(), 3);
        a.toggle_thinking();
        assert_eq!(a.content_line_count(), 5);
        a.toggle_thinking();
        assert_eq!(a.content_line_count(), 3);
    }

    #[test]
    fn follow_tail_pins_scroll_to_the_bottom_as_content_grows() {
        let mut a = app();
        a.last_viewport_height = 3;
        for i in 0..10 {
            a.push_line(Role::User, format!("line {i}"));
        }
        // draw() would normally do this recompute; exercised directly here
        // since ui::draw needs a real Frame.
        assert!(a.follow_tail);
        a.scroll = a.max_scroll();
        assert_eq!(a.scroll, a.content_line_count() - a.last_viewport_height);
    }

    #[test]
    fn scroll_up_disengages_follow_tail_and_clamps_at_zero() {
        let mut a = app();
        a.last_viewport_height = 3;
        for i in 0..10 {
            a.push_line(Role::User, format!("line {i}"));
        }
        a.follow_tail = true;
        a.scroll_up(100);
        assert!(!a.follow_tail);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn scroll_down_reengages_follow_tail_once_it_reaches_the_bottom() {
        let mut a = app();
        a.last_viewport_height = 3;
        for i in 0..10 {
            a.push_line(Role::User, format!("line {i}"));
        }
        a.scroll_up(100);
        assert!(!a.follow_tail);

        a.scroll_down(1000);
        assert!(a.follow_tail);
        assert_eq!(a.scroll, a.max_scroll());
    }

    #[test]
    fn input_editing_supports_cursor_movement_and_unicode() {
        let mut a = app();
        for c in "héllo".chars() {
            a.input_insert_char(c);
        }
        assert_eq!(a.input, "héllo");
        assert_eq!(a.input_cursor, "héllo".len());

        a.input_move_left();
        a.input_move_left();
        // cursor now between the two 'l's: "hél|lo"
        a.input_backspace();
        assert_eq!(a.input, "hélo");

        a.input_move_home();
        a.input_delete_forward();
        assert_eq!(a.input, "élo");

        a.input_move_end();
        a.input_insert_newline();
        a.input_insert_char('!');
        assert_eq!(a.input, "élo\n!");

        let taken = a.take_input();
        assert_eq!(taken, "élo\n!");
        assert_eq!(a.input, "");
        assert_eq!(a.input_cursor, 0);
    }

    #[test]
    fn a_question_is_answered_by_typed_text_or_the_highlighted_option() {
        let mut q = PendingQuestion {
            id: ToolCallId::new(),
            question: "Which?".into(),
            options: vec!["a".into(), "b".into()],
            allow_free_text: true,
            selected: 0,
        };
        assert_eq!(q.answer("  "), Some("a".into()));
        q.move_down();
        q.move_down();
        assert_eq!(q.answer(""), Some("b".into()));
        assert_eq!(q.answer(" my own "), Some("my own".into()));
        q.allow_free_text = false;
        assert_eq!(q.answer("my own"), Some("b".into()));
        let open = PendingQuestion {
            options: vec![],
            allow_free_text: true,
            ..q
        };
        assert_eq!(open.answer(""), None);
        assert_eq!(open.answer("x"), Some("x".into()));
    }

    #[test]
    fn profile_picker_starts_on_the_active_profile_and_clamps() {
        let profile = |name: &str| arbe_runtime::ProfileInfo {
            name: name.to_string(),
            ..Default::default()
        };
        let mut picker = ProfilePicker::new(
            vec![profile("coding"), profile("general"), profile("grok")],
            "general",
        );
        assert_eq!(picker.selected_name(), Some("general"));
        picker.move_down();
        picker.move_down();
        assert_eq!(picker.selected_name(), Some("grok"));
        picker.move_up();
        picker.move_up();
        picker.move_up();
        assert_eq!(picker.selected_name(), Some("coding"));
        // An unknown current profile starts at the top.
        assert_eq!(
            ProfilePicker::new(vec![profile("a")], "zzz").selected_name(),
            Some("a")
        );
    }

    #[test]
    fn session_picker_selection_clamps_at_the_list_bounds() {
        let mut picker = SessionPicker::new(vec![
            SessionMeta::new("default", "ollama", "llama3"),
            SessionMeta::new("default", "ollama", "llama3"),
        ]);
        assert_eq!(picker.selected, 0);

        picker.move_up();
        assert_eq!(picker.selected, 0);

        picker.move_down();
        assert_eq!(picker.selected, 1);
        picker.move_down();
        assert_eq!(picker.selected, 1);
    }

    #[test]
    fn session_picker_sorts_most_recently_updated_first() {
        let mut older = SessionMeta::new("default", "ollama", "llama3");
        let mut newer = SessionMeta::new("default", "ollama", "llama3");
        older.updated_at = chrono::Utc::now() - chrono::Duration::seconds(60);
        newer.updated_at = chrono::Utc::now();

        let picker = SessionPicker::new(vec![older.clone(), newer.clone()]);
        assert_eq!(picker.sessions[0].id, newer.id);
        assert_eq!(picker.sessions[1].id, older.id);
    }
}

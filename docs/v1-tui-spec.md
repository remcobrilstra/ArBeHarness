# ArBeHarness v1 — TUI Specification

## 1. Objective
Define a minimal but extensible v1 terminal interface for ArBeHarness that is clearly separated from harness internals.

Primary role of TUI in v1:
- Chat interaction
- Session visibility
- Tool approval prompts
- Basic status + diagnostics

---

## 2. Design Constraints
- TUI must not contain agent decision logic.
- TUI interacts with harness through a stable runtime API/event stream.
- TUI should degrade gracefully in low-feature terminals.
- Keep v1 intentionally simple; optimize for clarity and reliability.

---

## 3. Functional Requirements

### TUI-FR-1 Chat Interaction
- Input box for user message entry.
- Scrollable transcript area with role distinction (user/assistant/system/tool).
- Streaming assistant output rendering token-by-token.

### TUI-FR-2 Tool Approval UX
When a tool invocation is requested:
- Pause auto progression.
- Show approval dialog containing:
  - tool name
  - formatted arguments
  - risk indicator
  - source turn reference
- Actions:
  - approve once
  - deny once
  - approve for session (optional)
  - always deny this tool (session scope)

### TUI-FR-3 Session Controls
- New session
- Resume previous session
- Show session id + current provider/model
- Exit safely with state flush

### TUI-FR-4 Status + Errors
- Status line showing:
  - active provider/model
  - context token usage estimate
  - current loop phase
- Non-blocking error notifications + optional details panel toggle.

### TUI-FR-5 Config Awareness
- Display active profile name.
- Reflect runtime-config changes on next turn.
- Optional command palette in v1.1; not required for v1.

---

## 4. UI Layout (v1 baseline)

Recommended 3-region layout:
1. **Header/Status bar**
2. **Main transcript pane**
3. **Input bar + hints**

Optional modal overlays:
- tool approval modal
- error detail modal
- help shortcuts modal

---

## 5. Event Contract Between Runtime and TUI

TUI subscribes to runtime events such as:
- `SessionStarted`
- `TurnStarted`
- `ContextBuilt`
- `ModelStreamChunk`
- `ToolCallProposed`
- `ToolApprovalRequested`
- `ToolExecuted`
- `TurnCompleted`
- `RuntimeError`

TUI sends commands such as:
- `SubmitUserMessage`
- `ApproveToolCall`
- `DenyToolCall`
- `CreateSession`
- `ResumeSession`
- `TerminateSession`

This contract should be typed and versioned to avoid coupling drift.

---

## 6. Keybindings (Initial Proposal)
- `Enter`: send message
- `Shift+Enter`: newline in input
- `Ctrl+C`: graceful exit
- `Ctrl+L`: clear local screen view (no data loss)
- `PgUp/PgDn` or `Ctrl+U/Ctrl+D`: transcript navigation
- `Tab`: cycle focus when modal active

---

## 7. Rendering Rules
- Different styles for message roles:
  - User: prominent
  - Assistant: neutral
  - Tool/System: dim/annotated
- Preserve markdown as plain text in v1 (light formatting optional).
- Long outputs should wrap correctly and remain scrollable.
- Avoid blocking redraw loops during streaming.

---

## 8. Accessibility & Usability
- High-contrast default theme.
- Works at 80x24 terminal minimum (degraded but usable).
- No mouse dependency.
- Clear text labels for modal actions (no icon-only affordances).

---

## 9. Failure Handling in TUI
- If runtime disconnects/fails:
  - freeze input
  - display explicit error with retry/reconnect options
- If approval prompt times out:
  - default action from policy (recommended: deny)
  - show user what action was applied

---

## 10. v1 Acceptance Criteria
1. User can run chat session entirely from TUI with streaming responses.
2. Tool calls always pass through visible approval flow (per policy).
3. TUI can resume a previous session from persisted storage.
4. Errors are surfaced without crashing TUI.
5. UI remains responsive during model streaming and tool execution.
6. No direct dependency from core harness crate to TUI crate.

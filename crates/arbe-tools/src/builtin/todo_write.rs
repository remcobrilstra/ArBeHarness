use std::sync::Mutex;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ToolContext, ToolExecutor};

/// Caps how many todos a single call can hold, so a runaway list can't
/// flood every subsequent turn's context (this tool's own output gets fed
/// back to the model as a tool result, same budget concerns as any other
/// tool).
const MAX_TODOS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Deserialize)]
struct Args {
    todos: Vec<TodoItem>,
}

/// Lets the model track and surface progress on multi-step work — the
/// harness's own `LoopPhase` tracks *loop* state, not the model's task
/// breakdown, so this is a separate, model-driven list (docs/todo.md).
///
/// Each call replaces the entire list (not a diff/append) — the model is
/// expected to resend the full set with updated statuses, same contract as
/// the reference tool this was modeled on. State lives in-memory per
/// `Agent`/session, not persisted to disk: it's a working list for the
/// current task, not a durable record.
pub struct TodoWriteTool {
    todos: Mutex<Vec<TodoItem>>,
}

impl TodoWriteTool {
    pub fn new() -> Self {
        Self {
            todos: Mutex::new(Vec::new()),
        }
    }

    /// The current list, for callers (e.g. the TUI) that want to render it
    /// outside the tool-call/result flow.
    pub fn snapshot(&self) -> Vec<TodoItem> {
        self.todos.lock().unwrap().clone()
    }
}

impl Default for TodoWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for TodoWriteTool {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid todo_write arguments: {e}")))?;

        if args.todos.len() > MAX_TODOS {
            return Err(ToolError::Validation(format!(
                "too many todos: {} exceeds the limit of {MAX_TODOS}",
                args.todos.len()
            )));
        }
        if args.todos.iter().any(|t| t.content.trim().is_empty()) {
            return Err(ToolError::Validation(
                "todo content must not be empty".to_string(),
            ));
        }
        let in_progress_count = args
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();
        if in_progress_count > 1 {
            return Err(ToolError::Validation(format!(
                "at most one todo may be in_progress at a time, got {in_progress_count}"
            )));
        }

        let pending = args
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Pending)
            .count();
        let completed = args
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        // Serialize before moving into storage, so the list is only ever
        // copied once (as the JSON `Value` the caller needs back) instead
        // of once via `.clone()` for storage and again via `json!`'s own
        // serialization of that clone.
        let todos_json = serde_json::to_value(&args.todos)
            .map_err(|e| ToolError::RuntimeFailure(format!("failed to serialize todos: {e}")))?;
        *self.todos.lock().unwrap() = args.todos;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({
                "todos": todos_json,
                "pending": pending,
                "in_progress": in_progress_count,
                "completed": completed,
            }),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecuteWithDefaultContext;
    use arbe_core::{RiskLevel, ToolCallId, TurnId};

    fn invocation(args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: "todo_write".to_string(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[tokio::test]
    async fn writes_and_snapshots_the_full_list() {
        let tool = TodoWriteTool::new();
        tool.execute_default(invocation(json!({
            "todos": [
                { "content": "first", "status": "pending" },
                { "content": "second", "status": "in_progress" },
            ]
        })))
        .await
        .unwrap();

        let snapshot = tool.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[1].status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn a_later_call_replaces_the_whole_list_not_appends() {
        let tool = TodoWriteTool::new();
        tool.execute_default(invocation(json!({
            "todos": [{ "content": "first", "status": "pending" }]
        })))
        .await
        .unwrap();
        tool.execute_default(invocation(json!({
            "todos": [{ "content": "only", "status": "completed" }]
        })))
        .await
        .unwrap();

        let snapshot = tool.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].content, "only");
    }

    #[tokio::test]
    async fn rejects_more_than_one_in_progress_todo() {
        let tool = TodoWriteTool::new();
        let err = tool
            .execute_default(invocation(json!({
                "todos": [
                    { "content": "a", "status": "in_progress" },
                    { "content": "b", "status": "in_progress" },
                ]
            })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let tool = TodoWriteTool::new();
        let err = tool
            .execute_default(invocation(json!({
                "todos": [{ "content": "  ", "status": "pending" }]
            })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn rejects_too_many_todos() {
        let tool = TodoWriteTool::new();
        let todos: Vec<_> = (0..MAX_TODOS + 1)
            .map(|i| json!({ "content": format!("t{i}"), "status": "pending" }))
            .collect();
        let err = tool
            .execute_default(invocation(json!({ "todos": todos })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn invalid_arguments_are_a_validation_error() {
        let tool = TodoWriteTool::new();
        let err = tool
            .execute_default(invocation(json!({ "not_todos": [] })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn reports_counts_by_status() {
        let tool = TodoWriteTool::new();
        let result = tool
            .execute_default(invocation(json!({
                "todos": [
                    { "content": "a", "status": "pending" },
                    { "content": "b", "status": "pending" },
                    { "content": "c", "status": "in_progress" },
                    { "content": "d", "status": "completed" },
                ]
            })))
            .await
            .unwrap();

        assert_eq!(result.output["pending"], 2);
        assert_eq!(result.output["in_progress"], 1);
        assert_eq!(result.output["completed"], 1);
    }
}

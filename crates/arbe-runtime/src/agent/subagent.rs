//! Subagents (v2 plan P6.3): the `task` tool hands a self-contained piece
//! of work to a child agent with a fresh context, and returns its final
//! answer as the tool result.
//!
//! A child is an ordinary [`Agent`] with its own session (`meta.json`
//! records the `parent`), built from the parent's configuration, with
//! optionally fewer tools. What ties it to the parent:
//! - **Events:** every child event is re-published on the parent's bus as
//!   `RuntimeEvent::SubagentEvent`, so a UI can show (and nest) it.
//! - **Approvals:** the whole tree shares one decision mailbox and one set
//!   of session approvals, so the parent's `supply_tool_decision` answers
//!   a child's request, and "approve for session" covers children too.
//! - **Limits:** a depth cap (children at the cap get no `task` tool) and
//!   a concurrency cap shared by the tree.
//! - **Cancellation:** cancelling the parent's turn cancels the child's.

use std::sync::Arc;
use std::time::Duration;

use arbe_core::{
    HarnessError, RiskLevel, RuntimeEvent, SessionId, ToolCallId, ToolError, ToolInvocation,
    ToolResult,
};
use arbe_providers::ProviderRegistry;
use arbe_storage::SessionStore;
use arbe_tools::{SessionApprovals, ToolContext, ToolDescription, ToolExecutor, schemars};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Semaphore;

use super::Agent;
use super::approvals::ToolDecisions;
use crate::EventBus;
use crate::config::{RuntimeConfig, tool_allowed};

pub(super) const TASK_TOOL: &str = "task";

/// Where an agent sits in a tree of subagents, and what the tree shares.
#[derive(Clone)]
pub(super) struct Lineage {
    /// 0 for a top-level agent.
    pub depth: u32,
    pub decisions: Arc<ToolDecisions>,
    pub questions: Arc<super::ask::QuestionMailbox>,
    pub session_approvals: SessionApprovals,
    /// Permits for subagents running at once.
    pub slots: Arc<Semaphore>,
    /// The session that started this one.
    pub parent: Option<SessionId>,
    /// The tree's mode: a subagent works under its parent's.
    pub mode: Arc<super::modes::ModeState>,
    /// Set when any agent in the tree runs a tool that can change things;
    /// the top-level turn's `before_turn_end` check reads and clears it.
    pub changed: Arc<std::sync::atomic::AtomicBool>,
}

impl Lineage {
    /// A top-level agent: the root of its own tree.
    pub(super) fn root(config: &RuntimeConfig) -> Self {
        Self {
            depth: 0,
            decisions: Arc::default(),
            questions: Arc::default(),
            session_approvals: SessionApprovals::new(),
            slots: Arc::new(Semaphore::new(config.subagent_max_concurrent)),
            parent: None,
            mode: Arc::new(super::modes::ModeState::new(config.mode.as_deref())),
            changed: Arc::default(),
        }
    }

    fn child_of(&self, parent: SessionId) -> Self {
        Self {
            depth: self.depth + 1,
            parent: Some(parent),
            ..self.clone()
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    /// A short (3-6 word) label for the task, shown to the user.
    description: String,
    /// Complete instructions for the subagent. It knows nothing of this
    /// conversation, so include every detail it needs (file paths, names,
    /// constraints) and say exactly what it should report back.
    prompt: String,
    /// Optional: only these tools for the subagent, e.g. `["read_file",
    /// "grep", "glob", "list_dir"]` for read-only research. Default: the
    /// same tools you have.
    #[serde(default)]
    tools: Option<Vec<String>>,
}

pub(super) struct TaskTool {
    /// The parent's configuration: what a child is built from.
    config: RuntimeConfig,
    providers: ProviderRegistry,
    store: SessionStore,
    parent_events: Arc<EventBus>,
    /// What each child gets.
    child_lineage: Lineage,
}

impl TaskTool {
    pub(super) fn new(
        config: &RuntimeConfig,
        providers: &ProviderRegistry,
        store: SessionStore,
        parent_events: Arc<EventBus>,
        parent_lineage: &Lineage,
        parent_session: SessionId,
    ) -> Self {
        let mut config = config.clone();
        // Each child would start its own copy of every MCP server; until
        // children can share the parent's connections, they go without.
        config.mcp_servers.clear();
        Self {
            config,
            providers: providers.clone(),
            store,
            parent_events,
            child_lineage: parent_lineage.child_of(parent_session),
        }
    }

    /// The child's configuration: the parent's, with the requested tools
    /// (never more than the parent may use).
    fn child_config(&self, requested: Option<Vec<String>>) -> RuntimeConfig {
        let mut config = self.config.clone();
        if let Some(requested) = requested {
            let allowed: Vec<String> = match &config.tools {
                Some(parent) => requested
                    .into_iter()
                    .filter(|t| tool_allowed(parent, t))
                    .collect(),
                None => requested,
            };
            config.tools = Some(allowed);
        }
        config
    }
}

#[async_trait]
impl ToolExecutor for TaskTool {
    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid task arguments: {e}")))?;
        if args.prompt.trim().is_empty() {
            return Err(ToolError::Validation("prompt is empty".into()));
        }

        let _permit = tokio::select! {
            permit = self.child_lineage.slots.clone().acquire_owned() => permit
                .map_err(|_| ToolError::RuntimeFailure("subagents are shutting down".into()))?,
            _ = ctx.cancel.cancelled() => {
                return Err(ToolError::RuntimeFailure("cancelled before the subagent started".into()));
            }
        };

        let events = Arc::new(EventBus::new(4_096));
        let child_rx = events.subscribe();
        let child = Agent::create_child(
            &self.child_config(args.tools),
            self.store.clone(),
            events.clone(),
            &self.providers,
            self.child_lineage.clone(),
        )
        .map_err(|e| ToolError::RuntimeFailure(format!("could not start the subagent: {e}")))?;
        if let Err(err) = child.set_title(args.description.trim()) {
            tracing::warn!(%err, "failed to name the subagent session");
        }
        let forwarder = tokio::spawn(forward(
            child_rx,
            self.parent_events.clone(),
            invocation.id,
            child.session_id(),
        ));

        let result = {
            let turn = child.submit_message(args.prompt);
            tokio::pin!(turn);
            tokio::select! {
                biased;
                result = &mut turn => result,
                _ = ctx.cancel.cancelled() => {
                    child.cancel_turn();
                    turn.await
                }
            }
        };
        if let Err(err) = child.close() {
            tracing::warn!(%err, "failed to close the subagent session");
        }
        // With the child and its bus gone, the forwarder drains what's left
        // and ends, so the parent sees every child event before the result.
        drop(child);
        drop(events);
        if tokio::time::timeout(Duration::from_secs(5), forwarder)
            .await
            .is_err()
        {
            tracing::warn!("subagent event forwarding did not finish");
        }

        match result {
            Ok(answer) if answer.trim().is_empty() => Ok(ToolResult {
                id: invocation.id,
                output: Value::String(
                    "(the subagent finished without writing a final answer)".into(),
                ),
                is_error: false,
                attachments: Vec::new(),
            }),
            Ok(answer) => Ok(ToolResult {
                id: invocation.id,
                output: Value::String(answer),
                is_error: false,
                attachments: Vec::new(),
            }),
            Err(HarnessError::Cancelled) => Err(ToolError::RuntimeFailure(
                "the subagent was cancelled".into(),
            )),
            Err(err) => Err(ToolError::RuntimeFailure(format!(
                "the subagent failed: {err}"
            ))),
        }
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Hand a self-contained task to a subagent: a separate agent with a fresh context and the same tools (or fewer), which works on it and returns its final answer as this tool's result. Use it for work that needs many steps but whose details you don't need afterwards — researching a codebase question, surveying many files, trying something out — so they don't fill your own context. Several calls in one turn run in parallel. The subagent can't see this conversation or ask you questions: its prompt must say everything.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        // The subagent's own tool calls each go through approval.
        RiskLevel::Low
    }
}

/// Re-publishes a child's events on the parent's bus until the child's bus
/// closes.
async fn forward(
    mut child: tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>,
    parent: Arc<EventBus>,
    parent_tool_call_id: ToolCallId,
    session_id: SessionId,
) {
    loop {
        match child.recv().await {
            Ok(envelope) => parent.publish(RuntimeEvent::SubagentEvent {
                parent_tool_call_id,
                session_id,
                event: Box::new(envelope.event),
            }),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "dropped subagent events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_tool(parent_tools: Option<Vec<&str>>) -> (TaskTool, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("arbe-subagent-{}", uuid::Uuid::new_v4()));
        let config = RuntimeConfig {
            tools: parent_tools.map(|t| t.into_iter().map(str::to_string).collect()),
            ..RuntimeConfig::defaults(dir.clone())
        };
        let tool = TaskTool::new(
            &config,
            &ProviderRegistry::with_builtins(),
            SessionStore::with_root(dir.join("sessions")),
            Arc::new(EventBus::default()),
            &Lineage::root(&config),
            SessionId::new(),
        );
        (tool, dir)
    }

    fn requested(names: &[&str]) -> Option<Vec<String>> {
        Some(names.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn a_subagent_never_gets_more_tools_than_its_parent() {
        let (tool, dir) = task_tool(Some(vec!["read_file", "grep", "gh__*"]));
        // Asking for more than the parent has: only the overlap is kept.
        let child = tool.child_config(requested(&["read_file", "execute", "write_file"]));
        assert_eq!(child.tools, requested(&["read_file"]));
        // A pattern the parent's allow-set covers is kept; a wider one isn't.
        let child = tool.child_config(requested(&["gh__issues", "gh__*", "g*", "*"]));
        assert_eq!(child.tools, requested(&["gh__issues", "gh__*"]));
        // Asking for nothing in particular: the parent's own set.
        assert_eq!(
            tool.child_config(None).tools,
            requested(&["read_file", "grep", "gh__*"])
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_parent_with_every_tool_can_narrow_its_subagent() {
        let (tool, dir) = task_tool(None);
        let child = tool.child_config(requested(&["read_file", "glob"]));
        assert_eq!(child.tools, requested(&["read_file", "glob"]));
        assert_eq!(tool.child_config(None).tools, None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subagents_start_without_the_parents_mcp_servers() {
        let dir = std::env::temp_dir().join(format!("arbe-subagent-{}", uuid::Uuid::new_v4()));
        let config = RuntimeConfig {
            mcp_servers: vec![arbe_mcp::McpServerConfig {
                name: "docs".into(),
                transport: arbe_mcp::TransportConfig::Http {
                    url: "http://localhost:1".into(),
                    headers: Default::default(),
                    bearer_token: None,
                },
                timeout: Duration::from_secs(1),
            }],
            ..RuntimeConfig::defaults(dir.clone())
        };
        let tool = TaskTool::new(
            &config,
            &ProviderRegistry::with_builtins(),
            SessionStore::with_root(dir.join("sessions")),
            Arc::new(EventBus::default()),
            &Lineage::root(&config),
            SessionId::new(),
        );
        assert!(tool.child_config(None).mcp_servers.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}

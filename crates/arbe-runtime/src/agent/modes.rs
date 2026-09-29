//! Session modes (v2 plan P6.6): a mode decides which tools the model may
//! use and adds its own instructions to the prompt. `default` changes
//! nothing; `plan` is read-only until the user approves a plan. Modes are
//! data ([`ModeSpec`]), so new ones are a list entry, not new code paths.
//!
//! A mode is an overlay on the approval gate, not a replacement: a tool the
//! mode allows still needs whatever approval it always needs, and a tool it
//! doesn't allow is refused before anyone is asked. Subagents share their
//! parent's mode (through `Lineage`), so a subagent started in plan mode is
//! read-only too.

use std::sync::{Arc, Mutex};

use arbe_core::{RiskLevel, RuntimeEvent, SessionId, ToolError, ToolInvocation, ToolResult};
use arbe_tools::{ToolContext, ToolDescription, ToolExecutor, schemars};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::EventBus;

pub const DEFAULT_MODE: &str = "default";
pub const PLAN_MODE: &str = "plan";
pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";

/// Which tools a mode offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAccess {
    /// Every tool the session has.
    All,
    /// Only tools that report [`ToolExecutor::read_only`], plus these.
    ReadOnly { also: Vec<String> },
}

/// How the model leaves a mode: by calling `tool`, which the user must
/// approve; approval switches the session to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeExit {
    pub tool: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSpec {
    pub name: String,
    /// One line for mode pickers and `/mode`.
    pub description: String,
    /// Added to the system prompt while the mode is on.
    pub instructions: Option<String>,
    pub tools: ToolAccess,
    pub exit: Option<ModeExit>,
}

impl ModeSpec {
    /// Whether a call to `name` is allowed in this mode (the mode's own
    /// exit tool always is).
    pub fn permits(&self, name: &str, executor: &dyn ToolExecutor) -> bool {
        if self.exit.as_ref().is_some_and(|exit| exit.tool == name) {
            return true;
        }
        match &self.tools {
            ToolAccess::All => true,
            ToolAccess::ReadOnly { also } => {
                executor.read_only() || also.iter().any(|allowed| allowed == name)
            }
        }
    }
}

const PLAN_INSTRUCTIONS: &str = "\
# Plan mode

You are in plan mode: the user wants a plan they can approve before anything is changed.

- Investigate first: read files, search the code, and use ask_user for anything that is unclear or that the user should decide. Only read-only tools are available; editing files, running commands and other changes are blocked until the plan is approved.
- When your plan is complete, call exit_plan_mode with the plan in Markdown: the goal, the steps in order with the files each one touches, and how the result will be verified. Keep it concrete and as short as the task allows.
- If the user approves, plan mode ends and you carry out the plan in the same conversation. If they don't, stay in plan mode: ask what should change, revise the plan, and present it again.
- If no user is available to answer (ask_user says so), don't keep resubmitting: end your turn with the plan as your answer.
- Don't try to make changes another way, and don't end your turn with a plan without calling exit_plan_mode.";

/// The modes every session has.
pub fn builtin_modes() -> Vec<ModeSpec> {
    vec![
        ModeSpec {
            name: DEFAULT_MODE.to_string(),
            description: "every tool, with your approval settings".to_string(),
            instructions: None,
            tools: ToolAccess::All,
            exit: None,
        },
        ModeSpec {
            name: PLAN_MODE.to_string(),
            description: "read-only research, then a plan for you to approve".to_string(),
            instructions: Some(PLAN_INSTRUCTIONS.to_string()),
            tools: ToolAccess::ReadOnly { also: Vec::new() },
            exit: Some(ModeExit {
                tool: EXIT_PLAN_MODE_TOOL.to_string(),
                to: DEFAULT_MODE.to_string(),
            }),
        },
    ]
}

/// Whether `name` is a known mode (for validating configuration).
pub fn is_known_mode(name: &str) -> bool {
    builtin_modes().iter().any(|mode| mode.name == name)
}

/// A mode as shown to users.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeInfo {
    pub name: String,
    pub description: String,
}

/// The mode a tree of agents is in: shared by a top-level agent and its
/// subagents.
#[derive(Debug)]
pub struct ModeState {
    modes: Vec<ModeSpec>,
    current: Mutex<usize>,
}

impl ModeState {
    /// Starts in `initial`, or in `default` if there's no such mode.
    pub fn new(initial: Option<&str>) -> Self {
        let modes = builtin_modes();
        let current = initial
            .and_then(|name| modes.iter().position(|m| m.name == name))
            .unwrap_or(0);
        Self {
            modes,
            current: Mutex::new(current),
        }
    }

    fn index(&self) -> usize {
        *self.current.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn current(&self) -> ModeSpec {
        self.modes[self.index()].clone()
    }

    pub fn name(&self) -> String {
        self.modes[self.index()].name.clone()
    }

    /// Switches to `name`. Returns whether the mode actually changed.
    pub fn set(&self, name: &str) -> Result<bool, String> {
        let Some(index) = self.modes.iter().position(|m| m.name == name) else {
            let known: Vec<&str> = self.modes.iter().map(|m| m.name.as_str()).collect();
            return Err(format!(
                "unknown mode {name:?}; available: {}",
                known.join(", ")
            ));
        };
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let changed = *current != index;
        *current = index;
        Ok(changed)
    }

    pub fn infos(&self) -> Vec<ModeInfo> {
        self.modes
            .iter()
            .map(|m| ModeInfo {
                name: m.name.clone(),
                description: m.description.clone(),
            })
            .collect()
    }

    /// Whether `name` is the tool some mode uses to exit.
    pub fn is_exit_tool(&self, name: &str) -> bool {
        self.modes
            .iter()
            .any(|m| m.exit.as_ref().is_some_and(|exit| exit.tool == name))
    }

    /// What the model is told when it calls a tool the mode doesn't allow.
    pub fn refusal(&self, name: &str) -> String {
        let mode = self.current();
        let mut text = format!(
            "`{name}` isn't available in {} mode ({}).",
            mode.name, mode.description
        );
        if let Some(exit) = &mode.exit {
            text.push_str(&format!(
                " Keep to the tools you have, and call {} when you're ready for the user's approval.",
                exit.tool
            ));
        }
        text
    }

    /// What the model is told when the user doesn't approve its request to
    /// leave the mode.
    pub fn exit_declined(&self) -> String {
        let mode = self.current().name;
        format!(
            "The user did not approve. You are still in {mode} mode: find out what they want changed (ask_user if it isn't clear), revise, and ask again."
        )
    }

    /// Whether the model is offered, and may call, `name` right now. Exit
    /// tools belong to the top-level agent: a subagent can't end its
    /// parent's mode.
    pub fn allows(&self, name: &str, executor: &dyn ToolExecutor, top_level: bool) -> bool {
        if self.is_exit_tool(name) && !top_level {
            return false;
        }
        let mode = self.current();
        if self.is_exit_tool(name) {
            return mode.exit.as_ref().is_some_and(|exit| exit.tool == name);
        }
        mode.permits(name, executor)
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExitArgs {
    /// The complete plan, in Markdown: the goal, the steps in order with
    /// the files each touches, and how the result will be verified. The
    /// user reads it and approves it or not.
    plan: String,
}

/// A mode's exit tool (`exit_plan_mode`): the call carries the plan, the
/// approval prompt is the user's review of it, and running it — only ever
/// after approval — switches the session out of the mode.
pub struct ExitModeTool {
    state: Arc<ModeState>,
    from: String,
    to: String,
    events: Arc<EventBus>,
    session_id: SessionId,
}

impl ExitModeTool {
    pub fn new(
        state: Arc<ModeState>,
        from: &ModeSpec,
        events: Arc<EventBus>,
        session_id: SessionId,
    ) -> Option<Self> {
        let exit = from.exit.as_ref()?;
        Some(Self {
            state,
            from: from.name.clone(),
            to: exit.to.clone(),
            events,
            session_id,
        })
    }
}

#[async_trait]
impl ToolExecutor for ExitModeTool {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: ExitArgs = serde_json::from_value(invocation.arguments.clone())
            .map_err(|e| ToolError::Validation(e.to_string()))?;
        if args.plan.trim().is_empty() {
            return Err(ToolError::Validation(
                "`plan` is empty: include the whole plan".to_string(),
            ));
        }
        if self.state.name() != self.from {
            return Err(ToolError::RuntimeFailure(format!(
                "not in {} mode",
                self.from
            )));
        }
        self.state
            .set(&self.to)
            .map_err(ToolError::RuntimeFailure)?;
        self.events.publish(RuntimeEvent::ModeChanged {
            session_id: self.session_id,
            mode: self.to.clone(),
        });
        Ok(ToolResult {
            id: invocation.id,
            output: serde_json::Value::String(format!(
                "The user approved the plan. {} mode is over: you are in {} mode now and every tool is available. Carry out the plan.",
                self.from, self.to
            )),
            is_error: false,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<ExitArgs>(format!(
            "Present your finished plan to the user and ask to leave {} mode. The user reviews the plan: if they approve, {} mode ends and you carry it out; if not, you stay in {} mode.",
            self.from, self.from, self.from
        ))
    }

    /// High: it always asks, even where lower-risk calls are approved
    /// automatically — approving the plan is the point.
    fn default_risk(&self) -> RiskLevel {
        RiskLevel::High
    }

    fn parallel_safe(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        read_only: bool,
    }

    #[async_trait]
    impl ToolExecutor for Fake {
        async fn execute(
            &self,
            invocation: ToolInvocation,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: serde_json::Value::Null,
                is_error: false,
                attachments: Vec::new(),
            })
        }

        fn read_only(&self) -> bool {
            self.read_only
        }
    }

    const READER: Fake = Fake { read_only: true };
    const WRITER: Fake = Fake { read_only: false };

    #[test]
    fn plan_mode_allows_only_read_only_tools_and_its_exit() {
        let state = ModeState::new(Some(PLAN_MODE));
        assert_eq!(state.name(), PLAN_MODE);
        assert!(state.allows("read_file", &READER, true));
        assert!(!state.allows("write_file", &WRITER, true));
        assert!(state.allows(EXIT_PLAN_MODE_TOOL, &WRITER, true));
        // A subagent can read, but can't end its parent's plan mode.
        assert!(state.allows("read_file", &READER, false));
        assert!(!state.allows(EXIT_PLAN_MODE_TOOL, &WRITER, false));
    }

    #[test]
    fn default_mode_allows_everything_but_exit_tools() {
        let state = ModeState::new(None);
        assert_eq!(state.name(), DEFAULT_MODE);
        assert!(state.allows("write_file", &WRITER, true));
        assert!(!state.allows(EXIT_PLAN_MODE_TOOL, &WRITER, true));
    }

    #[test]
    fn switching_reports_changes_and_rejects_unknown_modes() {
        let state = ModeState::new(Some("nonsense"));
        assert_eq!(state.name(), DEFAULT_MODE);
        assert_eq!(state.set(PLAN_MODE), Ok(true));
        assert_eq!(state.set(PLAN_MODE), Ok(false));
        let err = state.set("yolo").unwrap_err();
        assert!(err.contains("default, plan"), "{err}");
        assert!(is_known_mode("plan") && !is_known_mode("yolo"));
        assert!(state.refusal("write_file").contains("call exit_plan_mode"));
        assert!(state.exit_declined().contains("still in plan mode"));
        assert_eq!(state.infos().len(), 2);
    }

    #[tokio::test]
    async fn the_exit_tool_ends_the_mode_and_says_so() {
        let state = Arc::new(ModeState::new(Some(PLAN_MODE)));
        let events = Arc::new(EventBus::default());
        let mut rx = events.subscribe();
        let tool =
            ExitModeTool::new(state.clone(), &state.current(), events, SessionId::new()).unwrap();
        let call = |plan: &str| ToolInvocation {
            id: arbe_core::ToolCallId::new(),
            source_turn: arbe_core::TurnId::new(),
            tool_name: EXIT_PLAN_MODE_TOOL.into(),
            arguments: serde_json::json!({ "plan": plan }),
            risk: RiskLevel::High,
            rationale: None,
        };
        assert!(
            tool.execute(call("  "), &ToolContext::for_testing())
                .await
                .is_err()
        );
        assert_eq!(state.name(), PLAN_MODE);

        let result = tool
            .execute(call("1. do it"), &ToolContext::for_testing())
            .await
            .unwrap();
        assert!(result.output.as_str().unwrap().contains("approved"));
        assert_eq!(state.name(), DEFAULT_MODE);
        assert!(matches!(
            rx.try_recv().unwrap().event,
            RuntimeEvent::ModeChanged { mode, .. } if mode == DEFAULT_MODE
        ));
        // Only from plan mode.
        assert!(
            tool.execute(call("again"), &ToolContext::for_testing())
                .await
                .is_err()
        );
        assert!(
            ExitModeTool::new(
                state.clone(),
                &builtin_modes()[0],
                Arc::new(EventBus::default()),
                SessionId::new()
            )
            .is_none()
        );
    }
}

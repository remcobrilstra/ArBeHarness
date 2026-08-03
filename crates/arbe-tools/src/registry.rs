use std::collections::HashMap;
use std::sync::Arc;

use arbe_core::ToolError;

use crate::ToolExecutor;

/// Maps tool names to their executors (harness spec FR-4: "tool registry
/// with typed signatures"). MCP-provided tools are exposed through this
/// same registry (overall design §4.4), so callers never need to know
/// whether a tool is local or came from an MCP server.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    executors: HashMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool_name: impl Into<String>, executor: Arc<dyn ToolExecutor>) {
        self.executors.insert(tool_name.into(), executor);
    }

    pub fn get(&self, tool_name: &str) -> Result<&Arc<dyn ToolExecutor>, ToolError> {
        self.executors
            .get(tool_name)
            .ok_or_else(|| ToolError::Validation(format!("no tool registered as {tool_name:?}")))
    }

    pub fn contains(&self, tool_name: &str) -> bool {
        self.executors.contains_key(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{RiskLevel, ToolCallId, ToolInvocation, ToolResult, TurnId};
    use async_trait::async_trait;
    use serde_json::json;

    struct EchoExecutor;

    #[async_trait]
    impl ToolExecutor for EchoExecutor {
        async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: invocation.arguments,
                is_error: false,
            })
        }
    }

    fn invocation(tool_name: &str) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: tool_name.to_string(),
            arguments: json!({"a": 1}),
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[test]
    fn missing_tool_is_a_validation_error() {
        let registry = ToolRegistry::new();
        let Err(err) = registry.get("nope") else {
            panic!("expected an error");
        };
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn registered_tool_can_be_looked_up_and_executed() {
        let mut registry = ToolRegistry::new();
        registry.register("echo", Arc::new(EchoExecutor));

        assert!(registry.contains("echo"));
        let executor = registry.get("echo").unwrap();
        let result = executor.execute(invocation("echo")).await.unwrap();
        assert_eq!(result.output, json!({"a": 1}));
    }
}

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::{Hook, HookPhase};

/// Runs every hook registered for a phase, isolating each one so a single
/// bad hook can't take down the turn (harness spec FR-7: "Hook failures
/// isolated from core flow unless configured strict"). Isolation covers
/// both:
/// - **timeout**: a hook that hangs past `timeout` is cut off and skipped.
/// - **panics**: each hook runs inside its own `tokio::spawn`, so a panic
///   inside a hook surfaces as a `JoinError` here rather than unwinding
///   through the caller.
///
/// A failed/timed-out/panicking hook is skipped — the payload it would
/// have received passes through unchanged to the next hook — rather than
/// aborting the rest of the phase.
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
    timeout: Duration,
}

impl HookRegistry {
    pub fn new(timeout: Duration) -> Self {
        Self {
            hooks: Vec::new(),
            timeout,
        }
    }

    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    /// Runs every hook registered for `phase`, in registration order,
    /// threading the (possibly transformed) payload through the chain.
    pub async fn run_phase(&self, phase: HookPhase, payload: Value) -> Value {
        let mut current = payload;
        for hook in self.hooks.iter().filter(|h| h.phase() == phase) {
            let hook = hook.clone();
            let input = current.clone();
            let outcome = tokio::time::timeout(
                self.timeout,
                tokio::spawn(async move { hook.run(input).await }),
            )
            .await;

            current = match outcome {
                Ok(Ok(Ok(transformed))) => transformed,
                Ok(Ok(Err(err))) => {
                    tracing::warn!(?err, "hook returned an error; skipping its output");
                    current
                }
                Ok(Err(join_err)) => {
                    tracing::warn!(%join_err, "hook panicked; skipping its output");
                    current
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        timeout_ms = self.timeout.as_millis(),
                        "hook timed out; skipping its output"
                    );
                    current
                }
            };
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::HookError;
    use async_trait::async_trait;
    use serde_json::json;

    struct AppendHook(HookPhase, &'static str);

    #[async_trait]
    impl Hook for AppendHook {
        fn phase(&self) -> HookPhase {
            self.0
        }
        async fn run(&self, payload: Value) -> Result<Value, HookError> {
            let mut s = payload.as_str().unwrap_or_default().to_string();
            s.push_str(self.1);
            Ok(Value::String(s))
        }
    }

    struct FailingHook(HookPhase);

    #[async_trait]
    impl Hook for FailingHook {
        fn phase(&self) -> HookPhase {
            self.0
        }
        async fn run(&self, _payload: Value) -> Result<Value, HookError> {
            Err(HookError::ContractViolation("nope".to_string()))
        }
    }

    struct PanickingHook(HookPhase);

    #[async_trait]
    impl Hook for PanickingHook {
        fn phase(&self) -> HookPhase {
            self.0
        }
        async fn run(&self, _payload: Value) -> Result<Value, HookError> {
            panic!("boom");
        }
    }

    struct SlowHook(HookPhase);

    #[async_trait]
    impl Hook for SlowHook {
        fn phase(&self) -> HookPhase {
            self.0
        }
        async fn run(&self, payload: Value) -> Result<Value, HookError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(payload)
        }
    }

    #[tokio::test]
    async fn hooks_for_other_phases_are_not_run() {
        let mut registry = HookRegistry::new(Duration::from_millis(500));
        registry.register(Arc::new(AppendHook(HookPhase::BeforeModelCall, "-x")));

        let result = registry
            .run_phase(HookPhase::AfterModelCall, json!("start"))
            .await;
        assert_eq!(result, json!("start"));
    }

    #[tokio::test]
    async fn hooks_chain_and_transform_the_payload_in_order() {
        let mut registry = HookRegistry::new(Duration::from_millis(500));
        registry.register(Arc::new(AppendHook(HookPhase::OnTurnComplete, "-a")));
        registry.register(Arc::new(AppendHook(HookPhase::OnTurnComplete, "-b")));

        let result = registry
            .run_phase(HookPhase::OnTurnComplete, json!("start"))
            .await;
        assert_eq!(result, json!("start-a-b"));
    }

    #[tokio::test]
    async fn a_failing_hook_is_isolated_and_does_not_affect_the_chain() {
        let mut registry = HookRegistry::new(Duration::from_millis(500));
        registry.register(Arc::new(FailingHook(HookPhase::OnError)));
        registry.register(Arc::new(AppendHook(HookPhase::OnError, "-after")));

        let result = registry.run_phase(HookPhase::OnError, json!("start")).await;
        assert_eq!(result, json!("start-after"));
    }

    #[tokio::test]
    async fn a_panicking_hook_is_isolated_and_does_not_affect_the_chain() {
        let mut registry = HookRegistry::new(Duration::from_millis(500));
        registry.register(Arc::new(PanickingHook(HookPhase::OnError)));
        registry.register(Arc::new(AppendHook(HookPhase::OnError, "-after")));

        let result = registry.run_phase(HookPhase::OnError, json!("start")).await;
        assert_eq!(result, json!("start-after"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_hook_is_isolated_and_does_not_affect_the_chain() {
        let mut registry = HookRegistry::new(Duration::from_millis(50));
        registry.register(Arc::new(SlowHook(HookPhase::OnError)));
        registry.register(Arc::new(AppendHook(HookPhase::OnError, "-after")));

        let result = registry.run_phase(HookPhase::OnError, json!("start")).await;
        assert_eq!(result, json!("start-after"));
    }
}

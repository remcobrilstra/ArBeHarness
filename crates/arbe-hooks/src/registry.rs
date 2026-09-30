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

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Runs every hook registered for `phase`, in registration order,
    /// threading the (possibly transformed) payload through the chain.
    pub async fn run_phase(&self, phase: HookPhase, payload: Value) -> Value {
        self.run_phase_reporting(phase, payload).await.0
    }

    /// [`run_phase`](Self::run_phase), also returning a report for every
    /// hook that failed, panicked, or timed out (and was skipped).
    pub async fn run_phase_reporting(
        &self,
        phase: HookPhase,
        payload: Value,
    ) -> (Value, Vec<HookFailure>) {
        let mut current = payload;
        let mut failures = Vec::new();
        for hook in self.hooks.iter().filter(|h| h.phase() == phase) {
            let name = hook.name();
            let blocking = hook.blocks_on_failure();
            let limit = hook.timeout().unwrap_or(self.timeout);
            let hook = hook.clone();
            let input = current.clone();
            let mut handle = tokio::spawn(async move { hook.run(input).await });
            let outcome = tokio::time::timeout(limit, &mut handle).await;

            let reason = match outcome {
                Ok(Ok(Ok(transformed))) => {
                    current = transformed;
                    continue;
                }
                Ok(Ok(Err(err))) => err.to_string(),
                Ok(Err(join_err)) => format!("panicked: {join_err}"),
                Err(_elapsed) => {
                    // Dropping the timeout future doesn't stop the spawned
                    // task — only .abort() does. Without this, a hook that
                    // times out (e.g. a slow outbound HTTP call) keeps
                    // running in the background indefinitely instead of
                    // actually being cut off.
                    handle.abort();
                    format!("timed out after {} ms", limit.as_millis())
                }
            };
            tracing::warn!(hook = %name, %reason, "hook failed; skipping its output");
            failures.push(HookFailure {
                hook: name,
                reason,
                blocking,
            });
        }
        (current, failures)
    }
}

/// A hook that was skipped because it failed.
#[derive(Debug, Clone, PartialEq)]
pub struct HookFailure {
    pub hook: String,
    pub reason: String,
    /// The hook asked for its failure to block what it guards (e.g. a
    /// `before_tool_execute` guard: the call is refused) instead of being
    /// skipped.
    pub blocking: bool,
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
    async fn failures_are_reported_with_the_hooks_name() {
        let mut registry = HookRegistry::new(Duration::from_millis(500));
        registry.register(Arc::new(FailingHook(HookPhase::OnError)));
        registry.register(Arc::new(AppendHook(HookPhase::OnError, "-after")));

        let (result, failures) = registry
            .run_phase_reporting(HookPhase::OnError, json!("start"))
            .await;
        assert_eq!(result, json!("start-after"));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].hook, "on_error hook");
        assert!(failures[0].reason.contains("nope"));
    }

    struct PatientHook;

    #[async_trait]
    impl Hook for PatientHook {
        fn phase(&self) -> HookPhase {
            HookPhase::OnError
        }
        async fn run(&self, payload: Value) -> Result<Value, HookError> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(json!(format!("{}-patient", payload.as_str().unwrap())))
        }
        fn timeout(&self) -> Option<Duration> {
            Some(Duration::from_secs(5))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hooks_own_timeout_overrides_the_registry_default() {
        let mut registry = HookRegistry::new(Duration::from_millis(50));
        registry.register(Arc::new(PatientHook));
        let result = registry.run_phase(HookPhase::OnError, json!("start")).await;
        assert_eq!(result, json!("start-patient"));
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

    struct MarkerHook(HookPhase, Arc<std::sync::atomic::AtomicBool>);

    #[async_trait]
    impl Hook for MarkerHook {
        fn phase(&self) -> HookPhase {
            self.0
        }
        async fn run(&self, payload: Value) -> Result<Value, HookError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            self.1.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(payload)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_hook_is_actually_aborted_not_just_ignored() {
        let ran_to_completion = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = HookRegistry::new(Duration::from_millis(50));
        registry.register(Arc::new(MarkerHook(
            HookPhase::OnError,
            ran_to_completion.clone(),
        )));

        registry.run_phase(HookPhase::OnError, json!("start")).await;

        // Advance virtual time well past the hook's own 10s sleep. If the
        // spawned task were merely ignored (not aborted), it would still be
        // running in the background and would flip the flag once its sleep
        // elapses; aborting it means it never gets the chance to.
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;

        assert!(!ran_to_completion.load(std::sync::atomic::Ordering::SeqCst));
    }
}

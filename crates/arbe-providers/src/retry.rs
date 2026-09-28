//! Retry with exponential backoff for transient provider failures
//! (rate limits, overload, timeouts, dropped connections).
//!
//! Retries happen only *before* the first event of a response is handed
//! to the caller. Once output has started streaming, a failure is
//! surfaced instead: silently restarting would duplicate text the user has
//! already seen.

use std::time::Duration;

use arbe_core::ProviderError;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{ModelProvider, ModelRequest, ProviderStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Retries after the first attempt; 0 disables retrying.
    pub max_retries: u32,
    /// Delay before the first retry; doubles on each subsequent one.
    pub base_delay: Duration,
    /// Upper bound for any single wait — including a provider's own
    /// `Retry-After`. A provider asking for longer than this fails fast
    /// instead of silently stalling the turn.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Backoff before retry number `attempt` (0-based): `base * 2^attempt`,
    /// capped at `max_delay`, scaled by `jitter` in `[0.5, 1.0]` so
    /// concurrent clients hitting the same limit don't retry in lockstep.
    pub fn backoff(&self, attempt: u32, jitter: f64) -> Duration {
        let exponential = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max_delay);
        exponential.mul_f64(jitter.clamp(0.5, 1.0))
    }

    /// How long to wait before retrying after `err`, or `None` if it
    /// shouldn't be retried at all.
    fn delay_for(&self, err: &ProviderError, attempt: u32, jitter: f64) -> Option<Duration> {
        if !err.is_retryable() || attempt >= self.max_retries {
            return None;
        }
        let delay = match err {
            ProviderError::RateLimit {
                retry_after: Some(requested),
                ..
            } => *requested,
            _ => self.backoff(attempt, jitter),
        };
        (delay <= self.max_delay).then_some(delay)
    }
}

/// Reported to the caller before each retry, e.g. to show "rate limited,
/// retrying in 4s" instead of an unexplained pause.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryNotice {
    /// 1 for the first retry.
    pub attempt: u32,
    pub delay: Duration,
    pub reason: String,
}

/// Cheap jitter without an RNG dependency: sub-second clock noise is
/// plenty to de-synchronize retries.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    0.5 + (nanos % 1_000) as f64 / 2_000.0
}

/// [`ModelProvider::stream`] with retries per `policy`. An error that
/// arrives as the stream's *first* item (some providers report overload
/// that way) is treated like a failed request and retried too. Waiting
/// between attempts is cancellable.
pub async fn stream_with_retry(
    provider: &dyn ModelProvider,
    req: ModelRequest,
    cancel: &CancellationToken,
    policy: &RetryPolicy,
    mut on_retry: impl FnMut(&RetryNotice) + Send,
) -> Result<ProviderStream, ProviderError> {
    let mut attempt = 0;
    loop {
        let err = match provider.stream(req.clone(), cancel.clone()).await {
            Ok(mut stream) => match stream.next().await {
                Some(Ok(first)) => {
                    let rest = stream;
                    return Ok(Box::pin(
                        futures_util::stream::iter([Ok(first)]).chain(rest),
                    ));
                }
                Some(Err(err)) => err,
                None => return Ok(Box::pin(futures_util::stream::empty())),
            },
            Err(err) => err,
        };

        let Some(delay) = policy.delay_for(&err, attempt, jitter()) else {
            return Err(err);
        };
        attempt += 1;
        tracing::warn!(attempt, ?delay, %err, "provider request failed; retrying");
        on_retry(&RetryNotice {
            attempt,
            delay,
            reason: err.to_string(),
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelCapabilities, ProviderEvent};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Fails with each scripted error in turn (at request time, or as the
    /// stream's first item), then succeeds.
    struct Flaky {
        failures: Mutex<Vec<(ProviderError, bool)>>,
        calls: Mutex<u32>,
    }

    impl Flaky {
        /// `(error, in_stream)`: `in_stream` delivers it as the first
        /// stream item rather than from `stream()` itself.
        fn new(failures: Vec<(ProviderError, bool)>) -> Self {
            Self {
                failures: Mutex::new(failures),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for Flaky {
        fn id(&self) -> &str {
            "flaky"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calls: false,
                vision: false,
                thinking: false,
                prompt_caching: false,
                max_context_tokens: 1_000,
            }
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            let mut failures = self.failures.lock().unwrap();
            if failures.is_empty() {
                return Ok(Box::pin(futures_util::stream::iter([
                    Ok(ProviderEvent::TextDelta("ok".into())),
                    Ok(ProviderEvent::TextDelta("!".into())),
                ])));
            }
            let (err, in_stream) = failures.remove(0);
            if in_stream {
                Ok(Box::pin(futures_util::stream::iter([Err(err)])))
            } else {
                Err(err)
            }
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            messages: vec![],
            temperature: 0.0,
            max_tokens: 1,
            tools: vec![],
            thinking_budget_tokens: None,
        }
    }

    fn fast_policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
        }
    }

    async fn collect_text(stream: ProviderStream) -> String {
        stream
            .map(|e| match e.unwrap() {
                ProviderEvent::TextDelta(t) => t,
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .await
            .concat()
    }

    #[test]
    fn backoff_doubles_is_capped_and_jittered_down_by_at_most_half() {
        let policy = RetryPolicy {
            max_retries: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
        };
        assert_eq!(policy.backoff(0, 1.0), Duration::from_secs(1));
        assert_eq!(policy.backoff(2, 1.0), Duration::from_secs(4));
        assert_eq!(policy.backoff(8, 1.0), Duration::from_secs(10));
        assert_eq!(policy.backoff(2, 0.5), Duration::from_secs(2));
        // Out-of-range jitter is clamped.
        assert_eq!(policy.backoff(0, 0.0), Duration::from_millis(500));
    }

    #[test]
    fn delay_honours_retry_after_but_not_beyond_the_cap() {
        let policy = fast_policy(3);
        let asks = |secs: f64| ProviderError::RateLimit {
            message: "slow".into(),
            retry_after: Some(Duration::from_secs_f64(secs)),
        };
        assert_eq!(
            policy.delay_for(&asks(0.2), 0, 1.0),
            Some(Duration::from_millis(200))
        );
        assert_eq!(policy.delay_for(&asks(5.0), 0, 1.0), None);
        assert_eq!(
            policy.delay_for(&ProviderError::Auth("x".into()), 0, 1.0),
            None
        );
        assert_eq!(
            policy.delay_for(&ProviderError::Overloaded("x".into()), 3, 1.0),
            None
        );
    }

    #[tokio::test]
    async fn retries_transient_failures_then_streams_everything() {
        let provider = Flaky::new(vec![
            (ProviderError::rate_limit("429"), false),
            (ProviderError::Overloaded("529".into()), true),
        ]);
        let mut notices = Vec::new();
        let stream = stream_with_retry(
            &provider,
            request(),
            &CancellationToken::new(),
            &fast_policy(3),
            |n| notices.push(n.clone()),
        )
        .await
        .unwrap();
        assert_eq!(collect_text(stream).await, "ok!");
        assert_eq!(provider.calls(), 3);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].attempt, 1);
        assert!(notices[1].reason.contains("overloaded"));
    }

    #[tokio::test]
    async fn gives_up_after_max_retries_with_the_last_error() {
        let provider = Flaky::new(vec![
            (ProviderError::Timeout("1".into()), false),
            (ProviderError::Timeout("2".into()), false),
            (ProviderError::Timeout("3".into()), false),
        ]);
        let Err(err) = stream_with_retry(
            &provider,
            request(),
            &CancellationToken::new(),
            &fast_policy(2),
            |_| {},
        )
        .await
        else {
            panic!("expected failure");
        };
        assert!(matches!(err, ProviderError::Timeout(ref m) if m == "3"));
        assert_eq!(provider.calls(), 3);
    }

    #[tokio::test]
    async fn non_retryable_errors_fail_immediately() {
        let provider = Flaky::new(vec![(ProviderError::Unreachable("refused".into()), false)]);
        let result = stream_with_retry(
            &provider,
            request(),
            &CancellationToken::new(),
            &fast_policy(5),
            |_| panic!("must not retry"),
        )
        .await;
        assert!(matches!(result, Err(ProviderError::Unreachable(_))));
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn cancelling_during_the_backoff_wait_stops_retrying() {
        let provider = Flaky::new(vec![(ProviderError::rate_limit("429"), false)]);
        let cancel = CancellationToken::new();
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(60),
        };
        let trigger = cancel.clone();
        let result = stream_with_retry(&provider, request(), &cancel, &policy, move |_| {
            trigger.cancel()
        })
        .await;
        assert!(matches!(result, Err(ProviderError::Cancelled)));
        assert_eq!(provider.calls(), 1);
    }
}

use std::time::Duration;

use arbe_core::ProviderError;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};

/// Normalizes an HTTP status + response body into the shared
/// `ProviderError` taxonomy (harness spec §6), so callers never need to
/// branch on a specific provider's status-code conventions.
pub fn map_http_error(status: StatusCode, body: &str) -> ProviderError {
    match status.as_u16() {
        401 | 403 => ProviderError::Auth(body.to_string()),
        429 => ProviderError::rate_limit(body),
        408 | 504 => ProviderError::Timeout(body.to_string()),
        // 503 Service Unavailable, and Anthropic's non-standard 529 Overloaded.
        503 | 529 => ProviderError::Overloaded(body.to_string()),
        400 | 413 if is_context_length_error(body) => {
            ProviderError::ContextLengthExceeded(body.to_string())
        }
        s if (400..500).contains(&s) => ProviderError::InvalidRequest(body.to_string()),
        _ => ProviderError::Internal(format!("HTTP {status}: {body}")),
    }
}

/// [`map_http_error`] plus the response headers, which carry the
/// provider's requested back-off (`Retry-After`) on rate limits and
/// overload responses.
pub fn map_http_response(status: StatusCode, headers: &HeaderMap, body: &str) -> ProviderError {
    match map_http_error(status, body) {
        ProviderError::RateLimit { message, .. } => ProviderError::RateLimit {
            message,
            retry_after: parse_retry_after(headers),
        },
        other => other,
    }
}

/// `Retry-After` in its delay-seconds form (the form model APIs use). The
/// HTTP-date form is rare here and ignored rather than half-supported.
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    let secs: f64 = value.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Providers signal "prompt too long" as a generic 400 with a recognizable
/// message; this is the union of the phrasings in use.
fn is_context_length_error(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "prompt is too long",
        "context window",
        "too many tokens",
    ]
    .iter()
    .any(|needle| body.contains(needle))
}

/// Transport-level failures: timeouts and mid-request drops are
/// transient; failing to connect at all is not (see
/// `ProviderError::Unreachable`). `{:#}`-style source chains are included
/// because reqwest's top-level message alone ("error sending request") is
/// rarely enough to act on.
pub fn map_transport_error(err: reqwest::Error) -> ProviderError {
    let message = error_chain(&err);
    if err.is_timeout() {
        ProviderError::Timeout(message)
    } else if err.is_connect() {
        ProviderError::Unreachable(message)
    } else if err.is_request() || err.is_body() {
        ProviderError::Network(message)
    } else {
        ProviderError::Internal(message)
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn maps_common_status_codes() {
        assert!(matches!(
            map_http_error(StatusCode::UNAUTHORIZED, "bad key"),
            ProviderError::Auth(_)
        ));
        assert!(matches!(
            map_http_error(StatusCode::TOO_MANY_REQUESTS, "slow down"),
            ProviderError::RateLimit { .. }
        ));
        assert!(matches!(
            map_http_error(StatusCode::BAD_REQUEST, "bad json"),
            ProviderError::InvalidRequest(_)
        ));
        assert!(matches!(
            map_http_error(StatusCode::INTERNAL_SERVER_ERROR, "oops"),
            ProviderError::Internal(_)
        ));
    }

    #[test]
    fn overload_statuses_are_overloaded_including_anthropic_529() {
        assert!(matches!(
            map_http_error(StatusCode::SERVICE_UNAVAILABLE, "busy"),
            ProviderError::Overloaded(_)
        ));
        assert!(matches!(
            map_http_error(StatusCode::from_u16(529).unwrap(), "overloaded_error"),
            ProviderError::Overloaded(_)
        ));
    }

    #[test]
    fn recognizes_context_length_errors_across_providers() {
        for body in [
            r#"{"error":{"code":"context_length_exceeded"}}"#,
            "This model's maximum context length is 128000 tokens",
            r#"{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}"#,
        ] {
            assert!(
                matches!(
                    map_http_error(StatusCode::BAD_REQUEST, body),
                    ProviderError::ContextLengthExceeded(_)
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn rate_limits_carry_retry_after_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("2.5"));
        let err = map_http_response(StatusCode::TOO_MANY_REQUESTS, &headers, "slow");
        let ProviderError::RateLimit { retry_after, .. } = err else {
            panic!("expected rate limit, got {err:?}");
        };
        assert_eq!(retry_after, Some(Duration::from_millis(2_500)));
    }

    #[test]
    fn unparseable_retry_after_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        let err = map_http_response(StatusCode::TOO_MANY_REQUESTS, &headers, "slow");
        assert!(matches!(
            err,
            ProviderError::RateLimit {
                retry_after: None,
                ..
            }
        ));
    }
}

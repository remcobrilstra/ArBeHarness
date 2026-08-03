use arbe_core::ProviderError;
use reqwest::StatusCode;

/// Normalizes an HTTP status + response body into the shared
/// `ProviderError` taxonomy (harness spec §6), so callers never need to
/// branch on a specific provider's status-code conventions.
pub fn map_http_error(status: StatusCode, body: &str) -> ProviderError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Auth(body.to_string()),
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimit(body.to_string()),
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            ProviderError::Timeout(body.to_string())
        }
        s if s.is_client_error() => ProviderError::InvalidRequest(body.to_string()),
        _ => ProviderError::Internal(format!("HTTP {status}: {body}")),
    }
}

pub fn map_transport_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout(err.to_string())
    } else {
        ProviderError::Internal(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_status_codes() {
        assert!(matches!(
            map_http_error(StatusCode::UNAUTHORIZED, "bad key"),
            ProviderError::Auth(_)
        ));
        assert!(matches!(
            map_http_error(StatusCode::TOO_MANY_REQUESTS, "slow down"),
            ProviderError::RateLimit(_)
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
}

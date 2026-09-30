//! The OAuth HTTP exchanges: the device-code grant (RFC 8628) and the
//! refresh-token grant, against whatever endpoints a scheme names.

use std::time::{Duration, Instant};

use arbe_core::ProviderError;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{AuthScheme, DevicePrompt, Issued};

/// Asks for a device code, shows it, and polls until the person confirms.
pub(super) async fn sign_in(
    scheme: &AuthScheme,
    client: &reqwest::Client,
    cancel: &CancellationToken,
    mut on_prompt: impl FnMut(&DevicePrompt) + Send,
) -> Result<Issued, ProviderError> {
    let device = request_device_code(scheme, client, cancel).await?;
    on_prompt(&DevicePrompt {
        user_code: device.user_code,
        verification_uri: device.verification_uri,
        verification_uri_complete: device.verification_uri_complete,
    });
    let deadline = Instant::now() + Duration::from_secs(device.expires_in.max(1));
    let mut interval = device.interval.clamp(1, 30);
    loop {
        sleep_or_cancel(Duration::from_secs(interval), cancel).await?;
        if Instant::now() >= deadline {
            return Err(code_expired(scheme));
        }
        let body = form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", &scheme.client_id),
            ("device_code", &device.device_code),
        ]);
        let value = post_form(scheme, client, &scheme.endpoints.token_url, body, cancel).await?;
        match interpret_poll(scheme, &value)? {
            Poll::Pending => {}
            Poll::SlowDown => interval = (interval + 5).min(30),
            Poll::Tokens(issued) => return Ok(issued),
        }
    }
}

/// Redeems a refresh token for a new token pair.
pub(super) async fn refresh(
    scheme: &AuthScheme,
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<Issued, ProviderError> {
    let body = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", &scheme.client_id),
        ("refresh_token", refresh_token),
    ]);
    // Not tied to a turn's cancellation: other requests may be waiting on
    // this refresh. The client's own timeout bounds it.
    let value = post_form(
        scheme,
        client,
        &scheme.endpoints.token_url,
        body,
        &CancellationToken::new(),
    )
    .await?;
    match interpret_poll(scheme, &value)? {
        Poll::Tokens(issued) => Ok(issued),
        Poll::Pending | Poll::SlowDown => Err(session_expired(scheme)),
    }
}

struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug)]
enum Poll {
    Pending,
    SlowDown,
    Tokens(Issued),
}

async fn request_device_code(
    scheme: &AuthScheme,
    client: &reqwest::Client,
    cancel: &CancellationToken,
) -> Result<DeviceCode, ProviderError> {
    let what = format!("starting the {} sign-in", scheme.display_name);
    let body = form(&[("client_id", &scheme.client_id), ("scope", &scheme.scope)]);
    let value = post_form(
        scheme,
        client,
        &scheme.endpoints.device_code_url,
        body,
        cancel,
    )
    .await?;
    if let Some(code) = value.get("error").and_then(|v| v.as_str()) {
        return Err(ProviderError::Auth(format!(
            "{what} failed ({code}). {}",
            scheme.login_hint()
        )));
    }
    Ok(DeviceCode {
        device_code: required_str(scheme, &value, "device_code", &what)?,
        user_code: required_str(scheme, &value, "user_code", &what)?,
        verification_uri: required_str(scheme, &value, "verification_uri", &what)?,
        verification_uri_complete: value
            .get("verification_uri_complete")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        expires_in: value
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(600),
        interval: value.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
    })
}

fn interpret_poll(scheme: &AuthScheme, value: &Value) -> Result<Poll, ProviderError> {
    let Some(error) = value.get("error").and_then(|v| v.as_str()) else {
        return Ok(Poll::Tokens(issued_from(scheme, value)?));
    };
    match error {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown),
        "access_denied" | "authorization_denied" => Err(ProviderError::Auth(format!(
            "the {} sign-in was declined. {}",
            scheme.display_name,
            scheme.login_hint()
        ))),
        "expired_token" => Err(code_expired(scheme)),
        _ if scheme.is_entitlement_refusal(&value.to_string()) => {
            Err(scheme.entitlement_error(Some(error)))
        }
        "invalid_grant" => Err(session_expired(scheme)),
        other => Err(ProviderError::Auth(format!(
            "the {} sign-in failed ({other}). {}",
            scheme.display_name,
            scheme.login_hint()
        ))),
    }
}

fn issued_from(scheme: &AuthScheme, value: &Value) -> Result<Issued, ProviderError> {
    let what = format!("reading the {} sign-in response", scheme.display_name);
    Ok(Issued {
        access_token: required_str(scheme, value, "access_token", &what)?,
        refresh_token: required_str(scheme, value, "refresh_token", &what)?,
        expires_in: value
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0),
    })
}

fn code_expired(scheme: &AuthScheme) -> ProviderError {
    ProviderError::Auth(format!(
        "the sign-in code expired before it was confirmed. {}",
        scheme.login_hint()
    ))
}

fn session_expired(scheme: &AuthScheme) -> ProviderError {
    ProviderError::Auth(format!(
        "the {} session expired. {}",
        scheme.display_name,
        scheme.login_hint()
    ))
}

fn required_str(
    scheme: &AuthScheme,
    value: &Value,
    field: &str,
    what: &str,
) -> Result<String, ProviderError> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ProviderError::Auth(format!(
                "{what} failed: the response had no {field}. {}",
                scheme.login_hint()
            ))
        })
}

/// POSTs a form and returns the JSON answer, or an error when the answer
/// isn't one a sign-in service gives.
async fn post_form(
    scheme: &AuthScheme,
    client: &reqwest::Client,
    url: &str,
    body: String,
    cancel: &CancellationToken,
) -> Result<Value, ProviderError> {
    let request = client
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(body);
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        response = request.send() => response.map_err(crate::error_map::map_transport_error)?,
    };
    let status = response.status();
    let text = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        text = response.text() => text.map_err(crate::error_map::map_transport_error)?,
    };
    if let Ok(value) = serde_json::from_str::<Value>(&text)
        && (value.get("error").is_some()
            || value.get("access_token").is_some()
            || value.get("device_code").is_some())
    {
        return Ok(value);
    }
    if status.is_success() {
        return Err(ProviderError::Auth(format!(
            "the {} sign-in service returned a response the harness could not read. {}",
            scheme.display_name,
            scheme.login_hint()
        )));
    }
    if status.as_u16() == 403 || scheme.is_entitlement_refusal(&text) {
        return Err(scheme.entitlement_error(None));
    }
    Err(ProviderError::Auth(format!(
        "the {} sign-in service returned HTTP {status}. {}",
        scheme.display_name,
        scheme.login_hint()
    )))
}

async fn sleep_or_cancel(
    duration: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::testing::scheme;
    use super::*;

    #[test]
    fn form_encoding_escapes_the_device_grant() {
        let body = form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code")]);
        assert_eq!(
            body,
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
        );
    }

    #[test]
    fn poll_errors_name_the_scheme_and_its_login_command() {
        let scheme = scheme("http://unused");
        let declined = interpret_poll(&scheme, &serde_json::json!({"error": "access_denied"}))
            .unwrap_err()
            .to_string();
        assert!(
            declined.contains("Test subscription sign-in was declined"),
            "{declined}"
        );
        assert!(declined.contains("arbeharness login test"), "{declined}");

        let refused = interpret_poll(
            &scheme,
            &serde_json::json!({"error": "forbidden", "detail": "spending-limit"}),
        )
        .unwrap_err()
        .to_string();
        assert!(refused.contains("isn't entitled"), "{refused}");
    }

    #[test]
    fn a_token_response_without_a_refresh_token_is_refused() {
        let scheme = scheme("http://unused");
        let err = interpret_poll(&scheme, &serde_json::json!({"access_token": "a"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no refresh_token"), "{err}");
    }
}

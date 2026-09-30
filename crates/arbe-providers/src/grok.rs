//! Grok subscription provider.
//!
//! Chat Completions against `https://cli-chat-proxy.grok.com/v1`, authorized
//! by signing in to a Grok account (the [`auth_scheme`] below, through the
//! generic [`crate::auth`] module) rather than with an API key.
//! A 401 refreshes the session once and retries. A 402 or an entitlement
//! refusal is reported as such, because signing in again will not fix it.

use arbe_core::ProviderError;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::auth::{Account, AuthScheme, OAuthEndpoints};
use crate::catalog::ModelCatalog;
use crate::openai::{chat_completion_body, stream_sse_chat};
use crate::{ModelCapabilities, ModelProvider, ModelRequest, ProviderStream, http};

/// How a Grok subscription signs in: the Grok CLI's public OAuth client at
/// `auth.x.ai`. This is a different rail from an `XAI_API_KEY`.
pub(crate) fn auth_scheme() -> AuthScheme {
    AuthScheme {
        id: "grok".into(),
        display_name: "Grok subscription".into(),
        client_id: "b1a00492-073a-47ea-816f-4c329264a828".into(),
        scope: "openid profile email offline_access grok-cli:access api:access \
                conversations:read conversations:write"
            .into(),
        endpoints: OAuthEndpoints {
            device_code_url: "https://auth.x.ai/oauth2/device/code".into(),
            token_url: "https://auth.x.ai/oauth2/token".into(),
        },
        entitlement_markers: vec![
            "spending-limit".into(),
            "personal-team-blocked".into(),
            "entitlement".into(),
        ],
        entitlement_hint: "An XAI_API_KEY uses the developer API at https://api.x.ai, \
                           which is a separate bill."
            .into(),
    }
}

/// Chat Completions base URL the subscription token is allowed to reach.
const DEFAULT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const ALLOWED_HOST: &str = "cli-chat-proxy.grok.com";

/// `configured` must be empty or `https://cli-chat-proxy.grok.com` with an
/// optional `/v1`. Anything else is refused so a project config cannot
/// point the subscription token at another host.
fn normalize_base_url(configured: Option<&str>) -> Result<String, ProviderError> {
    let raw = configured.map(str::trim).filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return Ok(DEFAULT_BASE_URL.to_string());
    };
    let url = reqwest::Url::parse(raw).map_err(|e| {
        ProviderError::InvalidRequest(format!("grok_subscription base_url is not a URL: {e}"))
    })?;
    let path = url.path().trim_end_matches('/');
    let path_ok = path.is_empty() || path == "/v1";
    if url.scheme() != "https"
        || url.host_str() != Some(ALLOWED_HOST)
        || url.port().is_some()
        || url.query().is_some()
        || !path_ok
    {
        return Err(ProviderError::InvalidRequest(format!(
            "grok_subscription only sends the subscription token to {DEFAULT_BASE_URL}. \
             For other endpoints, use openai_compatible with an API key."
        )));
    }
    Ok(DEFAULT_BASE_URL.to_string())
}

/// Sent so the CLI proxy treats the bearer as a Grok CLI session.
const TOKEN_AUTH: &str = "xai-grok-cli";
const CLIENT_IDENTIFIER: &str = "grok-shell";
/// Matches a current Grok CLI release. The proxy rejects client versions
/// below its floor; it does not require this exact build.
const CLIENT_VERSION: &str = "1.0.44";

const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-xai-token-auth",
    "x-grok-client-identifier",
    "x-grok-client-version",
];

pub struct GrokSubscriptionProvider {
    client: reqwest::Client,
    base_url: String,
    account: Account,
    extra_headers: Vec<(String, String)>,
    catalog: ModelCatalog,
}

impl GrokSubscriptionProvider {
    /// `auth_dir` is where signed-in accounts are stored (`<home>/auth`).
    pub fn new(
        base_url: Option<String>,
        auth_dir: &std::path::Path,
        extra_headers: Vec<(String, String)>,
        catalog: ModelCatalog,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            client: http::client(),
            base_url: normalize_base_url(base_url.as_deref())?,
            account: Account::new(auth_scheme(), auth_dir),
            extra_headers,
            catalog,
        })
    }

    /// Skips the host allowlist. The subscription token must not be
    /// sendable to an arbitrary URL outside tests.
    #[cfg(test)]
    fn for_test(base_url: String, account: Account) -> Self {
        Self {
            client: http::client(),
            base_url,
            account,
            extra_headers: Vec::new(),
            catalog: ModelCatalog::new(),
        }
    }

    fn request(&self, body: &impl serde::Serialize, token: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(body);
        let headers: Vec<(String, String)> = self
            .extra_headers
            .iter()
            .filter(|(name, _)| {
                !RESERVED_HEADERS
                    .iter()
                    .any(|reserved| name.eq_ignore_ascii_case(reserved))
            })
            .cloned()
            .collect();
        request = http::with_extra_headers(request, &headers);
        request
            .bearer_auth(token)
            .header("X-XAI-Token-Auth", TOKEN_AUTH)
            .header("x-grok-client-identifier", CLIENT_IDENTIFIER)
            .header("x-grok-client-version", CLIENT_VERSION)
    }
}

enum Attempt {
    Ready(reqwest::Response),
    Unauthorized,
    Failed(ProviderError),
}

async fn classify(
    scheme: &AuthScheme,
    response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Attempt, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(Attempt::Ready(response));
    }
    let headers = response.headers().clone();
    let body = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        body = response.text() => body.map_err(crate::error_map::map_transport_error)?,
    };
    if status.as_u16() == 401 {
        return Ok(Attempt::Unauthorized);
    }
    if status.as_u16() == 402 || scheme.is_entitlement_refusal(&body) {
        return Ok(Attempt::Failed(scheme.entitlement_error(None)));
    }
    Ok(Attempt::Failed(crate::error_map::map_http_response(
        status, &headers, &body,
    )))
}

#[async_trait]
impl ModelProvider for GrokSubscriptionProvider {
    fn id(&self) -> &str {
        "grok_subscription"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        self.catalog.lookup(self.id(), model)
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = chat_completion_body(&req);
        let token = self.account.bearer(false).await?;
        let first = self.request(&body, &token).send();
        let first = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            response = first => response.map_err(crate::error_map::map_transport_error)?,
        };
        let response = match classify(self.account.scheme(), first, &cancel).await? {
            Attempt::Ready(response) => response,
            Attempt::Failed(err) => return Err(err),
            Attempt::Unauthorized => {
                let token = self.account.bearer(true).await?;
                let retry = self.request(&body, &token).send();
                let retry = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                    response = retry => response.map_err(crate::error_map::map_transport_error)?,
                };
                match classify(self.account.scheme(), retry, &cancel).await? {
                    Attempt::Ready(response) => response,
                    Attempt::Unauthorized => {
                        let scheme = self.account.scheme();
                        return Err(ProviderError::Auth(format!(
                            "the {} rejected the session. {}",
                            scheme.display_name,
                            scheme.login_hint()
                        )));
                    }
                    Attempt::Failed(err) => return Err(err),
                }
            }
        };
        Ok(stream_sse_chat(response, cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::testing::{now, store, temp_dir};
    use crate::infer;
    use arbe_core::{Message, Role};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The real Grok scheme, with its token endpoint on `server`, and a
    /// provider that sends chat requests there too.
    fn provider(server: &str, dir: &std::path::Path) -> GrokSubscriptionProvider {
        let mut scheme = auth_scheme();
        scheme.endpoints = OAuthEndpoints {
            device_code_url: format!("{server}/device"),
            token_url: format!("{server}/token"),
        };
        GrokSubscriptionProvider::for_test(format!("{server}/v1"), Account::new(scheme, dir))
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "grok-4.7".into(),
            messages: vec![Message::new(Role::User, "hi")],
            temperature: 0.2,
            max_tokens: 32,
            tools: Vec::new(),
            thinking_budget_tokens: None,
        }
    }

    fn hello_sse() -> String {
        [
            r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ]
        .map(|line| format!("{line}\n\n"))
        .concat()
    }

    #[test]
    fn only_the_cli_proxy_is_an_acceptable_base_url() {
        assert_eq!(normalize_base_url(None).unwrap(), DEFAULT_BASE_URL);
        assert_eq!(
            normalize_base_url(Some("https://cli-chat-proxy.grok.com")).unwrap(),
            DEFAULT_BASE_URL
        );
        assert_eq!(
            normalize_base_url(Some("https://cli-chat-proxy.grok.com/v1/")).unwrap(),
            DEFAULT_BASE_URL
        );
        for bad in [
            "http://cli-chat-proxy.grok.com/v1",
            "https://api.x.ai/v1",
            "https://cli-chat-proxy.grok.com/v2",
            "https://evil.example/v1",
            "https://cli-chat-proxy.grok.com:8443/v1",
        ] {
            assert!(normalize_base_url(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_grok_account_is_stored_under_its_own_name() {
        let dir = temp_dir("grok-name");
        let account = Account::new(auth_scheme(), &dir);
        assert_eq!(account.credential_path(), dir.join("grok.json"));
        assert_eq!(
            account.scheme().login_hint(),
            "Run `arbeharness login grok`."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_valid_session_is_sent_to_the_proxy_with_the_cli_headers() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer access-token-123",
            ))
            .and(wiremock::matchers::header("x-xai-token-auth", TOKEN_AUTH))
            .and(wiremock::matchers::header(
                "x-grok-client-identifier",
                CLIENT_IDENTIFIER,
            ))
            .and(wiremock::matchers::header(
                "x-grok-client-version",
                CLIENT_VERSION,
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(hello_sse()),
            )
            .mount(&server)
            .await;

        let dir = temp_dir("grok-valid");
        let provider = provider(&server.uri(), &dir);
        store(
            &provider.account,
            "access-token-123",
            "refresh-token-123",
            now() + 3600,
        );
        let response = infer(&provider, request(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(response.text(), "Hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_rejected_access_token_is_refreshed_once() {
        let server = wiremock::MockServer::start().await;
        let chats = AtomicUsize::new(0);
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let n = chats.fetch_add(1, Ordering::SeqCst);
                let auth = req
                    .headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or(""))
                    .unwrap_or("");
                if n == 0 {
                    assert_eq!(auth, "Bearer old-access-token");
                    wiremock::ResponseTemplate::new(401).set_body_string("unauthorized")
                } else {
                    assert_eq!(auth, "Bearer new-access-token");
                    wiremock::ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(hello_sse())
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "new-access-token",
                    "refresh_token": "new-refresh-token",
                    "expires_in": 3600
                })),
            )
            .mount(&server)
            .await;

        let dir = temp_dir("grok-401");
        let provider = provider(&server.uri(), &dir);
        store(
            &provider.account,
            "old-access-token",
            "old-refresh-token",
            now() + 3600,
        );
        let response = infer(&provider, request(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(response.text(), "Hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_entitlement_refusal_is_not_treated_as_a_bad_request() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(402)
                    .set_body_string(r#"{"error":"personal-team-blocked:spending-limit"}"#),
            )
            .mount(&server)
            .await;
        let dir = temp_dir("grok-402");
        let provider = provider(&server.uri(), &dir);
        store(
            &provider.account,
            "access-token-123",
            "refresh-token-123",
            now() + 3600,
        );
        let err = infer(&provider, request(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Auth(ref message)
                if message.contains("isn't entitled to the Grok subscription")
                    && message.contains("XAI_API_KEY")),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Signing in to a model service with an account instead of an API key.
//!
//! An [`AuthScheme`] describes one OAuth client: whose sign-in service it
//! talks to, with which client id and scopes, and how that service says an
//! account isn't entitled to use it. An [`Account`] is a scheme plus the
//! file its credential lives in (`<home>/auth/<scheme id>.json`). Nothing in
//! here knows about a particular service: each provider that signs in
//! defines its scheme next to its adapter (see `grok.rs`), and
//! [`builtin_schemes`] lists them for `arbeharness login`.
//!
//! Only the device-code grant (RFC 8628) is implemented: it needs no local
//! callback port and no client secret, so it works from any terminal,
//! including over SSH. Refresh tokens are treated as rotating: a refresh
//! writes the new pair before the old one is forgotten, under a lock, so
//! two processes never redeem the same refresh token.

mod device;
mod store;

use std::path::{Path, PathBuf};
use std::time::Duration;

use arbe_core::ProviderError;
use tokio_util::sync::CancellationToken;

use crate::http;

pub use store::secrets_in_dir;

/// One way to sign in: an OAuth client registered with a sign-in service.
#[derive(Debug, Clone)]
pub struct AuthScheme {
    /// Stable id: the `arbeharness login <id>` argument and the credential
    /// file's name. Changing it signs everyone out.
    pub id: String,
    /// For messages, e.g. "Grok subscription".
    pub display_name: String,
    /// Public client id. Device-code clients have no secret.
    pub client_id: String,
    /// Space-separated scopes to request.
    pub scope: String,
    pub endpoints: OAuthEndpoints,
    /// Substrings (matched case-insensitively) of a sign-in or model
    /// response that mean the account isn't entitled to the service —
    /// something signing in again won't fix.
    pub entitlement_markers: Vec<String>,
    /// Appended to the "not entitled" message: what the person can do
    /// instead (e.g. use an API key).
    pub entitlement_hint: String,
}

impl AuthScheme {
    /// "Run `arbeharness login <id>`." — the fix for most sign-in errors.
    pub fn login_hint(&self) -> String {
        format!("Run `arbeharness login {}`.", self.id)
    }

    /// Whether `text` (a response body) is an entitlement refusal.
    pub fn is_entitlement_refusal(&self, text: &str) -> bool {
        let text = text.to_ascii_lowercase();
        self.entitlement_markers
            .iter()
            .any(|marker| text.contains(&marker.to_ascii_lowercase()))
    }

    /// The error for an account that isn't entitled to the service.
    pub fn entitlement_error(&self, code: Option<&str>) -> ProviderError {
        let code = code.map(|c| format!(" ({c})")).unwrap_or_default();
        let hint = if self.entitlement_hint.is_empty() {
            String::new()
        } else {
            format!(" {}", self.entitlement_hint)
        };
        ProviderError::SignIn(format!(
            "this account isn't entitled to the {}{code}.{hint}",
            self.display_name
        ))
    }
}

/// A sign-in service's device-code and token endpoints.
#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub device_code_url: String,
    pub token_url: String,
}

/// What the person signing in has to open and confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
    /// The URL with the code already filled in, when the service gives one.
    pub verification_uri_complete: Option<String>,
}

/// The schemes `arbeharness login` knows, sorted by id.
pub fn builtin_schemes() -> Vec<AuthScheme> {
    let mut schemes = vec![crate::grok::auth_scheme()];
    schemes.sort_by(|a, b| a.id.cmp(&b.id));
    schemes
}

/// Where credentials live under a harness home: `<home>/auth`.
pub fn auth_dir(home: &Path) -> PathBuf {
    home.join("auth")
}

/// HTTP client for sign-in services: a bounded timeout, so a stuck refresh
/// cannot hold the credential lock for as long as a model response may take.
pub fn oauth_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(http::CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to initialize the HTTP client")
}

/// A scheme and the file its credential is stored in.
#[derive(Debug, Clone)]
pub struct Account {
    scheme: AuthScheme,
    path: PathBuf,
}

impl Account {
    /// The account for `scheme`, stored in `auth_dir` (see [`auth_dir`]).
    pub fn new(scheme: AuthScheme, auth_dir: &Path) -> Self {
        let path = auth_dir.join(format!("{}.json", scheme.id));
        Self { scheme, path }
    }

    pub fn scheme(&self) -> &AuthScheme {
        &self.scheme
    }

    pub fn credential_path(&self) -> &Path {
        &self.path
    }

    /// Runs the device-code flow and saves the credential. `on_prompt` is
    /// called once, with what to show the person signing in.
    pub async fn login(
        &self,
        client: &reqwest::Client,
        cancel: &CancellationToken,
        on_prompt: impl FnMut(&DevicePrompt) + Send,
    ) -> Result<(), ProviderError> {
        let issued = device::sign_in(&self.scheme, client, cancel, on_prompt).await?;
        let _lock = store::FileLock::acquire(&self.path).await?;
        store::write_issued(&self.path, &issued)?;
        tracing::info!(scheme = %self.scheme.id, "saved a new sign-in credential");
        Ok(())
    }

    /// Deletes the credential. `Ok(true)` when there was one.
    pub async fn logout(&self) -> Result<bool, ProviderError> {
        let _lock = store::FileLock::acquire(&self.path).await?;
        store::remove(&self.path)
    }

    /// A usable access token, refreshed first when it is near expiry or
    /// when `force` is set (the service rejected the current one).
    pub async fn bearer(&self, force: bool) -> Result<String, ProviderError> {
        let scheme = &self.scheme;
        if !force && let Some(token) = store::fresh_access_token(scheme, &self.path)? {
            return Ok(token);
        }
        let _lock = store::FileLock::acquire(&self.path).await?;
        // Another process may have refreshed while this one waited.
        if !force && let Some(token) = store::fresh_access_token(scheme, &self.path)? {
            return Ok(token);
        }
        let stored = store::read_required(scheme, &self.path)?;
        let issued = device::refresh(scheme, &oauth_client(), &stored.refresh_token).await?;
        let access = issued.access_token.clone();
        let expires_at = store::write_issued(&self.path, &issued)?;
        tracing::info!(scheme = %scheme.id, expires_at, "refreshed a sign-in credential");
        Ok(access)
    }
}

/// Tokens a sign-in service issued.
#[derive(Debug)]
pub(crate) struct Issued {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: Option<i64>,
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A scheme whose endpoints are on `server` (`/device`, `/token`).
    pub fn scheme(server: &str) -> AuthScheme {
        AuthScheme {
            id: "test".into(),
            display_name: "Test subscription".into(),
            client_id: "client-1".into(),
            scope: "openid offline_access".into(),
            endpoints: OAuthEndpoints {
                device_code_url: format!("{server}/device"),
                token_url: format!("{server}/token"),
            },
            entitlement_markers: vec!["spending-limit".into()],
            entitlement_hint: "Use an API key instead.".into(),
        }
    }

    /// A fresh directory under the system temp dir.
    pub fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("arbe-auth-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a credential for `account` directly.
    pub fn store(account: &Account, access: &str, refresh: &str, expires_at: i64) {
        std::fs::write(
            account.credential_path(),
            format!(
                r#"{{"access_token":"{access}","refresh_token":"{refresh}","expires_at":{expires_at},"refresh_skew_secs":60}}"#
            ),
        )
        .unwrap();
    }

    pub fn now() -> i64 {
        store::now_unix()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_scheme_names_its_own_login_command_and_file() {
        let dir = temp_dir("names");
        let account = Account::new(scheme("http://unused"), &dir);
        assert_eq!(account.credential_path(), dir.join("test.json"));
        assert_eq!(
            account.scheme().login_hint(),
            "Run `arbeharness login test`."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entitlement_markers_match_case_insensitively() {
        let scheme = scheme("http://unused");
        assert!(scheme.is_entitlement_refusal(r#"{"error":"Spending-Limit reached"}"#));
        assert!(!scheme.is_entitlement_refusal("unauthorized"));
        let err = scheme.entitlement_error(Some("spending-limit")).to_string();
        assert!(
            err.contains(
                "isn't entitled to the Test subscription (spending-limit). Use an API key"
            ),
            "{err}"
        );
    }

    #[test]
    fn builtin_scheme_ids_are_unique() {
        let schemes = builtin_schemes();
        let mut ids: Vec<_> = schemes.iter().map(|s| s.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), schemes.len());
    }

    #[tokio::test]
    async fn device_login_polls_until_the_token_arrives_and_persists_it() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/device"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "device_code": "device-1",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://auth.example/device",
                    "verification_uri_complete": "https://auth.example/device?user_code=ABCD-EFGH",
                    "expires_in": 60,
                    "interval": 1
                })),
            )
            .mount(&server)
            .await;
        let polls = AtomicUsize::new(0);
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(move |_req: &wiremock::Request| {
                if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                    wiremock::ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({"error": "authorization_pending"}))
                } else {
                    wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "access_token": "access-token-123456",
                        "refresh_token": "refresh-token-123456",
                        "expires_in": 3600,
                        "token_type": "Bearer"
                    }))
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let dir = temp_dir("login");
        let account = Account::new(scheme(&server.uri()), &dir);
        let mut prompt = None;
        account
            .login(&oauth_client(), &CancellationToken::new(), |p| {
                prompt = Some(p.clone())
            })
            .await
            .unwrap();
        let prompt = prompt.unwrap();
        assert_eq!(prompt.user_code, "ABCD-EFGH");
        assert_eq!(
            prompt.verification_uri_complete.as_deref(),
            Some("https://auth.example/device?user_code=ABCD-EFGH")
        );
        assert_eq!(account.bearer(false).await.unwrap(), "access-token-123456");
        assert_eq!(
            secrets_in_dir(&dir),
            vec!["access-token-123456", "refresh-token-123456"]
        );

        assert!(account.logout().await.unwrap());
        assert!(!account.logout().await.unwrap());
        let err = account.bearer(false).await.unwrap_err().to_string();
        assert!(err.contains("arbeharness login test"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn refresh_rotates_the_stored_refresh_token() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .and(wiremock::matchers::body_string(
                "grant_type=refresh_token&client_id=client-1&refresh_token=old-refresh-token",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "new-access-token",
                    "refresh_token": "new-refresh-token",
                    "expires_in": 120
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = temp_dir("refresh");
        let account = Account::new(scheme(&server.uri()), &dir);
        store(
            &account,
            "old-access-token",
            "old-refresh-token",
            now() - 10,
        );

        assert_eq!(account.bearer(false).await.unwrap(), "new-access-token");
        assert_eq!(
            secrets_in_dir(&dir),
            vec!["new-access-token", "new-refresh-token"]
        );
        // Still fresh: a second call does not hit the token endpoint.
        assert_eq!(account.bearer(false).await.unwrap(), "new-access-token");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

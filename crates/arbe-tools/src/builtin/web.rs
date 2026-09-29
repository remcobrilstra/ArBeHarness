//! Web tools (v2 plan P6.7): `web_fetch` reads a page as text, and
//! `web_search` queries a search service configured by the user (Brave,
//! Tavily, or a SearXNG instance). Both are medium risk: they send data
//! (a URL, a query) off the machine, so they go through approval like any
//! other call — and a rule can allow them per site, e.g.
//! `web_fetch(https://docs.rs/*)`.

use std::time::Duration;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use futures_util::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Most of a response body read (bigger pages are cut, and say so).
const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
const DEFAULT_MAX_CHARS: usize = 20_000;
const MAX_CHARS: usize = 100_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESULTS: usize = 10;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("arbeharness/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default()
}

fn failure(e: impl std::fmt::Display) -> ToolError {
    ToolError::RuntimeFailure(e.to_string())
}

// ---------------------------------------------------------------------------
// web_fetch
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct FetchArgs {
    /// The page's full URL (http or https).
    url: String,
    /// Most characters of text to return (default 20000, at most 100000).
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Default)]
pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolExecutor for WebFetchTool {
    fn read_only(&self) -> bool {
        true
    }

    /// Rule subject: the URL, so rules can allow sites.
    fn subject(&self, arguments: &Value) -> Option<String> {
        arguments
            .get("url")
            .and_then(Value::as_str)
            .map(|u| u.trim().to_string())
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: FetchArgs = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid web_fetch arguments: {e}")))?;
        let url = reqwest::Url::parse(args.url.trim())
            .map_err(|e| ToolError::Validation(format!("not a URL: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ToolError::Validation(format!(
                "only http and https URLs can be fetched, not {}",
                url.scheme()
            )));
        }
        let max_chars = args
            .max_chars
            .unwrap_or(DEFAULT_MAX_CHARS)
            .clamp(1, MAX_CHARS);

        let fetch = async {
            let response = client().get(url).send().await.map_err(failure)?;
            let status = response.status();
            let final_url = response.url().to_string();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let (body, cut) = read_capped(response).await?;
            Ok::<_, ToolError>((status, final_url, content_type, body, cut))
        };
        let (status, final_url, content_type, body, cut) = tokio::select! {
            result = fetch => result?,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
        };

        let (title, text) = to_text(&content_type, &body)?;
        let chars = text.chars().count();
        let text: String = text.chars().take(max_chars).collect();
        let mut output = json!({
            "url": final_url,
            "status": status.as_u16(),
            "content_type": content_type,
            "text": text,
        });
        if let Some(title) = title {
            output["title"] = json!(title);
        }
        if chars > max_chars || cut {
            output["truncated"] = json!(true);
        }
        Ok(ToolResult {
            id: invocation.id,
            output,
            is_error: !status.is_success(),
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<FetchArgs>(
            "Fetch a web page and return its text (HTML is converted to readable text; JSON and plain text are returned as they are). For documentation, articles, API references. Binary files aren't supported.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }
}

/// The body, up to `MAX_BODY_BYTES`, and whether it was cut there.
async fn read_capped(response: reqwest::Response) -> Result<(Vec<u8>, bool), ToolError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(failure)?;
        let room = MAX_BODY_BYTES - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

/// The page's title (for HTML) and text.
fn to_text(content_type: &str, body: &[u8]) -> Result<(Option<String>, String), ToolError> {
    let is_html = content_type.contains("html")
        || (content_type.is_empty() && body.trim_ascii_start().starts_with(b"<"));
    if is_html {
        let raw = String::from_utf8_lossy(body);
        let title = extract_title(&raw);
        let text = html2text::from_read(body, 100)
            .map_err(|e| failure(format!("couldn't read the page: {e}")))?;
        return Ok((title, text.trim().to_string()));
    }
    let textual = content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript");
    if !textual {
        return Err(ToolError::Validation(format!(
            "the response is {content_type}, not text"
        )));
    }
    Ok((None, String::from_utf8_lossy(body).trim().to_string()))
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = start + lower[start..].find('>')? + 1;
    let close = open_end + lower[open_end..].find("</title>")?;
    let title = html[open_end..close]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!title.is_empty()).then_some(title)
}

// ---------------------------------------------------------------------------
// web_search
// ---------------------------------------------------------------------------

/// Which search service `web_search` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    /// Brave Search API (needs an API key).
    Brave,
    /// Tavily (needs an API key).
    Tavily,
    /// A SearXNG instance with the JSON format enabled (needs its URL).
    Searxng,
}

impl SearchBackend {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "brave" => Some(Self::Brave),
            "tavily" => Some(Self::Tavily),
            "searxng" => Some(Self::Searxng),
            _ => None,
        }
    }
}

/// `web_search`'s configuration (from `[web.search]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSettings {
    pub backend: SearchBackend,
    pub api_key: Option<String>,
    /// Overrides the service's default address (required for SearXNG).
    pub base_url: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// What to search for.
    query: String,
    /// How many results (default 5, at most 10).
    #[serde(default)]
    count: Option<usize>,
}

pub struct WebSearchTool {
    settings: SearchSettings,
}

impl WebSearchTool {
    pub fn new(settings: SearchSettings) -> Self {
        Self { settings }
    }

    fn base(&self, default: &str) -> String {
        self.settings
            .base_url
            .clone()
            .unwrap_or_else(|| default.to_string())
            .trim_end_matches('/')
            .to_string()
    }

    async fn search(&self, query: &str, count: usize) -> Result<Vec<Value>, ToolError> {
        let key = || {
            self.settings
                .api_key
                .clone()
                .ok_or_else(|| failure("web_search: this search service needs an API key"))
        };
        let (items, fields): (Value, (&str, &str, &str)) = match self.settings.backend {
            SearchBackend::Brave => {
                let response = client()
                    .get(format!(
                        "{}/res/v1/web/search",
                        self.base("https://api.search.brave.com")
                    ))
                    .query(&[("q", query), ("count", &count.to_string())])
                    .header("X-Subscription-Token", key()?)
                    .header("Accept", "application/json")
                    .send()
                    .await
                    .map_err(failure)?;
                let body = json_body(response).await?;
                (
                    body["web"]["results"].clone(),
                    ("title", "url", "description"),
                )
            }
            SearchBackend::Tavily => {
                let key = key()?;
                let response = client()
                    .post(format!("{}/search", self.base("https://api.tavily.com")))
                    .bearer_auth(&key)
                    .json(&json!({"query": query, "max_results": count, "api_key": key}))
                    .send()
                    .await
                    .map_err(failure)?;
                let body = json_body(response).await?;
                (body["results"].clone(), ("title", "url", "content"))
            }
            SearchBackend::Searxng => {
                let base = self
                    .settings
                    .base_url
                    .clone()
                    .ok_or_else(|| failure("web_search: SearXNG needs base_url"))?;
                let response = client()
                    .get(format!("{}/search", base.trim_end_matches('/')))
                    .query(&[("q", query), ("format", "json")])
                    .send()
                    .await
                    .map_err(failure)?;
                let body = json_body(response).await?;
                (body["results"].clone(), ("title", "url", "content"))
            }
        };
        let (title, url, snippet) = fields;
        Ok(items
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .take(count)
                    .map(|item| {
                        json!({
                            "title": item[title].as_str().unwrap_or(""),
                            "url": item[url].as_str().unwrap_or(""),
                            "snippet": item[snippet].as_str().unwrap_or(""),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

async fn json_body(response: reqwest::Response) -> Result<Value, ToolError> {
    let status = response.status();
    let text = response.text().await.map_err(failure)?;
    if !status.is_success() {
        let snippet: String = text.chars().take(300).collect();
        return Err(failure(format!(
            "search service returned {status}: {snippet}"
        )));
    }
    serde_json::from_str(&text).map_err(|e| failure(format!("unreadable search response: {e}")))
}

#[async_trait]
impl ToolExecutor for WebSearchTool {
    fn read_only(&self) -> bool {
        true
    }

    /// Rule subject: the query.
    fn subject(&self, arguments: &Value) -> Option<String> {
        arguments
            .get("query")
            .and_then(Value::as_str)
            .map(|q| q.trim().to_string())
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: SearchArgs = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid web_search arguments: {e}")))?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::Validation("query is empty".into()));
        }
        let count = args.count.unwrap_or(5).clamp(1, MAX_RESULTS);
        let results = tokio::select! {
            results = self.search(query, count) => results?,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "query": query, "results": results }),
            is_error: false,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<SearchArgs>(
            "Search the web. Returns titles, URLs and short snippets; read a result in full with web_fetch.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecuteWithDefaultContext;
    use arbe_core::{ToolCallId, TurnId};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn call(tool: &str, args: Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: tool.into(),
            arguments: args,
            risk: RiskLevel::Medium,
            rationale: None,
        }
    }

    async fn serve(route: &str, response: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn html_pages_come_back_as_text_with_their_title() {
        let server = serve(
            "/doc",
            ResponseTemplate::new(200).set_body_raw(
                "<html><head><title> The  Guide </title><script>var x=1;</script></head><body><h1>Install</h1><p>Run <b>cargo build</b>.</p></body></html>",
                "text/html; charset=utf-8",
            ),
        )
        .await;
        let r = WebFetchTool::new()
            .execute_default(call(
                "web_fetch",
                json!({"url": format!("{}/doc", server.uri())}),
            ))
            .await
            .unwrap();
        assert_eq!(r.output["title"], "The Guide");
        let text = r.output["text"].as_str().unwrap();
        assert!(
            text.contains("Install") && text.contains("cargo build"),
            "{text}"
        );
        assert!(!text.contains("var x"), "{text}");
        assert!(!r.is_error);
    }

    #[tokio::test]
    async fn text_is_capped_and_errors_and_binaries_are_reported() {
        let server = serve(
            "/big",
            ResponseTemplate::new(200).set_body_raw("a".repeat(500), "text/plain"),
        )
        .await;
        let tool = WebFetchTool::new();
        let r = tool
            .execute_default(call(
                "web_fetch",
                json!({"url": format!("{}/big", server.uri()), "max_chars": 100}),
            ))
            .await
            .unwrap();
        assert_eq!(r.output["text"].as_str().unwrap().len(), 100);
        assert_eq!(r.output["truncated"], true);

        let missing = serve(
            "/x",
            ResponseTemplate::new(404).set_body_raw("gone", "text/plain"),
        )
        .await;
        let r = tool
            .execute_default(call(
                "web_fetch",
                json!({"url": format!("{}/nope", missing.uri())}),
            ))
            .await
            .unwrap();
        assert!(r.is_error);
        assert_eq!(r.output["status"], 404);

        let binary = serve(
            "/img",
            ResponseTemplate::new(200).set_body_raw(vec![0u8, 1, 2], "image/png"),
        )
        .await;
        assert!(
            tool.execute_default(call(
                "web_fetch",
                json!({"url": format!("{}/img", binary.uri())})
            ))
            .await
            .is_err()
        );
        for bad in ["file:///etc/passwd", "not a url"] {
            assert!(
                tool.execute_default(call("web_fetch", json!({"url": bad})))
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn each_search_backend_returns_normalized_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(query_param("q", "rust async"))
            .and(header("X-Subscription-Token", "brave-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "web": {"results": [{"title": "Tokio", "url": "https://tokio.rs", "description": "An async runtime"}]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("authorization", "Bearer tavily-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"title": "Tokio", "url": "https://tokio.rs", "content": "An async runtime"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"title": "Tokio", "url": "https://tokio.rs", "content": "An async runtime"}]
            })))
            .mount(&server)
            .await;

        for (backend, key) in [
            (SearchBackend::Brave, Some("brave-key")),
            (SearchBackend::Tavily, Some("tavily-key")),
            (SearchBackend::Searxng, None),
        ] {
            let tool = WebSearchTool::new(SearchSettings {
                backend,
                api_key: key.map(str::to_string),
                base_url: Some(server.uri()),
            });
            let r = tool
                .execute_default(call("web_search", json!({"query": "rust async"})))
                .await
                .unwrap_or_else(|e| panic!("{backend:?}: {e}"));
            assert_eq!(
                r.output["results"],
                json!([{"title": "Tokio", "url": "https://tokio.rs", "snippet": "An async runtime"}]),
                "{backend:?}"
            );
        }

        let no_key = WebSearchTool::new(SearchSettings {
            backend: SearchBackend::Brave,
            api_key: None,
            base_url: Some(server.uri()),
        });
        assert!(
            no_key
                .execute_default(call("web_search", json!({"query": "x"})))
                .await
                .is_err()
        );
    }
}

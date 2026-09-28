//! MCP server configuration, as written in `config.toml`:
//!
//! ```toml
//! [mcp.servers.github]            # stdio: the harness starts the process
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-github"]
//! env = { GITHUB_API_URL = "https://api.github.com" }
//!
//! [mcp.servers.docs]              # streamable HTTP: the server is already running
//! url = "https://example.com/mcp"
//! bearer_token_env = "DOCS_MCP_TOKEN"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Default time limit for one request to a server.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// One `[mcp.servers.<name>]` table, as written.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSettings {
    /// Program to start (stdio transport).
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the process (it also inherits the
    /// harness's own environment, so secrets can stay there).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory for the process.
    pub cwd: Option<PathBuf>,
    /// Endpoint (streamable HTTP transport).
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Name of an environment variable holding a bearer token for `url`.
    pub bearer_token_env: Option<String>,
    /// Set `false` to switch off a server defined in another config file.
    pub enabled: Option<bool>,
    pub timeout_secs: Option<u64>,
}

/// How to reach a server.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        bearer_token: Option<String>,
    },
}

/// A validated, ready-to-connect server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: TransportConfig,
    pub timeout: Duration,
}

impl McpServerSettings {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Validates the table and resolves `bearer_token_env` through `env`.
    pub fn resolve(
        &self,
        name: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<McpServerConfig, String> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "mcp server name {name:?} may only contain letters, digits, '-' and '_'"
            ));
        }
        let transport = match (&self.command, &self.url) {
            (Some(command), None) => TransportConfig::Stdio {
                command: command.clone(),
                args: self.args.clone(),
                env: self.env.clone(),
                cwd: self.cwd.clone(),
            },
            (None, Some(url)) => TransportConfig::Http {
                url: url.clone(),
                headers: self.headers.clone(),
                bearer_token: match &self.bearer_token_env {
                    Some(var) => Some(env(var).ok_or_else(|| {
                        format!("mcp server {name:?}: bearer_token_env {var:?} is not set")
                    })?),
                    None => None,
                },
            },
            (Some(_), Some(_)) => {
                return Err(format!(
                    "mcp server {name:?}: set either `command` or `url`, not both"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "mcp server {name:?}: needs a `command` (stdio) or a `url` (HTTP)"
                ));
            }
        };
        Ok(McpServerConfig {
            name: name.to_string(),
            transport,
            timeout: self
                .timeout_secs
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_TIMEOUT),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn parse(text: &str) -> McpServerSettings {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn resolves_a_stdio_server() {
        let s = parse("command = \"npx\"\nargs = [\"-y\", \"pkg\"]\ntimeout_secs = 5\n");
        let c = s.resolve("github", &no_env).unwrap();
        assert!(
            matches!(c.transport, TransportConfig::Stdio { ref command, .. } if command == "npx")
        );
        assert_eq!(c.timeout, Duration::from_secs(5));
        assert!(s.is_enabled());
    }

    #[test]
    fn resolves_an_http_server_with_a_bearer_token_from_env() {
        let s = parse("url = \"https://x/mcp\"\nbearer_token_env = \"TOKEN\"\n");
        let env = |k: &str| (k == "TOKEN").then(|| "secret".to_string());
        let c = s.resolve("docs", &env).unwrap();
        assert_eq!(
            c.transport,
            TransportConfig::Http {
                url: "https://x/mcp".into(),
                headers: BTreeMap::new(),
                bearer_token: Some("secret".into())
            }
        );
        assert!(s.resolve("docs", &no_env).unwrap_err().contains("TOKEN"));
    }

    #[test]
    fn needs_exactly_one_transport_and_a_safe_name() {
        assert!(McpServerSettings::default().resolve("x", &no_env).is_err());
        let both = parse("command = \"a\"\nurl = \"b\"\n");
        assert!(both.resolve("x", &no_env).is_err());
        let ok = parse("command = \"a\"\n");
        assert!(ok.resolve("bad/name", &no_env).is_err());
        assert!(!parse("command = \"a\"\nenabled = false\n").is_enabled());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<McpServerSettings>("comand = \"a\"\n").is_err());
    }
}

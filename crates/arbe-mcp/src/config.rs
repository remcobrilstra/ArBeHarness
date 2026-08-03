use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::McpServerConfig;
use crate::protocol::McpError;

#[derive(Debug, Deserialize)]
struct ServersFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

/// Loads `~/.arbe/mcp/servers.toml` (harness spec §5). A missing file
/// yields no servers rather than an error — MCP is optional, per FR-8
/// ("handle unavailable servers gracefully").
pub fn load_servers_file(path: &Path) -> Result<Vec<McpServerConfig>, McpError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(McpError::Transport(e.to_string())),
    };
    let parsed: ServersFile =
        toml::from_str(&text).map_err(|e| McpError::Parse(format!("{}: {e}", path.display())))?;
    Ok(parsed.servers)
}

/// Servers configured with `enabled = false` are skipped entirely (FR-8).
pub fn enabled_servers(servers: &[McpServerConfig]) -> impl Iterator<Item = &McpServerConfig> {
    servers.iter().filter(|s| s.enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_no_servers() {
        let path =
            std::env::temp_dir().join(format!("arbe-mcp-missing-{}.toml", std::process::id()));
        assert!(load_servers_file(&path).unwrap().is_empty());
    }

    #[test]
    fn parses_servers_and_filters_enabled() {
        let dir = std::env::temp_dir().join(format!(
            "arbe-mcp-config-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("servers.toml");
        fs::write(
            &path,
            r#"
[[servers]]
name = "fs"
command = "mcp-server-fs"
args = ["--root", "."]
enabled = true

[[servers]]
name = "disabled-one"
command = "mcp-server-x"
args = []
enabled = false
"#,
        )
        .unwrap();

        let servers = load_servers_file(&path).unwrap();
        assert_eq!(servers.len(), 2);

        let enabled: Vec<&str> = enabled_servers(&servers).map(|s| s.name.as_str()).collect();
        assert_eq!(enabled, vec!["fs"]);

        fs::remove_dir_all(&dir).ok();
    }
}

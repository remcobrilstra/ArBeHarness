//! Keeps a session's MCP servers connected and their tools registered.

use std::path::PathBuf;
use std::sync::Arc;

use arbe_tools::ToolExecutor;
use futures_util::future::join_all;
use tokio::sync::Mutex;

use crate::bridge::{McpToolExecutor, qualified_tool_name};
use crate::client::McpClient;
use crate::config::McpServerConfig;
use crate::protocol::McpError;

/// Where a server's tools go — implemented by the runtime over its tool
/// registry.
pub trait ToolSink: Send + Sync {
    /// Replaces every tool previously registered for `server` with `tools`
    /// (name → executor). An empty list removes the server's tools.
    fn replace_server_tools(&self, server: &str, tools: Vec<(String, Arc<dyn ToolExecutor>)>);
}

/// A connection attempt's outcome, for reporting to the user.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerStatus {
    Connected { server: String, tools: usize },
    Failed { server: String, error: String },
}

/// One configured server. Connects lazily, and reconnects if the
/// connection has closed (e.g. the process crashed) the next time a
/// client is needed.
pub struct McpServer {
    config: McpServerConfig,
    log_dir: Option<PathBuf>,
    client: Mutex<Option<Arc<McpClient>>>,
}

impl McpServer {
    pub fn new(config: McpServerConfig, log_dir: Option<PathBuf>) -> Self {
        Self {
            config,
            log_dir,
            client: Mutex::new(None),
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// A live client, connecting (or reconnecting) if needed.
    pub async fn client(&self) -> Result<Arc<McpClient>, McpError> {
        let mut slot = self.client.lock().await;
        if let Some(client) = slot.as_ref()
            && !client.is_closed()
        {
            return Ok(client.clone());
        }
        if slot.is_some() {
            tracing::info!(server = %self.config.name, "mcp server connection closed; reconnecting");
        }
        let client = Arc::new(McpClient::connect(&self.config, self.log_dir.as_deref()).await?);
        *slot = Some(client.clone());
        Ok(client)
    }

    /// The current client if one is connected, without connecting.
    async fn current(&self) -> Option<Arc<McpClient>> {
        self.client.lock().await.clone()
    }
}

pub struct McpManager {
    servers: Vec<Arc<McpServer>>,
}

impl McpManager {
    /// `log_dir` receives each stdio server's stderr as `<name>.log`.
    pub fn new(configs: Vec<McpServerConfig>, log_dir: Option<PathBuf>) -> Self {
        Self {
            servers: configs
                .into_iter()
                .map(|c| Arc::new(McpServer::new(c, log_dir.clone())))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Connects every server concurrently and registers its tools. A
    /// failing server is reported and skipped; the others still connect.
    pub async fn connect_all(
        &self,
        sink: &dyn ToolSink,
        on_status: &(dyn Fn(ServerStatus) + Sync),
    ) {
        let attempts = self.servers.iter().map(|server| async move {
            let result = async {
                let client = server.client().await?;
                register_tools(server, &client, sink).await
            }
            .await;
            on_status(match result {
                Ok(tools) => ServerStatus::Connected {
                    server: server.name().to_string(),
                    tools,
                },
                Err(err) => ServerStatus::Failed {
                    server: server.name().to_string(),
                    error: err.to_string(),
                },
            });
        });
        join_all(attempts).await;
    }

    /// Re-lists the tools of every connected server that announced a
    /// change (`notifications/tools/list_changed`). Cheap when nothing
    /// changed: one atomic flag per server.
    pub async fn refresh_changed(&self, sink: &dyn ToolSink) {
        for server in &self.servers {
            let Some(client) = server.current().await else {
                continue;
            };
            if client.take_tools_changed()
                && let Err(err) = register_tools(server, &client, sink).await
            {
                tracing::warn!(server = server.name(), %err, "failed to refresh mcp tools");
            }
        }
    }
}

async fn register_tools(
    server: &Arc<McpServer>,
    client: &McpClient,
    sink: &dyn ToolSink,
) -> Result<usize, McpError> {
    let tools = client.list_tools().await?;
    let count = tools.len();
    let executors = tools
        .into_iter()
        .map(|tool| {
            let name = qualified_tool_name(server.name(), &tool.name);
            let executor: Arc<dyn ToolExecutor> =
                Arc::new(McpToolExecutor::new(server.clone(), tool));
            (name, executor)
        })
        .collect();
    sink.replace_server_tools(server.name(), executors);
    Ok(count)
}

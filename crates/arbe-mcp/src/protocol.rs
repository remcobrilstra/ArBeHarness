//! JSON-RPC 2.0 framing for MCP: building requests/notifications and
//! classifying incoming messages. Pure — no I/O — so it's tested without a
//! process or network.

use serde_json::{Value, json};
use thiserror::Error;

/// The protocol version this client speaks. Servers answer `initialize`
/// with the version they'll use; this client accepts whatever they choose.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, Error)]
pub enum McpError {
    #[error("failed to parse MCP message: {0}")]
    Parse(String),
    #[error("MCP server returned an error (code {code}): {message}")]
    Rpc { code: i64, message: String },
    #[error("MCP transport error: {0}")]
    Transport(String),
    #[error("MCP request timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("MCP request was cancelled")]
    Cancelled,
}

pub fn build_request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn build_notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A reply to a request the *server* sent us.
pub fn build_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn build_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;

/// One message from the server, classified.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to one of our requests.
    Response {
        id: u64,
        outcome: Result<Value, (i64, String)>,
    },
    /// A notification (no reply expected).
    Notification { method: String, params: Value },
    /// A request from the server to us (e.g. `ping`); must be answered.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

/// Classifies one JSON-RPC message.
pub fn classify(text: &str) -> Result<Incoming, McpError> {
    let value: Value = serde_json::from_str(text).map_err(|e| McpError::Parse(e.to_string()))?;
    classify_value(value)
}

pub fn classify_value(value: Value) -> Result<Incoming, McpError> {
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = value.get("id").filter(|id| !id.is_null()).cloned();
    match (method, id) {
        (Some(method), Some(id)) => Ok(Incoming::Request {
            id,
            method,
            params: value.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(method), None) => Ok(Incoming::Notification {
            method,
            params: value.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let id = id
                .as_u64()
                .ok_or_else(|| McpError::Parse(format!("unexpected response id {id}")))?;
            let outcome = if let Some(error) = value.get("error") {
                Err((
                    error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown MCP error")
                        .to_string(),
                ))
            } else {
                Ok(value.get("result").cloned().ok_or_else(|| {
                    McpError::Parse("response has neither result nor error".into())
                })?)
            };
            Ok(Incoming::Response { id, outcome })
        }
        (None, None) => Err(McpError::Parse("message has neither method nor id".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_well_formed_requests_and_notifications() {
        let req = build_request(1, "tools/list", json!({}));
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "tools/list");
        let notif = build_notification("notifications/initialized", json!({}));
        assert!(notif.get("id").is_none());
    }

    #[test]
    fn classifies_a_successful_response() {
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#).unwrap(),
            Incoming::Response {
                id: 1,
                outcome: Ok(json!({"tools": []}))
            }
        );
    }

    #[test]
    fn classifies_a_server_side_error() {
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"not found"}}"#)
                .unwrap(),
            Incoming::Response {
                id: 7,
                outcome: Err((-32601, "not found".into()))
            }
        );
    }

    #[test]
    fn classifies_notifications_and_server_requests() {
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#).unwrap(),
            Incoming::Notification {
                method: "notifications/tools/list_changed".into(),
                params: Value::Null
            }
        );
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap(),
            Incoming::Request {
                id: json!("abc"),
                method: "ping".into(),
                params: Value::Null
            }
        );
    }

    #[test]
    fn rejects_malformed_messages() {
        assert!(matches!(classify("not json"), Err(McpError::Parse(_))));
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0"}"#),
            Err(McpError::Parse(_))
        ));
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","id":1}"#),
            Err(McpError::Parse(_))
        ));
    }
}

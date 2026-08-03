use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to parse MCP message: {0}")]
    Parse(String),
    #[error("MCP server returned an error (code {code}): {message}")]
    Rpc { code: i64, message: String },
    #[error("MCP response id mismatch: expected {expected}, got {got}")]
    IdMismatch { expected: u64, got: u64 },
    #[error("MCP transport error: {0}")]
    Transport(String),
}

/// Builds a JSON-RPC 2.0 request. MCP's stdio transport frames each message
/// as one JSON value per line (no `Content-Length` headers like LSP uses).
pub fn build_request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Builds a JSON-RPC 2.0 notification (no `id`, no response expected) —
/// used for `notifications/initialized` after the handshake.
pub fn build_notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Parses one response line and returns its `result`, or an `McpError` if
/// the server reported an error or the id doesn't match what we sent.
pub fn parse_response(line: &str, expected_id: u64) -> Result<Value, McpError> {
    let value: Value = serde_json::from_str(line).map_err(|e| McpError::Parse(e.to_string()))?;

    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown MCP error")
            .to_string();
        return Err(McpError::Rpc { code, message });
    }

    let id = value
        .get("id")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| McpError::Parse("response missing id".to_string()))?;
    if id != expected_id {
        return Err(McpError::IdMismatch {
            expected: expected_id,
            got: id,
        });
    }

    value
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Parse("response missing result".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_well_formed_request() {
        let req = build_request(1, "tools/list", json!({}));
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "tools/list");
    }

    #[test]
    fn builds_a_notification_with_no_id() {
        let notif = build_notification("notifications/initialized", json!({}));
        assert!(notif.get("id").is_none());
        assert_eq!(notif["method"], "notifications/initialized");
    }

    #[test]
    fn parses_a_successful_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let result = parse_response(line, 1).unwrap();
        assert_eq!(result, json!({"tools": []}));
    }

    #[test]
    fn parses_a_server_side_error() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"not found"}}"#;
        let Err(err) = parse_response(line, 1) else {
            panic!("expected an error");
        };
        assert!(matches!(err, McpError::Rpc { code: -32601, .. }));
    }

    #[test]
    fn rejects_a_response_with_the_wrong_id() {
        let line = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
        let Err(err) = parse_response(line, 1) else {
            panic!("expected an error");
        };
        assert!(matches!(
            err,
            McpError::IdMismatch {
                expected: 1,
                got: 2
            }
        ));
    }

    #[test]
    fn rejects_malformed_json() {
        let Err(err) = parse_response("not json", 1) else {
            panic!("expected an error");
        };
        assert!(matches!(err, McpError::Parse(_)));
    }
}

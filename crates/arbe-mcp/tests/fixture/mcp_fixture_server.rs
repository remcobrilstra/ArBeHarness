//! A tiny MCP server over stdio, used only by arbe-mcp's integration tests
//! (v2 plan P7.3). Deliberately a little awkward in ways real servers are:
//! it prints a non-protocol line on startup, logs to stderr, answers
//! `tools/call`s on their own threads (so responses can come back out of
//! order), and sends a notification before answering `tools/list`.
//!
//! Tools: `echo {text}`, `add {a, b}`, `slow {ms}`, `fail`, `crash`
//! (exits the process), `toggle` (adds/removes a `bonus` tool and sends
//! `notifications/tools/list_changed`).

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

fn send(out: &Mutex<std::io::Stdout>, message: &Value) {
    let mut out = out.lock().unwrap();
    writeln!(out, "{message}").unwrap();
    out.flush().unwrap();
}

fn text(t: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": t.into() }] })
}

fn tools(bonus: bool) -> Value {
    let mut list = vec![
        json!({"name": "echo", "description": "Echo text back",
               "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
               "annotations": {"readOnlyHint": true}}),
        json!({"name": "add", "description": "Add two numbers",
               "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}}),
        json!({"name": "slow", "description": "Sleep for ms milliseconds",
               "inputSchema": {"type": "object", "properties": {"ms": {"type": "integer"}}}}),
        json!({"name": "fail", "description": "Always reports an error"}),
        json!({"name": "crash", "description": "Exits the server", "annotations": {"destructiveHint": true}}),
        json!({"name": "toggle", "description": "Adds or removes the bonus tool"}),
    ];
    if bonus {
        list.push(json!({"name": "bonus", "description": "Only here after toggle"}));
    }
    Value::Array(list)
}

fn main() {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let bonus = Arc::new(AtomicBool::new(false));
    eprintln!("fixture server starting");
    // Real servers sometimes print noise to stdout; clients must cope.
    send(&out, &json!("not a JSON-RPC message"));

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let id = message.get("id").cloned();
        let Some(id) = id else {
            // Notifications (initialized, cancelled): nothing to answer.
            eprintln!("notification: {method}");
            continue;
        };
        let params = message["params"].clone();
        match method.as_str() {
            "initialize" => send(
                &out,
                &json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": params["protocolVersion"],
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "fixture", "version": "0"}
                }}),
            ),
            "tools/list" => {
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "method": "notifications/message",
                            "params": {"level": "info", "data": "listing"}}),
                );
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools(bonus.load(Ordering::SeqCst))}}),
                );
            }
            "tools/call" => {
                let out = out.clone();
                let bonus = bonus.clone();
                std::thread::spawn(move || {
                    let args = &params["arguments"];
                    let result = match params["name"].as_str().unwrap_or_default() {
                        "echo" => text(args["text"].as_str().unwrap_or_default()),
                        "add" => text(format!(
                            "{}",
                            args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0)
                        )),
                        "slow" => {
                            std::thread::sleep(Duration::from_millis(
                                args["ms"].as_u64().unwrap_or(0),
                            ));
                            text("done")
                        }
                        "fail" => {
                            json!({"content": [{"type": "text", "text": "it failed"}], "isError": true})
                        }
                        "crash" => std::process::exit(3),
                        "toggle" => {
                            bonus.fetch_xor(true, Ordering::SeqCst);
                            send(
                                &out,
                                &json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                            );
                            text("toggled")
                        }
                        other => {
                            send(
                                &out,
                                &json!({"jsonrpc": "2.0", "id": id,
                                               "error": {"code": -32602, "message": format!("unknown tool {other}")}}),
                            );
                            return;
                        }
                    };
                    send(&out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                });
            }
            other => send(
                &out,
                &json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": format!("unknown method {other}")}}),
            ),
        }
    }
}

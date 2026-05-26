use std::path::PathBuf;

use agent_pty::{http::handle_http_request, mcp::handle_mcp_message, session::SessionManager};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn http_json_transport_handles_agent_terminal_requests() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    let new_body = json!({
        "op": "new",
        "id": "httpy",
        "repo": temp.path(),
        "shell": "/bin/sh",
        "rows": 24,
        "cols": 80,
        "env": {}
    });
    let response = handle_http_request(
        &manager,
        &format!(
            "POST /request HTTP/1.1\r\ncontent-length: {}\r\n\r\n{}",
            new_body.to_string().len(),
            new_body
        ),
    )
    .unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("session_created"));

    let send_body = json!({
        "op": "send",
        "id": "httpy",
        "text": "printf 'http-ok\\n'",
        "enter": true
    });
    handle_http_request(
        &manager,
        &format!(
            "POST /request HTTP/1.1\r\ncontent-length: {}\r\n\r\n{}",
            send_body.to_string().len(),
            send_body
        ),
    )
    .unwrap();

    let wait_body = json!({
        "op": "wait",
        "id": "httpy",
        "until": "http-ok",
        "timeout_ms": 3000
    });
    let response = handle_http_request(
        &manager,
        &format!(
            "POST /request HTTP/1.1\r\ncontent-length: {}\r\n\r\n{}",
            wait_body.to_string().len(),
            wait_body
        ),
    )
    .unwrap();
    assert!(response.contains("http-ok"));

    manager.kill("httpy").unwrap();
}

#[test]
fn mcp_stdio_handler_exposes_terminal_tools_and_proof() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    let list = handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .unwrap();
    assert!(
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "terminal.proof")
    );
    assert!(
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "terminal.approve")
    );

    handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "terminal.new",
                "arguments": {
                    "id": "mcpty",
                    "repo": temp.path(),
                    "shell": PathBuf::from("/bin/sh"),
                    "rows": 24,
                    "cols": 80,
                    "env": {}
                }
            }
        }),
    )
    .unwrap();

    handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "terminal.send",
                "arguments": {
                    "id": "mcpty",
                    "text": "printf 'mcp-ok\\n'",
                    "enter": true
                }
            }
        }),
    )
    .unwrap();

    let waited = handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "terminal.wait",
                "arguments": {
                    "id": "mcpty",
                    "until": "mcp-ok",
                    "timeout_ms": 3000
                }
            }
        }),
    )
    .unwrap();
    assert!(waited.to_string().contains("mcp-ok"));

    let approved_command = "printf 'mcp-approved\\n' # vault write";
    let approval = handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "terminal.approve",
                "arguments": {
                    "id": "mcpty",
                    "command": approved_command,
                    "rule": "vault write",
                    "ttl_ms": 60000
                }
            }
        }),
    )
    .unwrap();
    let token = approval["result"]["structuredContent"]["data"]["token"]
        .as_str()
        .unwrap();
    handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "terminal.send",
                "arguments": {
                    "id": "mcpty",
                    "text": approved_command,
                    "enter": true,
                    "approval": token
                }
            }
        }),
    )
    .unwrap();
    let approved = handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "terminal.wait",
                "arguments": {
                    "id": "mcpty",
                    "until": "mcp-approved",
                    "timeout_ms": 3000
                }
            }
        }),
    )
    .unwrap();
    assert!(approved.to_string().contains("mcp-approved"));

    let proof = handle_mcp_message(
        &manager,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "terminal.proof",
                "arguments": { "id": "mcpty" }
            }
        }),
    )
    .unwrap();
    assert!(proof.to_string().contains("commands_run"));

    manager.kill("mcpty").unwrap();
}

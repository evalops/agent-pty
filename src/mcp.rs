use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    daemon::{Request, ResponsePayload, handle_request},
    session::SessionManager,
};

pub fn handle_mcp_message(manager: &SessionManager, message: Value) -> Result<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .context("MCP message missing method")?;

    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "serverInfo": { "name": "agent-pty", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "tools": {} }
        }),
        "tools/list" => json!({ "tools": tool_descriptors() }),
        "tools/call" => {
            let params = message.get("params").context("tools/call missing params")?;
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .context("tools/call missing name")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let payload = call_tool(manager, name, arguments)?;
            json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&payload)?
                    }
                ],
                "structuredContent": payload
            })
        }
        _ => bail!("unsupported MCP method {method}"),
    };

    Ok(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    }))
}

pub fn serve_mcp_stdio(log_dir: impl Into<PathBuf>) -> Result<()> {
    let manager = SessionManager::new(log_dir)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line.context("read MCP stdin line")?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line).context("parse MCP JSON-RPC message")?;
        let response = match handle_mcp_message(&manager, message) {
            Ok(response) => response,
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32000, "message": format!("{error:#}") }
            }),
        };
        serde_json::to_writer(&mut stdout, &response).context("write MCP response")?;
        stdout.write_all(b"\n").context("write MCP newline")?;
        stdout.flush().context("flush MCP stdout")?;
    }

    Ok(())
}

fn call_tool(manager: &SessionManager, name: &str, arguments: Value) -> Result<ResponsePayload> {
    let request = match name {
        "terminal.new" => Request::New {
            id: string_arg(&arguments, "id")?,
            repo: path_arg(&arguments, "repo")?,
            shell: arguments
                .get("shell")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/bin/sh")),
            rows: u16_arg(&arguments, "rows", 24)?,
            cols: u16_arg(&arguments, "cols", 80)?,
            env: map_arg(&arguments, "env")?,
        },
        "terminal.send" => Request::Send {
            id: string_arg(&arguments, "id")?,
            text: string_arg(&arguments, "text")?,
            enter: bool_arg(&arguments, "enter", true),
        },
        "terminal.screen" => Request::Screen {
            id: string_arg(&arguments, "id")?,
        },
        "terminal.wait" => Request::Wait {
            id: string_arg(&arguments, "id")?,
            until: string_arg(&arguments, "until")?,
            timeout_ms: u64_arg(&arguments, "timeout_ms", 30_000)?,
        },
        "terminal.kill" => Request::Kill {
            id: string_arg(&arguments, "id")?,
        },
        "terminal.replay" => Request::Replay {
            id: string_arg(&arguments, "id")?,
        },
        "terminal.list" => Request::List,
        "terminal.proof" => Request::Proof {
            id: string_arg(&arguments, "id")?,
        },
        "terminal.fork" => Request::Fork {
            id: string_arg(&arguments, "id")?,
            name: string_arg(&arguments, "name")?,
            copy_worktree: bool_arg(&arguments, "copy_worktree", false),
        },
        _ => bail!("unknown MCP tool {name}"),
    };
    handle_request(manager, request)
}

fn tool_descriptors() -> Vec<Value> {
    [
        "terminal.new",
        "terminal.send",
        "terminal.screen",
        "terminal.wait",
        "terminal.kill",
        "terminal.replay",
        "terminal.list",
        "terminal.proof",
        "terminal.fork",
    ]
    .into_iter()
    .map(|name| {
        json!({
            "name": name,
            "description": format!("{name} for persistent agent terminal sessions"),
            "inputSchema": { "type": "object", "additionalProperties": true }
        })
    })
    .collect()
}

fn string_arg(arguments: &Value, key: &str) -> Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .with_context(|| format!("missing string argument {key}"))
}

fn path_arg(arguments: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(string_arg(arguments, key)?))
}

fn u16_arg(arguments: &Value, key: &str, default: u16) -> Result<u16> {
    let Some(value) = arguments.get(key) else {
        return Ok(default);
    };
    Ok(value
        .as_u64()
        .with_context(|| format!("argument {key} must be an integer"))? as u16)
}

fn u64_arg(arguments: &Value, key: &str, default: u64) -> Result<u64> {
    let Some(value) = arguments.get(key) else {
        return Ok(default);
    };
    value
        .as_u64()
        .with_context(|| format!("argument {key} must be an integer"))
}

fn bool_arg(arguments: &Value, key: &str, default: bool) -> bool {
    arguments
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn map_arg(arguments: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = arguments.get(key) else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .with_context(|| format!("argument {key} must be an object"))?;
    Ok(object
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect())
}

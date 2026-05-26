use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn runnable_agent_integration_examples_drive_real_sessions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new().unwrap();

    let cli = run_example(
        Command::new("bash")
            .arg(root.join("examples/cli_repair_agent.sh"))
            .env("AGENT_PTY_BIN", agent_pty_bin())
            .env("AGENT_PTY_ROOT", temp.path().join("cli")),
    );
    assert_eq!(cli["transport"], "cli");
    assert_path_exists(&cli["proof_html_path"]);

    let http = run_example(
        Command::new("python3")
            .arg(root.join("examples/http_agent.py"))
            .env("AGENT_PTY_BIN", agent_pty_bin())
            .env("AGENT_PTY_ROOT", temp.path().join("http")),
    );
    assert_eq!(http["transport"], "http");
    assert!(http["command"].as_str().unwrap().contains("printf"));
    assert_path_exists(&http["proof_html_path"]);

    let mcp = run_example(
        Command::new("python3")
            .arg(root.join("examples/mcp_stdio_agent.py"))
            .env("AGENT_PTY_BIN", agent_pty_bin())
            .env("AGENT_PTY_ROOT", temp.path().join("mcp")),
    );
    assert_eq!(mcp["transport"], "mcp");
    assert_eq!(mcp["server"], "agent-pty");
    assert!(mcp["tool_count"].as_u64().unwrap() >= 8);
    assert_path_exists(&mcp["proof_html_path"]);
}

fn run_example(command: &mut Command) -> Value {
    let output = command.output().expect("run integration example");
    assert!(
        output.status.success(),
        "example failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("example JSON output")
}

fn assert_path_exists(value: &Value) {
    let path = value.as_str().expect("path field is a string");
    assert!(Path::new(path).exists(), "{path} should exist");
}

fn agent_pty_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-pty"))
}

use std::{fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn demo_cli_creates_artifacts_and_exercises_a_real_session() {
    let temp = TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-pty"))
        .arg("demo")
        .arg("--root")
        .arg(temp.path())
        .arg("--name")
        .arg("smoke")
        .arg("--shell")
        .arg("/bin/sh")
        .arg("--json")
        .output()
        .expect("run agent-pty demo");

    assert!(
        output.status.success(),
        "demo failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["session_id"], "smoke");
    assert_path_exists(&payload["workspace"]);
    assert_path_exists(&payload["event_log_path"]);
    assert_path_exists(&payload["proof_json_path"]);
    assert_path_exists(&payload["proof_markdown_path"]);
    assert_path_exists(&payload["proof_html_path"]);
    assert_path_exists(&payload["report_path"]);

    let workspace = Path::new(payload["workspace"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(workspace.join("answer.txt"))
            .unwrap()
            .trim(),
        "fixed"
    );

    let proof = fs::read_to_string(payload["proof_markdown_path"].as_str().unwrap()).unwrap();
    assert!(proof.contains("agent-pty-demo-success"));
    assert!(proof.contains("demo checks passed"));
    let proof_html = fs::read_to_string(payload["proof_html_path"].as_str().unwrap()).unwrap();
    assert!(proof_html.contains("agent-pty proof: smoke"));
    assert!(proof_html.contains("agent-pty-demo-success"));

    let report = fs::read_to_string(payload["report_path"].as_str().unwrap()).unwrap();
    assert!(report.contains("agent-pty demo complete"));
    assert!(report.contains("demo checks passed"));
    assert!(report.contains("proof html:"));
}

fn assert_path_exists(value: &Value) {
    let path = value.as_str().expect("path field is a string");
    assert!(Path::new(path).exists(), "{path} should exist");
}

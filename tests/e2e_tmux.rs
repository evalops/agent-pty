use std::{fs, path::PathBuf, process::Command};

#[test]
#[ignore = "runs a real tmux black-box harness with daemon processes and artifact capture"]
fn tmux_black_box_harness_exercises_agent_pty_end_to_end() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let artifacts = root.join("target/e2e-tmux/latest");

    let output = Command::new(root.join("scripts/e2e-tmux.sh"))
        .arg("--ci")
        .arg("--artifacts")
        .arg(&artifacts)
        .current_dir(&root)
        .output()
        .expect("run scripts/e2e-tmux.sh");

    assert!(
        output.status.success(),
        "tmux harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report = fs::read_to_string(artifacts.join("report.md")).unwrap();
    assert!(report.contains("Unix CLI: ok"));
    assert!(report.contains("HTTP: ok"));
    assert!(report.contains("MCP: ok"));
    assert!(report.contains("tmux capture: ok"));
    assert!(report.contains("policy denial: ok"));
    assert!(report.contains("human attach: ok"));
    assert!(report.contains("daemon restart replay: ok"));
    assert!(report.contains("fork comparison: ok"));

    let proof = fs::read_to_string(artifacts.join("proofs/fix-a.proof.md")).unwrap();
    assert!(proof.contains("cargo test"));
    assert!(proof.contains("test result: ok"));
    assert!(proof.contains("tests passed"));

    let capture = fs::read_to_string(artifacts.join("tmux-driver-pane.txt")).unwrap();
    assert!(capture.contains("agent-pty-e2e-complete"));
}

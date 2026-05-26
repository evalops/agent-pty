use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn lifecycle_cli_reports_and_stops_a_real_daemon() {
    let temp = TempDir::new().unwrap();
    let socket = temp.path().join("agent-pty.sock");
    let log_dir = temp.path().join("logs");
    let mut daemon = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(&socket)
        .arg("serve")
        .arg("--log-dir")
        .arg(&log_dir)
        .spawn()
        .expect("start agent-pty daemon");

    let result = catch_unwind(AssertUnwindSafe(|| run_lifecycle_flow(&socket, &log_dir)));
    cleanup_daemon(&mut daemon);
    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

fn run_lifecycle_flow(socket: &Path, log_dir: &Path) {
    wait_for_socket(socket);

    let status = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("status")
        .arg("--json")
        .output()
        .expect("run agent-pty status");
    assert!(
        status.status.success(),
        "status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["running"], true);
    assert_eq!(status["session_count"], 0);
    assert!(
        status["trace_path"]
            .as_str()
            .unwrap()
            .ends_with("traces.jsonl")
    );

    let stop = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("stop")
        .output()
        .expect("run agent-pty stop");
    assert!(
        stop.status.success(),
        "stop failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(String::from_utf8_lossy(&stop.stdout).contains("stopped"));
    wait_for_socket_removed(socket);

    let stopped = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("status")
        .arg("--json")
        .output()
        .expect("run agent-pty status after stop");
    assert!(stopped.status.success());
    let stopped: Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(stopped["running"], false);
    assert!(!stopped["error"].as_str().unwrap().is_empty());

    let doctor = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("doctor")
        .arg("--log-dir")
        .arg(log_dir)
        .arg("--json")
        .output()
        .expect("run agent-pty doctor");
    assert!(
        doctor.status.success(),
        "doctor failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["ok"], true);
    assert_eq!(doctor["daemon_running"], false);
    assert!(
        doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "git" && check["ok"] == true)
    );
}

fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if socket.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear: {}", socket.display());
}

fn wait_for_socket_removed(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !socket.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket was not removed: {}", socket.display());
}

fn cleanup_daemon(child: &mut Child) {
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn agent_pty_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-pty"))
}

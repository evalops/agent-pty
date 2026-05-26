use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn unix_daemon_returns_large_json_responses_without_truncation() {
    let temp = TempDir::new().unwrap();
    let socket = temp.path().join("agent-pty.sock");
    let log_dir = temp.path().join("logs");
    let mut daemon = start_daemon(&socket, &log_dir);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_socket(&socket);
        run_ok(
            Command::new(agent_pty_bin())
                .arg("--socket")
                .arg(&socket)
                .arg("new")
                .arg("--repo")
                .arg(temp.path())
                .arg("--name")
                .arg("large-response")
                .arg("--shell")
                .arg("/bin/sh"),
        );
        run_ok(
            Command::new(agent_pty_bin())
                .arg("--socket")
                .arg(&socket)
                .arg("send")
                .arg("large-response")
                .arg("printf '%012000d\\n' 0"),
        );
        run_ok(
            Command::new(agent_pty_bin())
                .arg("--socket")
                .arg(&socket)
                .arg("wait")
                .arg("large-response")
                .arg("--until")
                .arg("000000")
                .arg("--timeout")
                .arg("5s"),
        );
        let replay = Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(&socket)
            .arg("replay")
            .arg("large-response")
            .arg("--json")
            .output()
            .expect("run large replay");
        assert!(
            replay.status.success(),
            "large replay failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&replay.stdout),
            String::from_utf8_lossy(&replay.stderr)
        );
        let events: Value = serde_json::from_slice(&replay.stdout).expect("large replay JSON");
        assert!(events.to_string().len() > 10_000);
    }));
    cleanup_daemon(&mut daemon, &socket);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn start_daemon(socket: &Path, log_dir: &Path) -> Child {
    Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("serve")
        .arg("--log-dir")
        .arg(log_dir)
        .spawn()
        .expect("start agent-pty daemon")
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

fn cleanup_daemon(child: &mut Child, socket: &Path) {
    let _ = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("stop")
        .output();
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run_ok(command: &mut Command) {
    let output = command.output().expect("run command");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn agent_pty_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-pty"))
}

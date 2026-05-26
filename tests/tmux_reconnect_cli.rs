use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn tmux_backend_reconnects_to_live_session_after_daemon_restart() {
    if !tmux_available() {
        eprintln!("skipping tmux reconnect test because tmux is unavailable");
        return;
    }

    let temp = TempDir::new().unwrap();
    let socket = temp.path().join("agent-pty.sock");
    let log_dir = temp.path().join("logs");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut daemon = start_daemon(&socket, &log_dir);
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_reconnect_flow(&socket, &log_dir, &workspace)
    }));
    cleanup_daemon(&mut daemon, &socket);
    cleanup_tmux("agent-pty-reconnect");
    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

fn run_reconnect_flow(socket: &Path, log_dir: &Path, workspace: &Path) {
    wait_for_socket(socket);
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("new")
            .arg("--repo")
            .arg(workspace)
            .arg("--name")
            .arg("agent-pty-reconnect")
            .arg("--shell")
            .arg("/bin/sh")
            .arg("--backend")
            .arg("tmux"),
    );

    let list = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("list")
        .arg("--json")
        .output()
        .expect("list sessions");
    let sessions: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(sessions[0]["backend"], "tmux");
    assert_eq!(sessions[0]["active"], true);

    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("send")
            .arg("agent-pty-reconnect")
            .arg("printf 'before-restart\\n'; (while [ ! -f resume.flag ]; do sleep 0.1; done; printf 'after-restart\\n') &"),
    );
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("wait")
            .arg("agent-pty-reconnect")
            .arg("--until")
            .arg("before-restart")
            .arg("--timeout")
            .arg("5s"),
    );

    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("stop"),
    );
    wait_for_socket_removed(socket);
    std::fs::write(workspace.join("resume.flag"), "go\n").unwrap();

    let mut daemon = start_daemon(socket, log_dir);
    wait_for_socket(socket);
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("wait")
            .arg("agent-pty-reconnect")
            .arg("--until")
            .arg("after-restart")
            .arg("--timeout")
            .arg("5s"),
    );
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("send")
            .arg("agent-pty-reconnect")
            .arg("printf 'post-reconnect\\n'"),
    );
    let screen = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("screen")
        .arg("agent-pty-reconnect")
        .output()
        .expect("screen after reconnect");
    assert!(String::from_utf8_lossy(&screen.stdout).contains("post-reconnect"));

    let attach = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("attach")
        .arg("agent-pty-reconnect")
        .arg("--read-only")
        .arg("--timeout")
        .arg("2s")
        .arg("--history-bytes")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("attach to reconnected tmux session");
    thread::sleep(Duration::from_millis(150));
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("send")
            .arg("agent-pty-reconnect")
            .arg("printf 'attach-after-reconnect\\n'"),
    );
    let attach_output = attach.wait_with_output().expect("wait for attach output");
    assert!(
        attach_output.status.success(),
        "attach failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&attach_output.stdout),
        String::from_utf8_lossy(&attach_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&attach_output.stdout).contains("attach-after-reconnect"),
        "attach did not stream post-reconnect output\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&attach_output.stdout),
        String::from_utf8_lossy(&attach_output.stderr)
    );

    cleanup_daemon(&mut daemon, socket);
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

fn cleanup_tmux(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .output();
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

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn agent_pty_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-pty"))
}

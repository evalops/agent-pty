use std::{
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[test]
fn attach_streams_output_and_can_send_input_to_a_real_session() {
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

    let result = catch_unwind(AssertUnwindSafe(|| run_attach_flow(temp.path(), &socket)));
    cleanup_daemon(&mut daemon, &socket);
    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

#[test]
fn attach_unknown_session_reports_a_daemon_error() {
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

    let result = catch_unwind(AssertUnwindSafe(|| {
        wait_for_socket(&socket);
        let output = Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(&socket)
            .arg("attach")
            .arg("missing")
            .arg("--read-only")
            .arg("--timeout")
            .arg("100ms")
            .output()
            .expect("attach missing session");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown session missing"));
    }));
    cleanup_daemon(&mut daemon, &socket);
    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

fn run_attach_flow(workspace: &Path, socket: &Path) {
    wait_for_socket(socket);
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("new")
            .arg("--repo")
            .arg(workspace)
            .arg("--name")
            .arg("attachy")
            .arg("--shell")
            .arg("/bin/sh"),
    );

    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("send")
            .arg("attachy")
            .arg("printf 'before-attach\\n'; printf 'waiting-for-attach> '; read line; printf 'got:%s\\n' \"$line\""),
    );
    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("wait")
            .arg("attachy")
            .arg("--until")
            .arg("waiting-for-attach>")
            .arg("--timeout")
            .arg("5s"),
    );

    let read_only = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("attach")
        .arg("attachy")
        .arg("--read-only")
        .arg("--timeout")
        .arg("250ms")
        .output()
        .expect("run read-only attach");
    assert!(
        read_only.status.success(),
        "read-only attach failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_only.stdout),
        String::from_utf8_lossy(&read_only.stderr)
    );
    let read_only_out = String::from_utf8_lossy(&read_only.stdout);
    assert!(read_only_out.contains("before-attach"));
    assert!(read_only_out.contains("waiting-for-attach>"));

    let mut attach = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("attach")
        .arg("attachy")
        .arg("--timeout")
        .arg("1s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn writable attach");
    attach
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"typed-through-attach\n")
        .unwrap();
    let output = attach.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "writable attach failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("got:typed-through-attach"));

    let replay = Command::new(agent_pty_bin())
        .arg("--socket")
        .arg(socket)
        .arg("replay")
        .arg("attachy")
        .output()
        .expect("replay attachy");
    let replay = String::from_utf8_lossy(&replay.stdout);
    assert!(replay.contains("attach human"));

    run_ok(
        Command::new(agent_pty_bin())
            .arg("--socket")
            .arg(socket)
            .arg("kill")
            .arg("attachy"),
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

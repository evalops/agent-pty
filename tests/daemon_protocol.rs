use std::{path::PathBuf, time::Duration};

use agent_pty::{
    daemon::{Request, ResponsePayload, handle_request},
    session::SessionManager,
};
use tempfile::TempDir;

#[test]
fn daemon_request_flow_controls_session_and_replays_events() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    let created = handle_request(
        &manager,
        Request::New {
            id: "codex-daemon".to_string(),
            repo: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            rows: 24,
            cols: 80,
            env: Default::default(),
        },
    )
    .unwrap();
    assert!(matches!(created, ResponsePayload::SessionCreated { .. }));

    handle_request(
        &manager,
        Request::Send {
            id: "codex-daemon".to_string(),
            text: "printf 'daemon-ok\\n'".to_string(),
            enter: true,
        },
    )
    .unwrap();

    let waited = handle_request(
        &manager,
        Request::Wait {
            id: "codex-daemon".to_string(),
            until: "daemon-ok".to_string(),
            timeout_ms: 3_000,
        },
    )
    .unwrap();
    let ResponsePayload::Observation(observation) = waited else {
        panic!("expected observation response");
    };
    assert!(observation.screen.text.contains("daemon-ok"));

    let replay = handle_request(
        &manager,
        Request::Replay {
            id: "codex-daemon".to_string(),
        },
    )
    .unwrap();
    let ResponsePayload::Replay(events) = replay else {
        panic!("expected replay response");
    };
    assert!(events.len() >= 3);

    handle_request(
        &manager,
        Request::Wait {
            id: "codex-daemon".to_string(),
            until: "idle:50ms".to_string(),
            timeout_ms: Duration::from_secs(3).as_millis() as u64,
        },
    )
    .unwrap();

    handle_request(
        &manager,
        Request::Kill {
            id: "codex-daemon".to_string(),
        },
    )
    .unwrap();
}

#[test]
fn daemon_rejects_dangerous_commands_before_they_reach_the_pty() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    handle_request(
        &manager,
        Request::New {
            id: "policy".to_string(),
            repo: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            rows: 24,
            cols: 80,
            env: Default::default(),
        },
    )
    .unwrap();

    let error = handle_request(
        &manager,
        Request::Send {
            id: "policy".to_string(),
            text: "rm -rf /tmp/agent-pty-policy-test".to_string(),
            enter: true,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("requires approval"));

    handle_request(
        &manager,
        Request::Kill {
            id: "policy".to_string(),
        },
    )
    .unwrap();
}

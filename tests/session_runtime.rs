use std::{path::PathBuf, time::Duration};

use agent_pty::{
    evidence::{Action, EventKind},
    session::{SessionConfig, SessionManager, WaitCondition},
};
use tempfile::TempDir;

#[test]
fn pty_session_sends_input_reads_screen_and_records_evidence() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();
    let session_id = "codex-1";

    manager
        .open(SessionConfig {
            id: session_id.to_string(),
            workspace: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            env: Default::default(),
            rows: 24,
            cols: 80,
        })
        .unwrap();

    manager
        .send_line(session_id, "printf 'agent-pty-ok\\n'")
        .unwrap();
    let observation = manager
        .wait(
            session_id,
            WaitCondition::Regex {
                pattern: "agent-pty-ok".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();

    assert!(observation.screen.text.contains("agent-pty-ok"));
    assert!(
        observation
            .screen
            .spans
            .iter()
            .any(|span| span.text.contains("agent-pty-ok"))
    );

    let events = manager.replay(session_id).unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::Action(Action::SendKeys { bytes }) if bytes.ends_with(b"\n")
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::Observation(observation)
                if observation
                    .screen_text
                    .as_deref()
                    .is_some_and(|screen| screen.contains("agent-pty-ok"))
        )
    }));

    manager.kill(session_id).unwrap();
}

#[test]
fn pty_session_waits_for_idle_and_process_exit() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();
    let session_id = "codex-2";

    manager
        .open(SessionConfig {
            id: session_id.to_string(),
            workspace: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            env: Default::default(),
            rows: 24,
            cols: 80,
        })
        .unwrap();

    manager
        .send_line(session_id, "printf 'settled\\n'")
        .unwrap();
    let idle_observation = manager
        .wait(
            session_id,
            WaitCondition::Idle {
                quiet_for: Duration::from_millis(50),
            },
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(idle_observation.stdout_tail.contains("settled"));

    manager.send_line(session_id, "exit 7").unwrap();
    let exit_observation = manager
        .wait(session_id, WaitCondition::Exit, Duration::from_secs(3))
        .unwrap();
    assert_eq!(exit_observation.exit_status, Some(7));
}

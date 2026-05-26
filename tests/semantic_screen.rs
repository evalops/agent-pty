use std::{path::PathBuf, thread, time::Duration};

use agent_pty::session::{ScreenSpan, SessionBackend, SessionConfig, SessionManager};
use tempfile::TempDir;

#[test]
fn screen_snapshot_exposes_semantic_summary_for_agent_reading() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    manager
        .open(SessionConfig {
            id: "semantic".to_string(),
            workspace: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            env: Default::default(),
            rows: 24,
            cols: 120,
            backend: SessionBackend::Pty,
        })
        .unwrap();

    manager
        .send_line(
            "semantic",
            "printf 'error: failed at src/main.rs:12\\nsee http://localhost:3000/build\\n'",
        )
        .unwrap();
    manager
        .wait(
            "semantic",
            agent_pty::session::WaitCondition::Regex {
                pattern: "localhost:3000".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();

    let screen = manager.screen("semantic").unwrap();
    assert!(
        screen.semantic.command.as_deref().is_some_and(|command| {
            command.contains("printf") && command.contains("src/main.rs")
        })
    );
    assert!(
        screen
            .semantic
            .error_lines
            .iter()
            .any(|line| line_text(&screen.spans, *line).contains("failed"))
    );
    assert!(
        screen
            .semantic
            .urls
            .contains(&"http://localhost:3000/build".to_string())
    );
    assert!(
        screen
            .semantic
            .file_paths
            .contains(&"src/main.rs:12".to_string())
    );

    manager.send_line("semantic", "sleep 2").unwrap();
    let running = wait_for_active_command(&manager, "semantic", "sleep 2");
    assert!(
        running
            .semantic
            .active_process
            .as_ref()
            .is_some_and(|process| process.command.contains("sleep 2"))
    );

    manager.kill("semantic").unwrap();
}

fn wait_for_active_command(
    manager: &SessionManager,
    session_id: &str,
    command: &str,
) -> agent_pty::session::ScreenSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let screen = manager.screen(session_id).unwrap();
        if screen
            .semantic
            .active_process
            .as_ref()
            .is_some_and(|process| process.command.contains(command))
        {
            return screen;
        }
        thread::sleep(Duration::from_millis(25));
    }
    manager.screen(session_id).unwrap()
}

fn line_text(spans: &[ScreenSpan], line: usize) -> String {
    spans
        .iter()
        .find(|span| span.line == line)
        .map(|span| span.text.clone())
        .unwrap_or_default()
}

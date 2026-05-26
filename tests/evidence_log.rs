use std::{fs, process::Command, sync::Arc, thread};

use agent_pty::evidence::{Action, EventKind, EventLog, GitSnapshot, Observation};
use tempfile::TempDir;

#[test]
fn event_log_replays_appended_events_in_order() {
    let temp = TempDir::new().unwrap();
    let log = EventLog::open(temp.path().join("run.jsonl")).unwrap();

    let first = log
        .append_action(
            "codex-1",
            Action::SendKeys {
                bytes: b"cargo test\n".to_vec(),
            },
            None,
        )
        .unwrap();
    let second = log
        .append_observation(
            "codex-1",
            Observation {
                screen_text: Some("running 12 tests".to_string()),
                stdout_tail: "running 12 tests".to_string(),
                stderr_tail: String::new(),
                exit_status: None,
                files_changed: vec![],
                git_snapshot: None,
            },
        )
        .unwrap();

    let events = log.replay().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, first.id);
    assert_eq!(events[1].id, second.id);
    assert!(matches!(events[0].kind, EventKind::Action(_)));
    assert!(matches!(events[1].kind, EventKind::Observation(_)));

    let raw = fs::read_to_string(temp.path().join("run.jsonl")).unwrap();
    assert_eq!(raw.lines().count(), 2);
}

#[test]
fn git_snapshot_reports_status_and_diff_for_a_dirty_repo() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init"]);
    fs::write(temp.path().join("README.md"), "hello\n").unwrap();
    run_git(temp.path(), &["add", "README.md"]);
    run_git(
        temp.path(),
        &[
            "-c",
            "user.name=Agent PTY",
            "-c",
            "user.email=agent-pty@example.test",
            "commit",
            "-m",
            "initial",
        ],
    );

    fs::write(temp.path().join("README.md"), "hello\nworld\n").unwrap();

    let snapshot = GitSnapshot::capture(temp.path()).unwrap();
    assert!(!snapshot.clean);
    assert!(snapshot.status_short.contains("M README.md"));
    assert!(snapshot.diff.contains("+world"));
}

#[test]
fn event_log_handles_concurrent_appenders_without_corrupting_jsonl() {
    let temp = TempDir::new().unwrap();
    let log = Arc::new(EventLog::open(temp.path().join("run.jsonl")).unwrap());

    let threads = (0..32)
        .map(|index| {
            let log = Arc::clone(&log);
            thread::spawn(move || {
                log.append_action(
                    "concurrent",
                    Action::SendKeys {
                        bytes: format!("echo {index}\n").into_bytes(),
                    },
                    None,
                )
                .unwrap();
            })
        })
        .collect::<Vec<_>>();

    for thread in threads {
        thread.join().unwrap();
    }

    let events = log.replay().unwrap();
    assert_eq!(events.len(), 32);
}

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

use std::{fs, path::PathBuf, process::Command, time::Duration};

use agent_pty::{
    daemon::{Request, handle_request},
    evidence::{Action, EventKind},
    session::{SessionBackend, SessionConfig, SessionManager, WaitCondition},
};
use tempfile::TempDir;

#[test]
fn manager_persists_session_metadata_and_writes_proof_bundle() {
    let repo = git_repo();
    let manager = SessionManager::new(repo.path().join(".agent-pty")).unwrap();

    manager
        .open(SessionConfig {
            id: "proofy".to_string(),
            workspace: repo.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            env: Default::default(),
            rows: 24,
            cols: 80,
            backend: SessionBackend::Pty,
        })
        .unwrap();
    manager.send_line("proofy", "printf 'proof-ok\\n'").unwrap();
    manager
        .wait(
            "proofy",
            WaitCondition::Regex {
                pattern: "proof-ok".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();

    let sessions = manager.list_sessions().unwrap();
    let metadata = sessions
        .iter()
        .find(|session| session.id == "proofy")
        .unwrap();
    assert!(metadata.active);
    assert_eq!(metadata.workspace, repo.path());
    assert!(metadata.process_id.is_some());

    let fresh_manager = SessionManager::new(repo.path().join(".agent-pty")).unwrap();
    let persisted = fresh_manager.list_sessions().unwrap();
    assert!(persisted.iter().any(|session| {
        session.id == "proofy" && !session.active && session.workspace == repo.path()
    }));

    let proof = manager.proof("proofy").unwrap();
    assert_eq!(proof.session_id, "proofy");
    assert!(
        proof
            .commands_run
            .iter()
            .any(|command| command.contains("printf"))
    );
    assert!(proof.screen_tail.contains("proof-ok"));
    assert!(proof.log_integrity.verified);
    assert_eq!(proof.log_integrity.event_count, proof.event_count);
    assert!(proof.event_count >= 4);
    assert!(proof.json_path.exists());
    assert!(proof.markdown_path.exists());
    assert!(proof.html_path.exists());
    assert!(
        fs::read_to_string(&proof.markdown_path)
            .unwrap()
            .contains("proof-ok")
    );
    let html = fs::read_to_string(&proof.html_path).unwrap();
    assert!(html.contains("agent-pty proof: proofy"));
    assert!(html.contains("proof-ok"));
    assert!(html.contains("Commands Run"));

    let observation = manager
        .wait(
            "proofy",
            WaitCondition::Idle {
                quiet_for: Duration::from_millis(20),
            },
            Duration::from_secs(3),
        )
        .unwrap();
    let process_snapshot = observation.process_snapshot.unwrap();
    assert!(process_snapshot.root_pid > 0);
    assert!(
        process_snapshot
            .processes
            .iter()
            .any(|process| process.pid == process_snapshot.root_pid)
    );

    manager.kill("proofy").unwrap();
}

#[test]
fn denied_policy_actions_are_recorded_as_evidence() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();

    handle_request(
        &manager,
        Request::New {
            id: "policy-audit".to_string(),
            repo: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            rows: 24,
            cols: 80,
            env: Default::default(),
            backend: SessionBackend::Pty,
        },
    )
    .unwrap();

    let error = handle_request(
        &manager,
        Request::Send {
            id: "policy-audit".to_string(),
            text: "terraform apply".to_string(),
            enter: true,
            approval: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("requires approval"));

    let events = manager.replay("policy-audit").unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::Action(Action::PolicyDenied { rule, command })
                if rule == "terraform apply" && command == "terraform apply"
        )
    }));

    let traces = fs::read_to_string(manager.trace_path()).unwrap();
    assert!(traces.contains("terminal.send"));
    assert!(traces.contains("\"status\":\"error\""));

    manager.kill("policy-audit").unwrap();
}

#[test]
fn fork_can_create_a_git_worktree_backed_session() {
    let repo = git_repo();
    let manager = SessionManager::new(repo.path().join(".agent-pty")).unwrap();

    manager
        .open(SessionConfig {
            id: "base".to_string(),
            workspace: repo.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            env: Default::default(),
            rows: 24,
            cols: 80,
            backend: SessionBackend::Pty,
        })
        .unwrap();

    let fork = manager.fork("base", "attempt-b", true).unwrap();
    assert_eq!(fork.id, "attempt-b");
    assert_ne!(fork.workspace, repo.path());
    assert!(fork.workspace.join(".git").exists() || fork.workspace.join("README.md").exists());

    manager
        .send_line("attempt-b", "printf 'fork-ok\\n'")
        .unwrap();
    let observation = manager
        .wait(
            "attempt-b",
            WaitCondition::Regex {
                pattern: "fork-ok".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(observation.screen.text.contains("fork-ok"));

    manager.kill("attempt-b").unwrap();
    manager.kill("base").unwrap();
}

fn git_repo() -> TempDir {
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
    temp
}

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

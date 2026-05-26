use std::fs;

use agent_pty::evidence::{Action, EventKind, EventLog, Observation, redact_text};
use tempfile::TempDir;

#[test]
fn evidence_logs_redact_secret_like_values_and_hash_each_event() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("secure.jsonl");
    let log = EventLog::open(&log_path).unwrap();

    log.append_action(
        "secure",
        Action::SendKeys {
            bytes: b"OPENAI_API_KEY=sk-testsecretvalue1234567890 cargo test\n".to_vec(),
        },
        None,
    )
    .unwrap();
    log.append_observation(
        "secure",
        Observation {
            screen_text: Some("token echoed: ghp_abcdefghijklmnopqrstuvwxyz123456".to_string()),
            stdout_tail: "AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF".to_string(),
            stderr_tail: "PASSWORD=hunter2".to_string(),
            exit_status: None,
            files_changed: Vec::new(),
            git_snapshot: None,
        },
    )
    .unwrap();

    let raw_log = fs::read_to_string(&log_path).unwrap();
    assert!(!raw_log.contains("sk-testsecretvalue1234567890"));
    assert!(!raw_log.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"));
    assert!(!raw_log.contains("hunter2"));
    assert!(raw_log.contains("[REDACTED]"));

    let events = log.replay().unwrap();
    assert_eq!(events[0].sequence, 1);
    assert_eq!(events[1].sequence, 2);
    assert!(events[0].previous_hash.is_none());
    assert_eq!(
        events[1].previous_hash.as_ref(),
        Some(&events[0].event_hash)
    );
    assert_eq!(events[0].event_hash.len(), 64);
    assert_eq!(events[1].event_hash.len(), 64);

    let EventKind::Action(Action::SendKeys { bytes }) = &events[0].kind else {
        panic!("expected send keys action");
    };
    assert!(String::from_utf8_lossy(bytes).contains("OPENAI_API_KEY=[REDACTED]"));
    assert!(log.verify_integrity().unwrap().verified);
}

#[test]
fn evidence_integrity_detects_tampering() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("tamper.jsonl");
    let log = EventLog::open(&log_path).unwrap();
    log.append_action(
        "secure",
        Action::SendKeys {
            bytes: b"printf 'safe\n'".to_vec(),
        },
        None,
    )
    .unwrap();

    let raw = fs::read_to_string(&log_path).unwrap();
    fs::write(
        &log_path,
        raw.replacen(
            "\"session_id\":\"secure\"",
            "\"session_id\":\"tampered\"",
            1,
        ),
    )
    .unwrap();
    let integrity = log.verify_integrity().unwrap();
    assert!(!integrity.verified);
    assert!(
        integrity
            .first_error
            .as_deref()
            .is_some_and(|error| error.contains("hash"))
    );
}

#[test]
fn redact_text_covers_common_agent_secret_shapes() {
    let redacted = redact_text(
        "TOKEN=abc123 PASSWORD=hunter2 sk-proj-secretsecretsecret ghp_abcdefghijklmnopqrstuvwxyz123456 AKIA1234567890ABCDEF",
    );
    assert!(!redacted.contains("hunter2"));
    assert!(!redacted.contains("sk-proj-secretsecretsecret"));
    assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"));
    assert!(!redacted.contains("AKIA1234567890ABCDEF"));
    assert!(redacted.contains("TOKEN=[REDACTED]"));
    assert!(redacted.contains("PASSWORD=[REDACTED]"));
}
